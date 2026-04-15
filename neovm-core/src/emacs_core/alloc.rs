//! Bootstrap-facing subset of GNU Emacs's `alloc.c`.
//!
//! GNU exposes several GC / memory-management variables from C before Lisp
//! startup runs.  Keep those defaults here so Lisp like `jit-lock.el` can rely
//! on the same low-level variables during runtime and bootstrap.

use crate::emacs_core::symbol::Obarray;
use crate::emacs_core::value::{Value, list_to_vec, next_float_id};

/// Register bootstrap variables owned by the allocation / GC subsystem.
pub fn register_bootstrap_vars(obarray: &mut Obarray) {
    obarray.set_symbol_value("gc-cons-threshold", Value::fixnum(800_000));
    obarray.set_symbol_value("gc-cons-percentage", Value::make_float(0.1));
    obarray.set_symbol_value("garbage-collection-messages", Value::NIL);
    obarray.set_symbol_value("post-gc-hook", Value::NIL);
    obarray.set_symbol_value(
        "memory-signal-data",
        Value::list(vec![
            Value::symbol("error"),
            Value::string(
                "Memory exhausted--use M-x save-some-buffers then exit and restart Emacs",
            ),
        ]),
    );
    obarray.set_symbol_value("memory-full", Value::NIL);
    obarray.set_symbol_value("gc-elapsed", Value::make_float(0.0));
    obarray.set_symbol_value("gcs-done", Value::fixnum(0));
}

#[cfg(test)]
#[path = "alloc_test.rs"]
mod tests;
