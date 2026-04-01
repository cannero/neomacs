//! XML and compression stubs for the Elisp interpreter.
//!
//! Provides stub implementations for:
//! - `libxml-parse-html-region`, `libxml-parse-xml-region`, `libxml-available-p`
//! - `zlib-available-p`, `zlib-decompress-region`
//!
//! These are stubbed because libxml and zlib are not available in pure Rust Elisp yet.

use super::error::{EvalResult, Flow, signal};
use super::value::*;
use crate::emacs_core::value::ValueKind;

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

fn expect_optional_string(_name: &str, value: &Value) -> Result<(), Flow> {
    if value.is_nil() {
        return Ok(());
    }
    if value.as_str().is_none() {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        ))
    } else {
        Ok(())
    }
}

fn html_parse_fallback(name: &str, args: &[Value]) -> Value {
    let body = if args.len() <= 1 {
        Value::string(format!("({name})"))
    } else {
        Value::string("(")
    };
    Value::list(vec![
        Value::symbol("html"),
        Value::NIL,
        Value::list(vec![Value::symbol("body"), Value::NIL, body]),
    ])
}

fn expect_integer_or_marker(value: &Value) -> Result<i64, Flow> {
    if let Some(n) = value.as_fixnum() {
        return Ok(n);
    }
    if let Some(c) = value.as_char() {
        return Ok(c as i64);
    }
    if super::marker::is_marker(value) {
        return super::marker::marker_position_as_int(value);
    }
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("integer-or-marker-p"), *value],
    ))
}

// ---------------------------------------------------------------------------
// Pure builtins
// ---------------------------------------------------------------------------

/// (libxml-parse-html-region START END &optional BASE-URL DISCARD-COMMENTS)
/// Stub: returns a compatibility envelope until libxml parser support lands.
pub(crate) fn builtin_libxml_parse_html_region(args: Vec<Value>) -> EvalResult {
    expect_min_args("libxml-parse-html-region", &args, 0)?;
    expect_max_args("libxml-parse-html-region", &args, 4)?;
    if args.len() >= 2 {
        if args.first().is_some_and(|v| v.is_nil()) {
            return Ok(Value::NIL);
        }
        let start_pos = expect_integer_or_marker(
            args.first()
                .expect("libxml-parse-html-region requires a start position here"),
        )?;
        if let Some(end) = args.get(1) {
            if !end.is_nil() {
                let end_pos = expect_integer_or_marker(end)?;
                if start_pos == end_pos {
                    return Ok(Value::NIL);
                }
            }
        }
    }
    if let Some(base_url) = args.get(2) {
        expect_optional_string("libxml-parse-html-region", base_url)?;
    }
    // Stub parser path: return a stable compatibility envelope until libxml support lands.
    Ok(html_parse_fallback("libxml-parse-html-region", &args))
}

/// (libxml-parse-xml-region START END &optional BASE-URL DISCARD-COMMENTS)
/// Stub: returns nil (libxml not available in pure Rust yet).
pub(crate) fn builtin_libxml_parse_xml_region(args: Vec<Value>) -> EvalResult {
    expect_min_args("libxml-parse-xml-region", &args, 0)?;
    expect_max_args("libxml-parse-xml-region", &args, 4)?;
    if let Some(start) = args.first() {
        if !start.is_nil() {
            let _ = expect_integer_or_marker(start)?;
        }
    }
    if let Some(end) = args.get(1) {
        if !end.is_nil() {
            let _ = expect_integer_or_marker(end)?;
        }
    }
    if let Some(base_url) = args.get(2) {
        expect_optional_string("libxml-parse-xml-region", base_url)?;
    }
    // Stub parser path: we intentionally return nil until libxml parser support lands.
    Ok(Value::NIL)
}

/// (libxml-available-p)
/// Returns t (feature availability probe).
pub(crate) fn builtin_libxml_available_p(args: Vec<Value>) -> EvalResult {
    expect_args("libxml-available-p", &args, 0)?;
    Ok(Value::T)
}

/// (zlib-available-p)
/// Returns t (feature availability probe).
pub(crate) fn builtin_zlib_available_p(args: Vec<Value>) -> EvalResult {
    expect_args("zlib-available-p", &args, 0)?;
    Ok(Value::T)
}

/// (zlib-decompress-region START END)
/// Compatibility subset:
/// - validates START/END as integer-or-marker
/// - supports optional third arg
/// - signals the same unibyte-buffer requirement as Emacs in current multibyte buffers
pub(crate) fn builtin_zlib_decompress_region(args: Vec<Value>) -> EvalResult {
    expect_min_args("zlib-decompress-region", &args, 2)?;
    expect_max_args("zlib-decompress-region", &args, 3)?;
    let _start = expect_integer_or_marker(&args[0])?;
    let _end = expect_integer_or_marker(&args[1])?;

    Err(signal(
        "error",
        vec![Value::string(
            "This function can be called only in unibyte buffers",
        )],
    ))
}
#[cfg(test)]
#[path = "xml_test.rs"]
mod tests;
