//! Composition builtins (complex script rendering).
//!
//! In real Emacs, the composition system handles combining characters,
//! ligatures, and complex script shaping.  Most of this work is done by the
//! display engine (and in Neomacs, by the Rust layout engine), so here we
//! provide stubs that satisfy Elisp code which queries or manipulates
//! compositions at the Lisp level.

use super::chartable::make_char_table_value;
use super::error::{EvalResult, Flow, signal};
use super::value::*;

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn expect_range_args(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_max_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    if args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_integerp(arg: &Value) -> Result<(), Flow> {
    match arg {
        Value::Int(_) | Value::Char(_) => Ok(()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *other],
        )),
    }
}

fn expect_integer_or_marker_p(arg: &Value) -> Result<(), Flow> {
    match arg {
        Value::Int(_) | Value::Char(_) => Ok(()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *other],
        )),
    }
}

fn integer_value(arg: &Value) -> i64 {
    match arg {
        Value::Int(n) => *n,
        Value::Char(c) => *c as i64,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Pure builtins
// ---------------------------------------------------------------------------

/// `(compose-region-internal START END &optional COMPONENTS MODIFICATION-FUNC)`
///
/// Compose text in the current buffer between START and END.
/// COMPONENTS, if given, is a vector or string describing the composition.
/// MODIFICATION-FUNC, if non-nil, is called when the composition is modified.
///
/// Stub: composition is handled by the display/layout engine; return nil.
pub(crate) fn builtin_compose_region_internal(args: Vec<Value>) -> EvalResult {
    expect_range_args("compose-region-internal", &args, 2, 4)?;
    expect_integer_or_marker_p(&args[0])?;
    expect_integer_or_marker_p(&args[1])?;
    Ok(Value::Nil)
}

/// Evaluator-backed `(compose-region-internal START END &optional COMPONENTS MODIFICATION-FUNC)`.
///
/// Batch-compatible subset:
/// - validates START/END type (`integer-or-marker-p`)
/// - validates range against the current buffer's accessible positions
/// - returns nil on success
pub(crate) fn builtin_compose_region_internal_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("compose-region-internal", &args, 2, 4)?;
    expect_integer_or_marker_p(&args[0])?;
    expect_integer_or_marker_p(&args[1])?;

    let start = integer_value(&args[0]);
    let end = integer_value(&args[1]);
    let (buffer_handle, point_max) = if let Some(buf) = eval.buffers.current_buffer() {
        (
            Value::Buffer(buf.id),
            buf.buffer_string().chars().count() as i64 + 1,
        )
    } else {
        (Value::Nil, 1)
    };

    if start < 1 || end < 1 || start > end || start > point_max || end > point_max {
        return Err(signal(
            "args-out-of-range",
            vec![buffer_handle, Value::Int(start), Value::Int(end)],
        ));
    }
    Ok(Value::Nil)
}

/// `(compose-string-internal STRING START END &optional COMPONENTS MODIFICATION-FUNC)`
///
/// Compose text in STRING between indices START and END.
/// Returns STRING (possibly with composition properties attached).
///
/// Stub: return STRING unchanged.
pub(crate) fn builtin_compose_string_internal(args: Vec<Value>) -> EvalResult {
    expect_range_args("compose-string-internal", &args, 3, 5)?;
    if !args[0].is_string() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        ));
    }
    expect_integerp(&args[1])?;
    expect_integerp(&args[2])?;
    let start = integer_value(&args[1]);
    let end = integer_value(&args[2]);
    let len = args[0].as_str().expect("validated string").chars().count() as i64;
    if start < 0 || end < 0 || start > end || end > len {
        return Err(signal(
            "args-out-of-range",
            vec![args[0], Value::Int(start), Value::Int(end)],
        ));
    }
    // Return the string argument unchanged.
    Ok(args[0])
}

/// `(find-composition-internal POS LIMIT STRING DETAIL-P)`
///
/// Find a composition at or near position POS.
/// Returns a list describing the composition, or nil if none found.
///
/// Stub: no compositions exist, always return nil.
pub(crate) fn builtin_find_composition_internal(args: Vec<Value>) -> EvalResult {
    expect_args("find-composition-internal", &args, 4)?;
    expect_integer_or_marker_p(&args[0])?;
    if !args[1].is_nil() {
        expect_integer_or_marker_p(&args[1])?;
    }
    if !args[2].is_nil() && !args[2].is_string() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[2]],
        ));
    }
    let pos = integer_value(&args[0]);
    if let Some(text) = args[2].as_str() {
        let len = text.chars().count() as i64;
        if pos < 0 || pos > len {
            return Err(signal("args-out-of-range", vec![args[2], Value::Int(pos)]));
        }
    } else if pos <= 0 {
        return Err(signal(
            "args-out-of-range",
            vec![Value::Nil, Value::Int(pos)],
        ));
    }
    Ok(Value::Nil)
}

