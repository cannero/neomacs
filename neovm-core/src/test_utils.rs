//! Common test utilities for neovm-core.
//!
//! Provides shared helpers used across all test modules.

use crate::emacs_core::intern::resolve_sym;
use crate::emacs_core::load::{
    apply_ldefs_boot_autoloads_for_names, bootstrap_load_path_entries,
    create_runtime_startup_evaluator_cached, find_file_in_load_path, get_load_path, load_file,
};
use crate::emacs_core::value::Value;
use crate::emacs_core::{Context, Expr, format_eval_result, parse_forms};
use std::path::PathBuf;

/// Initialize the tracing subscriber for test output.
///
/// Reads `RUST_LOG` env var for filter level (default: `info`).
/// Uses `with_test_writer()` so output is captured by the test runner
/// and shown on failure.
///
/// Safe to call multiple times — `try_init()` silently no-ops if
/// already initialized.
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
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug")),
        )
        .with_test_writer()
        .try_init();
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
    let help_forms = parse_forms(&help_source).expect("parse help.el");
    let mut found_substitute_command_keys = false;
    for form in &help_forms {
        eval.eval_expr(form)
            .unwrap_or_else(|err| panic!("eval help.el prefix: {err:?}"));
        if is_named_defun(form, "substitute-command-keys") {
            found_substitute_command_keys = true;
            break;
        }
    }
    assert!(
        found_substitute_command_keys,
        "help.el should define substitute-command-keys"
    );
}

fn is_named_defun(form: &Expr, name: &str) -> bool {
    match form {
        Expr::List(items) => matches!(
            (items.first(), items.get(1)),
            (Some(Expr::Symbol(id0)), Some(Expr::Symbol(id1)))
                if resolve_sym(*id0) == "defun" && resolve_sym(*id1) == name
        ),
        _ => false,
    }
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
    let forms = parse_forms(src).expect("parse");
    eval.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

/// Evaluate the first form from SRC in a cached runtime-startup evaluator and
/// return the formatted result.
pub fn runtime_startup_eval_one(src: &str) -> String {
    let mut eval = runtime_startup_context();
    let forms = parse_forms(src).expect("parse");
    let result = eval.eval_expr(&forms[0]);
    format_eval_result(&result)
}
