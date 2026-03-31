//! Code Conversion Language (CCL) compatibility runtime.
//!
//! CCL is a low-level bytecode language for efficient character/text conversion.
//! This implementation currently provides partial CCL behavior:
//! - `ccl-program-p` — basic predicate for vector-shaped CCL program headers
//! - `register-ccl-program` — stores named CCL programs and returns stable ids
//! - `register-code-conversion-map` — stores named conversion maps and returns stable ids
//! - `ccl-execute` / `ccl-execute-on-string` — validates shape and designators
//!   and mirrors current oracle error payloads for unsupported execution paths.

use super::error::{EvalResult, Flow, signal};
use super::value::*;
use std::cell::RefCell;
use std::collections::HashMap;

fn is_integer(value: &Value) -> bool {
    value.is_fixnum()
}

fn is_valid_ccl_program(program: &Value) -> bool {
    if !program.is_vector() {
        return false;
    };

    let program = with_heap(|h| h.get_vector(*program).clone());
    if program.len() < 3 {
        return false;
    }

    let [first, second, third] = [&program[0], &program[1], &program[2]];

    let first = first.as_int();
    if first.is_none() || first.is_some_and(|n| n < 0) {
        return false;
    }

    let second = second.as_int();
    if second.is_none() || second.is_some_and(|n| !(0..=3).contains(&n)) {
        return false;
    }

    is_integer(third)
}

#[derive(Default)]
struct CclRegistry {
    programs: HashMap<String, (i64, Value)>,
    code_conversion_maps: HashMap<String, (i64, Value)>,
    next_program_id: i64,
    next_code_conversion_map_id: i64,
}

impl CclRegistry {
    fn with_defaults() -> Self {
        Self {
            programs: HashMap::new(),
            code_conversion_maps: HashMap::new(),
            next_program_id: 1,
            next_code_conversion_map_id: 0,
        }
    }

    fn register_program(&mut self, name: &str, program: Value) -> i64 {
        if let Some((id, slot)) = self.programs.get_mut(name) {
            *slot = program;
            return *id;
        }
        let id = self.next_program_id;
        self.next_program_id = self.next_program_id.saturating_add(1);
        self.programs.insert(name.to_string(), (id, program));
        id
    }

    fn lookup_program(&self, name: &str) -> Option<Value> {
        self.programs.get(name).map(|(_, program)| *program)
    }

    fn register_code_conversion_map(&mut self, name: &str, value: Value) -> i64 {
        if let Some((id, slot)) = self.code_conversion_maps.get_mut(name) {
            *slot = value;
            return *id;
        }
        let id = self.next_code_conversion_map_id;
        self.next_code_conversion_map_id = self.next_code_conversion_map_id.saturating_add(1);
        self.code_conversion_maps
            .insert(name.to_string(), (id, value));
        id
    }
}

thread_local! {
    static CCL_REGISTRY: RefCell<CclRegistry> = RefCell::new(CclRegistry::with_defaults());
}

fn with_ccl_registry<R>(f: impl FnOnce(&CclRegistry) -> R) -> R {
    CCL_REGISTRY.with(|r| f(&r.borrow()))
}

fn with_ccl_registry_mut<R>(f: impl FnOnce(&mut CclRegistry) -> R) -> R {
    CCL_REGISTRY.with(|r| f(&mut r.borrow_mut()))
}

/// Reset the CCL registry to its initial state.
pub(crate) fn reset_ccl_registry() {
    CCL_REGISTRY.with(|r| *r.borrow_mut() = CclRegistry::with_defaults());
}

/// Collect GC roots from the CCL registry.
pub(crate) fn collect_ccl_gc_roots(roots: &mut Vec<Value>) {
    CCL_REGISTRY.with(|r| {
        let reg = r.borrow();
        for (_, v) in reg.programs.values() {
            roots.push(*v);
        }
        for (_, v) in reg.code_conversion_maps.values() {
            roots.push(*v);
        }
    });
}

pub(crate) fn unregister_registered_ccl_program(name: &str) {
    with_ccl_registry_mut(|registry| {
        let _ = registry.programs.remove(name);
    });
}

pub(crate) fn is_registered_ccl_program(name: &str) -> bool {
    with_ccl_registry(|registry| registry.programs.contains_key(name))
}

enum CclProgramDesignatorKind {
    Inline,
    RegisteredSymbol,
}

fn resolve_ccl_program_designator(value: &Value) -> Option<(Value, CclProgramDesignatorKind)> {
    if value.is_vector() {
        return Some((*value, CclProgramDesignatorKind::Inline));
    }
    let name = value.as_symbol_name()?;
    with_ccl_registry(|registry| {
        registry
            .lookup_program(name)
            .map(|program| (program, CclProgramDesignatorKind::RegisteredSymbol))
    })
}

fn ccl_program_code_index_message(
    program: &Value,
    designator_kind: CclProgramDesignatorKind,
) -> String {
    let base_len = match program.kind() {
        ValueKind::Veclike(VecLikeType::Vector) => with_heap(|h| h.vector_len(*handle) as i64),
        _ => 0,
    };
    let index = match designator_kind {
        CclProgramDesignatorKind::Inline => base_len.saturating_add(1),
        CclProgramDesignatorKind::RegisteredSymbol => base_len.saturating_add(2),
    };
    format!("Error in CCL program at {index}th code")
}

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_max_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    if args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Pure builtins
// ---------------------------------------------------------------------------

