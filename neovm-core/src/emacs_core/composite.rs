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
use crate::emacs_core::value::ValueKind;

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn expect_range_args(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

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

fn expect_integerp(arg: &Value) -> Result<(), Flow> {
    match arg.kind() {
        ValueKind::Fixnum(_) => Ok(()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integerp"), *arg],
        )),
    }
}

fn expect_integer_or_marker_p(arg: &Value) -> Result<(), Flow> {
    match arg.kind() {
        ValueKind::Fixnum(_) => Ok(()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *arg],
        )),
    }
}

fn integer_value(arg: &Value) -> i64 {
    match arg.kind() {
        ValueKind::Fixnum(n) => n,
        _ => 0,
    }
}

fn expect_string_value(arg: &Value) -> Result<&crate::heap_types::LispString, Flow> {
    arg.as_lisp_string()
        .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("stringp"), *arg]))
}

// ---------------------------------------------------------------------------
// Pure builtins
// ---------------------------------------------------------------------------

/// Context-backed `(compose-region-internal START END &optional COMPONENTS MODIFICATION-FUNC)`.
///
/// Batch-compatible subset:
/// - validates START/END type (`integer-or-marker-p`)
/// - validates range against the current buffer's accessible positions
/// - returns nil on success
pub(crate) fn builtin_compose_region_internal(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("compose-region-internal", &args, 2, 4)?;
    expect_integer_or_marker_p(&args[0])?;
    expect_integer_or_marker_p(&args[1])?;

    let start = integer_value(&args[0]);
    let end = integer_value(&args[1]);
    let (buffer_handle, point_max) = if let Some(buf) = ctx.buffers.current_buffer() {
        (Value::make_buffer(buf.id), buf.point_max_char() as i64 + 1)
    } else {
        (Value::NIL, 1)
    };

    if start < 1 || end < 1 || start > end || start > point_max || end > point_max {
        return Err(signal(
            "args-out-of-range",
            vec![buffer_handle, Value::fixnum(start), Value::fixnum(end)],
        ));
    }
    Ok(Value::NIL)
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
    let len = expect_string_value(&args[0])?.schars() as i64;
    if start < 0 || end < 0 || start > end || end > len {
        return Err(signal(
            "args-out-of-range",
            vec![args[0], Value::fixnum(start), Value::fixnum(end)],
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
    if let Some(text) = args[2].as_lisp_string() {
        let len = text.schars() as i64;
        if pos < 0 || pos > len {
            return Err(signal(
                "args-out-of-range",
                vec![args[2], Value::fixnum(pos)],
            ));
        }
    } else if pos <= 0 {
        return Err(signal(
            "args-out-of-range",
            vec![Value::NIL, Value::fixnum(pos)],
        ));
    }
    Ok(Value::NIL)
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
    let from = match args[0].kind() {
        ValueKind::Fixnum(n) => n,
        _ => unreachable!("validated by expect_integerp"),
    };
    let to = match args[1].kind() {
        ValueKind::Fixnum(n) => n,
        _ => unreachable!("validated by expect_integerp"),
    };
    let text = expect_string_value(&args[3])?;
    let codes = crate::emacs_core::builtins::lisp_string_char_codes(text);
    let len = codes.len() as i64;

    if from > to || from > len || to > len {
        return Err(signal(
            "args-out-of-range",
            vec![args[3], Value::fixnum(from), Value::fixnum(to)],
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
    if from_usize >= codes.len() || to_usize > codes.len() || from_usize >= to_usize {
        return Err(signal(
            "args-out-of-range",
            vec![args[3], Value::fixnum(from), Value::fixnum(to)],
        ));
    }

    let segment = &codes[from_usize..to_usize];
    let mut encoded = vec![Value::symbol("utf-8-unix")];
    encoded.extend(segment.iter().map(|code| Value::fixnum(*code as i64)));

    let mut gstring = vec![Value::vector(encoded), Value::NIL];
    for code in segment {
        let code = *code as i64;
        gstring.push(Value::vector(vec![
            Value::fixnum(0),
            Value::fixnum(0),
            Value::fixnum(code),
            Value::fixnum(code),
            Value::fixnum(1),
            Value::fixnum(0),
            Value::fixnum(1),
            Value::fixnum(1),
            Value::fixnum(0),
            Value::NIL,
        ]));
    }
    while gstring.len() < 10 {
        gstring.push(Value::NIL);
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
    Ok(Value::NIL)
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
        return Ok(Value::NIL);
    }

    let items = list_to_vec(&args[0])
        .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("listp"), args[0]]))?;

    for item in items {
        if !item.is_cons() {
            return Err(signal(
                "error",
                vec![Value::string("Invalid composition rule in RULES argument")],
            ));
        }
    }

    Ok(args[0])
}

// ---------------------------------------------------------------------------
// Bootstrap variables
// ---------------------------------------------------------------------------

pub fn register_bootstrap_vars(obarray: &mut crate::emacs_core::symbol::Obarray) {
    // Official Emacs leaves unicode-category-table as nil at C init time;
    // it is populated later by characters.el via unicode-property-table-internal.
    obarray.set_symbol_value("unicode-category-table", Value::NIL);
    // composition-function-table must be a real char-table (composite.c:2289).
    obarray.set_symbol_value(
        "composition-function-table",
        make_char_table_value(Value::NIL, Value::NIL),
    );
    obarray.set_symbol_value("auto-composition-mode", Value::T);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "composite_test.rs"]
mod tests;
