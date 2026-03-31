//! DBus compatibility builtins.
//!
//! NeoVM does not include DBus transport, but a subset of DBus primitives are
//! exposed for startup/runtime compatibility with expected arity and basic
//! error contracts.

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::value::{Value, ValueKind};

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

fn expect_range_args(
    name: &str,
    args: &[Value],
    min: usize,
    max: Option<usize>,
) -> Result<(), Flow> {
    let out_of_range = match max {
        Some(max) => args.len() < min || args.len() > max,
        None => args.len() < min,
    };
    if out_of_range {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_symbolp(value: &Value) -> Result<String, Flow> {
    match value.kind() {
        ValueKind::Symbol(id) | ValueKind::Keyword(id) => Ok(resolve_sym(id).to_owned()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *other],
        )),
    }
}

fn expect_wholenump(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) if n >= 0 => Ok(n),
        ValueKind::Char(c) => Ok(c as i64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("wholenump"), *other],
        )),
    }
}

fn dbus_error(msg: &str, details: Value) -> Flow {
    signal("dbus-error", vec![Value::string(msg), details])
}

fn recognized_bus_name(name: &str) -> bool {
    matches!(name, ":system" | ":session")
}

/// `(dbus--init-bus BUS &optional PRIVATE)` -- initialize BUS and return
/// a numeric handle.
pub(crate) fn builtin_dbus_init_bus(args: Vec<Value>) -> EvalResult {
    expect_range_args("dbus--init-bus", &args, 1, Some(2))?;
    let bus = expect_symbolp(&args[0])?;
    if recognized_bus_name(&bus) {
        Ok(Value::fixnum(2))
    } else {
        Err(dbus_error("Wrong bus name", Value::symbol(bus)))
    }
}

/// `(dbus-get-unique-name BUS)` -- resolve unique name for BUS.
pub(crate) fn builtin_dbus_get_unique_name(args: Vec<Value>) -> EvalResult {
    expect_args("dbus-get-unique-name", &args, 1)?;
    let bus = expect_symbolp(&args[0])?;
    if recognized_bus_name(&bus) {
        Err(dbus_error("No connection to bus", Value::symbol(bus)))
    } else {
        Err(dbus_error("Wrong bus name", Value::symbol(bus)))
    }
}

/// `(dbus-message-internal BUS-ID DESTINATION ... )` -- DBus call helper.
pub(crate) fn builtin_dbus_message_internal(args: Vec<Value>) -> EvalResult {
    expect_range_args("dbus-message-internal", &args, 4, None)?;
    let _bus_id = expect_wholenump(&args[0])?;

    match args[1].kind() {
        ValueKind::Symbol(_) | ValueKind::Keyword(_) => Ok(Value::NIL),
        ValueKind::String => {
            let dest = crate::emacs_core::value::with_heap(|h| h.get_string(*id).to_owned());
            if !dest.contains(':') {
                Err(signal(
                    "dbus-error",
                    vec![Value::string("Address does not contain a colon")],
                ))
            } else if args.len() == 4 {
                Err(signal(
                    "wrong-number-of-arguments",
                    vec![Value::symbol("dbus-message-internal"), ValueKind::Fixnum(4)],
                ))
            } else {
                Ok(ValueKind::Nil)
            }
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *other],
        )),
    }
}

#[cfg(test)]
#[path = "dbus_test.rs"]
mod tests;