/// (ccl-program-p OBJECT) -> nil
/// This accepts program objects that match the minimum CCL header shape used by Emacs.
pub(crate) fn builtin_ccl_program_p_impl(args: Vec<Value>) -> EvalResult {
    expect_args("ccl-program-p", &args, 1)?;
    let is_program = resolve_ccl_program_designator(&args[0])
        .map(|(program, _)| is_valid_ccl_program(&program))
        .unwrap_or(false);
    Ok(Value::bool_val(is_program))
}

/// (ccl-execute CCL-PROGRAM STATUS) -> nil
/// Stub: doesn't actually execute CCL bytecode.
pub(crate) fn builtin_ccl_execute_impl(args: Vec<Value>) -> EvalResult {
    expect_args("ccl-execute", &args, 2)?;
    if !args[1].is_vector() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("vectorp"), args[1]],
        ));
    }

    let status_len = match args[1].kind() {
        ValueKind::Veclike(VecLikeType::Vector) => args[1].as_vector_data().unwrap().len(),
        _ => unreachable!("status already validated as vector"),
    };
    if status_len != 8 {
        return Err(signal(
            "error",
            vec![Value::string("Length of vector REGISTERS is not 8")],
        ));
    }

    let Some((program, designator_kind)) = resolve_ccl_program_designator(&args[0]) else {
        return Err(signal("error", vec![Value::string("Invalid CCL program")]));
    };
    if !is_valid_ccl_program(&program) {
        return Err(signal("error", vec![Value::string("Invalid CCL program")]));
    }

    let message = ccl_program_code_index_message(&program, designator_kind);
    Err(signal("error", vec![Value::string(message)]))
}

/// (ccl-execute-on-string CCL-PROGRAM STATUS STRING &optional CONTINUE UNIBYTE-P) -> STRING
/// Stub: returns STRING unchanged without processing.
pub(crate) fn builtin_ccl_execute_on_string_impl(args: Vec<Value>) -> EvalResult {
    expect_min_args("ccl-execute-on-string", &args, 3)?;
    expect_max_args("ccl-execute-on-string", &args, 5)?;
    if !args[1].is_vector() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("vectorp"), args[1]],
        ));
    }
    let status_len = match args[1].kind() {
        ValueKind::Veclike(VecLikeType::Vector) => args[1].as_vector_data().unwrap().len(),
        _ => unreachable!("status already validated as vector"),
    };
    if status_len != 9 {
        return Err(signal(
            "error",
            vec![Value::string("Length of vector STATUS is not 9")],
        ));
    }

    let Some((program, designator_kind)) = resolve_ccl_program_designator(&args[0]) else {
        return Err(signal("error", vec![Value::string("Invalid CCL program")]));
    };
    if !is_valid_ccl_program(&program) {
        return Err(signal("error", vec![Value::string("Invalid CCL program")]));
    }

    // Arguments:
    //   0: CCL-PROGRAM (we don't use)
    //   1: STATUS vector (we don't use)
    //   2: STRING (return this unchanged)
    //   3: CONTINUE (optional, we don't use)
    //   4: UNIBYTE-P (optional, we don't use)

    match args[2].kind() {
        ValueKind::String => {
            let message = ccl_program_code_index_message(&program, designator_kind);
            Err(signal("error", vec![Value::string(message)]))
        }
        other => {
            // Type error: STRING must be a string or nil
            Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), args[2]],
            ))
        }
    }
}

/// (register-ccl-program NAME CCL-PROG) -> nil
/// Stub: accepts and discards the CCL program registration.
pub(crate) fn builtin_register_ccl_program_impl(args: Vec<Value>) -> EvalResult {
    expect_args("register-ccl-program", &args, 2)?;
    if !args[0].is_symbol() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    }
    let program = if args[1].is_nil() {
        // Oracle accepts nil and behaves like a minimal valid registered program.
        Value::vector(vec![Value::fixnum(0), Value::fixnum(0), Value::fixnum(0)])
    } else {
        if !args[1].is_vector() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("vectorp"), args[1]],
            ));
        }
        args[1]
    };

    if !is_valid_ccl_program(&program) {
        return Err(signal("error", vec![Value::string("Error in CCL program")]));
    }

    let name = args[0]
        .as_symbol_name()
        .expect("symbol already validated by is_symbol");
    let program_id = with_ccl_registry_mut(|registry| registry.register_program(name, program));
    Ok(Value::fixnum(program_id))
}

/// (register-code-conversion-map SYMBOL MAP) -> nil
/// Stub: accepts and discards the code conversion map.
pub(crate) fn builtin_register_code_conversion_map_impl(args: Vec<Value>) -> EvalResult {
    expect_args("register-code-conversion-map", &args, 2)?;
    if !args[0].is_symbol() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    }
    if !args[1].is_vector() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("vectorp"), args[1]],
        ));
    }

    let name = args[0]
        .as_symbol_name()
        .expect("symbol already validated by is_symbol");
    let map_id =
        with_ccl_registry_mut(|registry| registry.register_code_conversion_map(name, args[1]));
    Ok(Value::fixnum(map_id))
}
#[cfg(test)]
#[path = "ccl_test.rs"]
mod tests;
