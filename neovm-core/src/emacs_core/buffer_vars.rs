//! Buffer-related bootstrap variables.
use crate::emacs_core::value::Value;

pub fn register_bootstrap_vars(obarray: &mut crate::emacs_core::symbol::Obarray) {
    obarray.set_symbol_value("kill-buffer-query-functions", Value::NIL);
    obarray.set_symbol_value("kill-buffer-hook", Value::NIL);
    obarray.set_symbol_value("buffer-list-update-hook", Value::NIL);
    obarray.set_symbol_value("change-major-mode-hook", Value::NIL);
    obarray.set_symbol_value("after-change-major-mode-hook", Value::NIL);
    obarray.set_symbol_value("first-change-hook", Value::NIL);
    obarray.set_symbol_value("before-change-functions", Value::NIL);
    obarray.set_symbol_value("after-change-functions", Value::NIL);
    obarray.set_symbol_value("inhibit-modification-hooks", Value::NIL);
    obarray.set_symbol_value("buffer-access-fontify-functions", Value::NIL);
    obarray.set_symbol_value("buffer-access-fontified-property", Value::NIL);
    obarray.set_symbol_value("buffer-file-coding-system", Value::NIL);
    obarray.set_symbol_value("buffer-file-format", Value::NIL);
    obarray.set_symbol_value("buffer-saved-size", Value::fixnum(0));
    obarray.set_symbol_value(
        "buffer-auto-save-file-format",
        Value::list(vec![Value::symbol("t")]),
    );
    obarray.set_symbol_value("buffer-stale-function", Value::NIL);
    obarray.set_symbol_value("buffer-undo-list", Value::NIL);
    obarray.set_symbol_value("buffer-display-table", Value::NIL);
    obarray.set_symbol_value("enable-multibyte-characters", Value::T);
    obarray.set_symbol_value("default-enable-multibyte-characters", Value::T);
    obarray.set_symbol_value("find-file-hook", Value::NIL);
    obarray.set_symbol_value("find-file-not-found-functions", Value::NIL);
    obarray.set_symbol_value("major-mode", Value::symbol("fundamental-mode"));
    obarray.set_symbol_value("mode-name", Value::string("Fundamental"));
    obarray.set_symbol_value("local-abbrev-table", Value::NIL);
    obarray.set_symbol_value("fill-column", Value::fixnum(70));
    obarray.set_symbol_value("left-margin", Value::fixnum(0));
    // tab-width is set by init_indent_vars() with special=true
    obarray.set_symbol_value("ctl-arrow", Value::T);
    obarray.set_symbol_value("truncate-lines", Value::NIL);
    obarray.set_symbol_value("word-wrap", Value::NIL);
    obarray.set_symbol_value("word-wrap-by-category", Value::NIL);
    obarray.set_symbol_value("selective-display", Value::NIL);
    obarray.set_symbol_value("selective-display-ellipses", Value::T);
    obarray.set_symbol_value("indicate-empty-lines", Value::NIL);
    obarray.set_symbol_value("indicate-buffer-boundaries", Value::NIL);
    obarray.set_symbol_value("fringe-indicator-alist", Value::NIL);
    obarray.set_symbol_value("fringe-cursor-alist", Value::NIL);
    obarray.set_symbol_value("scroll-up-aggressively", Value::NIL);
    obarray.set_symbol_value("scroll-down-aggressively", Value::NIL);
    obarray.set_symbol_value("auto-fill-function", Value::NIL);
    obarray.set_symbol_value("buffer-display-count", Value::fixnum(0));
    obarray.set_symbol_value("buffer-display-time", Value::NIL);
}