/// `(composition-get-gstring FROM TO FONT-OBJECT STRING)`
///
/// Return a gstring (grapheme cluster string) for composing characters
/// between FROM and TO with FONT-OBJECT in STRING.
///
/// Stub: return nil (let the display engine handle shaping).
pub(crate) fn builtin_composition_get_gstring(args: Vec<Value>) -> EvalResult {
    expect_args("composition-get-gstring", &args, 4)?;
    expect_integerp(&args[0])?;
    expect_integerp(&args[1])?;
    if !args[3].is_string() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[3]],
        ));
    }
    let from = match &args[0] {
        Value::Int(n) => *n,
        Value::Char(c) => *c as i64,
        _ => unreachable!("validated by expect_integerp"),
    };
    let to = match &args[1] {
        Value::Int(n) => *n,
        Value::Char(c) => *c as i64,
        _ => unreachable!("validated by expect_integerp"),
    };
    let text = args[3].as_str().expect("validated string");
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len() as i64;

    if from > to || from > len || to > len {
        return Err(signal(
            "args-out-of-range",
            vec![Value::string(text), Value::Int(from), Value::Int(to)],
        ));
    }
    if from < 0 || from == to {
        return Err(signal(
            "error",
            vec![Value::string("Attempt to shape zero-length text")],
        ));
    }

    let from_usize = from as usize;
    let to_usize = to as usize;
    if from_usize >= chars.len() || to_usize > chars.len() || from_usize >= to_usize {
        return Err(signal(
            "args-out-of-range",
            vec![Value::string(text), Value::Int(from), Value::Int(to)],
        ));
    }

    let segment = &chars[from_usize..to_usize];
    let mut encoded = vec![Value::symbol("utf-8-unix")];
    encoded.extend(segment.iter().map(|c| Value::Int(*c as i64)));

    let mut gstring = vec![Value::vector(encoded), Value::Nil];
    for ch in segment {
        let code = *ch as i64;
        gstring.push(Value::vector(vec![
            Value::Int(0),
            Value::Int(0),
            Value::Int(code),
            Value::Int(code),
            Value::Int(1),
            Value::Int(0),
            Value::Int(1),
            Value::Int(1),
            Value::Int(0),
            Value::Nil,
        ]));
    }
    while gstring.len() < 10 {
        gstring.push(Value::Nil);
    }

    Ok(Value::vector(gstring))
}

/// `(clear-composition-cache)`
///
/// Clear the internal composition cache.
///
/// Stub: no cache to clear, return nil.
pub(crate) fn builtin_clear_composition_cache(args: Vec<Value>) -> EvalResult {
    expect_max_args("clear-composition-cache", &args, 0)?;
    Ok(Value::Nil)
}

/// `(composition-sort-rules RULES)`
///
/// Sort composition rules by priority.
///
/// Batch-compatible subset:
/// - nil RULES => nil
/// - non-list RULES => `(wrong-type-argument listp RULES)`
/// - list entries that are not composition rules => generic invalid-rule error
/// - otherwise return RULES unchanged
pub(crate) fn builtin_composition_sort_rules(args: Vec<Value>) -> EvalResult {
    expect_args("composition-sort-rules", &args, 1)?;
    if args[0].is_nil() {
        return Ok(Value::Nil);
    }

    let items = list_to_vec(&args[0])
        .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("listp"), args[0]]))?;

    for item in items {
        if !matches!(item, Value::Cons(_)) {
            return Err(signal(
                "error",
                vec![Value::string("Invalid composition rule in RULES argument")],
            ));
        }
    }

    Ok(args[0])
}

/// `(auto-composition-mode &optional ARG)`
///
/// Toggle auto-composition mode.  In real Emacs this is a minor mode that
/// controls whether automatic character composition is performed.
///
/// Batch-compatible behavior: return `t`.
pub(crate) fn builtin_auto_composition_mode(args: Vec<Value>) -> EvalResult {
    expect_max_args("auto-composition-mode", &args, 1)?;
    Ok(Value::True)
}

// ---------------------------------------------------------------------------
// Bootstrap variables
// ---------------------------------------------------------------------------

pub fn register_bootstrap_vars(obarray: &mut crate::emacs_core::symbol::Obarray) {
    // Official Emacs leaves unicode-category-table as nil at C init time;
    // it is populated later by characters.el via unicode-property-table-internal.
    obarray.set_symbol_value("unicode-category-table", Value::Nil);
    // composition-function-table must be a real char-table (composite.c:2289).
    obarray.set_symbol_value(
        "composition-function-table",
        make_char_table_value(Value::Nil, Value::Nil),
    );
    obarray.set_symbol_value("auto-composition-mode", Value::True);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "composite_test.rs"]
mod tests;
