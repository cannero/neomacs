//! Frame and startup bootstrap variables.
use crate::emacs_core::value::Value;

pub fn register_bootstrap_vars(obarray: &mut crate::emacs_core::symbol::Obarray) {
    obarray.set_symbol_value("default-frame-alist", Value::NIL);
    // GNU frame.c exposes this as a built-in variable. GUI builds default to a
    // concrete side instead of leaving scroll-bar.el to trip over an unbound var.
    obarray.set_symbol_value("default-frame-scroll-bars", Value::symbol("right"));
    obarray.set_symbol_value("initial-frame-alist", Value::NIL);
    obarray.set_symbol_value("initial-window-system", Value::NIL);
    obarray.set_symbol_value("window-system", Value::NIL);
    obarray.set_symbol_value("handle-args-function", Value::symbol("command-line-1"));
    obarray.set_symbol_value("handle-args-function-alist", Value::NIL);
    obarray.set_symbol_value("inhibit-x-resources", Value::NIL);
    obarray.set_symbol_value("resize-mini-windows", Value::symbol("grow-only"));
    obarray.set_symbol_value("frame-title-format", Value::string("%b"));
    obarray.set_symbol_value("icon-title-format", Value::NIL);
    obarray.set_symbol_value("frame-resize-pixelwise", Value::NIL);
    obarray.set_symbol_value("focus-follows-mouse", Value::NIL);
    obarray.set_symbol_value("frame-inhibit-implied-resize", Value::NIL);
    obarray.set_symbol_value("terminal-frame", Value::NIL);
    obarray.set_symbol_value("frameset-filter-alist", Value::NIL);
    obarray.set_symbol_value("frameset-session-filter-alist", Value::NIL);
}
