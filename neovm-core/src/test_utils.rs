//! Common test utilities for neovm-core.
//!
//! Provides shared helpers used across all test modules.

use crate::emacs_core::error::map_flow;
use crate::emacs_core::load::{
    apply_ldefs_boot_autoloads_for_names, bootstrap_load_path_entries,
    create_runtime_startup_evaluator_cached, find_file_in_load_path, get_load_path, load_file,
};
use crate::emacs_core::value::Value;
use crate::emacs_core::{Context, format_eval_result};
use std::path::PathBuf;

/// Initialize the tracing subscriber for test output.
///
/// Thin wrapper around [`crate::logging::init_for_tests`] kept for the
/// many existing call sites in this crate's test files. Tests never
/// write to a log file regardless of `NEOMACS_LOG_TO_FILE` — output is
/// always routed through the test harness's writer.
///
/// # Usage
/// Call at the start of any test that needs tracing:
/// ```rust,ignore
/// #[test]
/// fn my_test() {
///     crate::test_utils::init_test_tracing();
///     // ... test code ...
/// }
/// ```
pub fn init_test_tracing() {
    crate::logging::init_for_tests();
}

/// Load a small GNU Lisp runtime that is sufficient for tests that need
/// `byte-run`, backquote expansion, and the basic `subr.el` support layer,
/// without paying for full `loadup.el` startup.
pub fn load_minimal_gnu_backquote_runtime(eval: &mut Context) {
    eval.set_lexical_binding(true);
    eval.set_variable(
        "load-path",
        Value::list(vec![
            Value::string(concat!(env!("CARGO_MANIFEST_DIR"), "/../lisp/emacs-lisp")),
            Value::string(concat!(env!("CARGO_MANIFEST_DIR"), "/../lisp")),
        ]),
    );
    let load_path = get_load_path(&eval.obarray());
    for name in &[
        "emacs-lisp/debug-early",
        "emacs-lisp/byte-run",
        "emacs-lisp/backquote",
        "subr",
    ] {
        let path = find_file_in_load_path(name, &load_path)
            .unwrap_or_else(|| panic!("cannot find {name}"));
        load_file(eval, &path).unwrap_or_else(|err| panic!("load {name}: {err:?}"));
    }
}

/// Load a small GNU Lisp runtime that is sufficient for `help.el`
/// semantics such as `substitute-command-keys`, without paying for
/// full `loadup.el` startup.
pub fn load_minimal_gnu_help_runtime(eval: &mut Context) {
    load_minimal_gnu_backquote_runtime(eval);
    let load_path = get_load_path(&eval.obarray());
    for name in &[
        "keymap",
        "widget",
        "custom",
        "cus-face",
        "faces",
        "emacs-lisp/macroexp",
        "emacs-lisp/pcase",
        "emacs-lisp/easy-mmode",
        "help-macro",
    ] {
        let path = find_file_in_load_path(name, &load_path)
            .unwrap_or_else(|| panic!("cannot find {name}"));
        load_file(eval, &path).unwrap_or_else(|err| panic!("load {name}: {err:?}"));
    }

    let help_path = find_file_in_load_path("help", &load_path).expect("cannot find help");
    let help_source =
        std::fs::read_to_string(&help_path).unwrap_or_else(|err| panic!("read help.el: {err}"));
    let help_forms = crate::emacs_core::value_reader::read_all(&help_source)
        .expect("parse help.el");
    let mut found_substitute_command_keys = false;
    for form in help_forms {
        let is_target = is_named_defun_value(&form, "substitute-command-keys");
        eval.eval_sub(form)
            .map_err(map_flow)
            .unwrap_or_else(|err| panic!("eval help.el prefix: {err:?}"));
        if is_target {
            found_substitute_command_keys = true;
            break;
        }
    }
    assert!(
        found_substitute_command_keys,
        "help.el should define substitute-command-keys"
    );
}

fn is_named_defun_value(form: &Value, name: &str) -> bool {
    if !form.is_cons() {
        return false;
    }
    let car = form.cons_car();
    if !car.is_symbol_named("defun") {
        return false;
    }
    let cdr = form.cons_cdr();
    if !cdr.is_cons() {
        return false;
    }
    cdr.cons_car().is_symbol_named(name)
}

/// Create a bare evaluator with GNU `ldefs-boot.el` autoload cells restored
/// for the named symbols and a bootstrap-compatible `load-path`.
pub fn eval_with_ldefs_boot_autoloads(names: &[&str]) -> Context {
    let mut eval = Context::new();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let lisp_dir = project_root.join("lisp");
    eval.set_variable(
        "load-path",
        Value::list(bootstrap_load_path_entries(&lisp_dir)),
    );
    for name in names {
        eval.obarray_mut().fmakunbound(name);
    }
    apply_ldefs_boot_autoloads_for_names(&mut eval, names).expect("ldefs-boot autoload restore");
    eval
}

/// Create a cached runtime-startup evaluator for tests that need the full
/// GNU bootstrap surface.
pub fn runtime_startup_context() -> Context {
    create_runtime_startup_evaluator_cached().expect("bootstrap")
}

/// Evaluate FORMS in a cached runtime-startup evaluator and return formatted
/// results, matching the common bootstrap test pattern.
pub fn runtime_startup_eval_all(src: &str) -> Vec<String> {
    let mut eval = runtime_startup_context();
    let forms = crate::emacs_core::value_reader::read_all(src).expect("parse");
    forms
        .into_iter()
        .map(|form| {
            let result = eval.eval_form(form);
            format_eval_result(&result)
        })
        .collect()
}

/// Evaluate the first form from SRC in a cached runtime-startup evaluator and
/// return the formatted result.
pub fn runtime_startup_eval_one(src: &str) -> String {
    let mut eval = runtime_startup_context();
    let result = eval.eval_str(src);
    format_eval_result(&result)
}
