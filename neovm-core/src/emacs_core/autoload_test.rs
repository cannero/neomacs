use super::*;
use crate::emacs_core::load::{apply_runtime_startup_state, create_bootstrap_evaluator_cached};
use crate::emacs_core::{Context, format_eval_result, parse_forms};

fn eval_one(src: &str) -> String {
    let mut ev = Context::new();
    let forms = parse_forms(src).expect("parse");
    let result = ev.eval_expr(&forms[0]);
    format_eval_result(&result)
}

fn eval_all(src: &str) -> Vec<String> {
    let mut ev = Context::new();
    let forms = parse_forms(src).expect("parse");
    ev.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

fn eval_all_with(ev: &mut Context, src: &str) -> Vec<String> {
    let forms = parse_forms(src).expect("parse");
    ev.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

fn bootstrap_eval_all(src: &str) -> Vec<String> {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("startup");
    let forms = parse_forms(src).expect("parse");
    ev.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

fn bootstrap_eval_one(src: &str) -> String {
    bootstrap_eval_all(src)
        .into_iter()
        .last()
        .expect("bootstrap eval result")
}

// -----------------------------------------------------------------------
// AutoloadManager unit tests
// -----------------------------------------------------------------------

#[test]
fn autoload_manager_register_and_lookup() {
    let mut mgr = AutoloadManager::new();
    assert!(!mgr.is_autoloaded("foo"));

    mgr.register(AutoloadEntry {
        name: "foo".into(),
        file: "foo-lib".into(),
        docstring: Some("Do foo things.".into()),
        interactive: false,
        autoload_type: AutoloadType::Function,
    });

    assert!(mgr.is_autoloaded("foo"));
    let entry = mgr.get_entry("foo").unwrap();
    assert_eq!(entry.file, "foo-lib");
    assert_eq!(entry.docstring.as_deref(), Some("Do foo things."));
    assert!(!entry.interactive);
    assert_eq!(entry.autoload_type, AutoloadType::Function);
}

#[test]
fn autoload_manager_remove() {
    let mut mgr = AutoloadManager::new();
    mgr.register(AutoloadEntry {
        name: "bar".into(),
        file: "bar-lib".into(),
        docstring: None,
        interactive: true,
        autoload_type: AutoloadType::Macro,
    });
    assert!(mgr.is_autoloaded("bar"));
    mgr.remove("bar");
    assert!(!mgr.is_autoloaded("bar"));
}

#[test]
fn autoload_manager_multiple_entries() {
    let mut mgr = AutoloadManager::new();
    mgr.register(AutoloadEntry {
        name: "a".into(),
        file: "file-a".into(),
        docstring: None,
        interactive: false,
        autoload_type: AutoloadType::Function,
    });
    mgr.register(AutoloadEntry {
        name: "b".into(),
        file: "file-b".into(),
        docstring: None,
        interactive: false,
        autoload_type: AutoloadType::Keymap,
    });
    assert!(mgr.is_autoloaded("a"));
    assert!(mgr.is_autoloaded("b"));
    assert!(!mgr.is_autoloaded("c"));
}

#[test]
fn autoload_type_from_value() {
    assert_eq!(
        AutoloadType::from_value(&Value::Nil),
        AutoloadType::Function
    );
    assert_eq!(
        AutoloadType::from_value(&Value::symbol("macro")),
        AutoloadType::Macro
    );
    assert_eq!(
        AutoloadType::from_value(&Value::symbol("keymap")),
        AutoloadType::Keymap
    );
    assert_eq!(
        AutoloadType::from_value(&Value::symbol("unknown")),
        AutoloadType::Function
    );
}

#[test]
fn autoload_type_roundtrip() {
    let types = [
        AutoloadType::Function,
        AutoloadType::Macro,
        AutoloadType::Keymap,
    ];
    for ty in &types {
        let val = ty.to_value();
        let back = AutoloadType::from_value(&val);
        assert_eq!(&back, ty);
    }
}

#[test]
fn after_load_add_and_take() {
    let mut mgr = AutoloadManager::new();
    mgr.add_after_load("my-file", Value::Int(1));
    mgr.add_after_load("my-file", Value::Int(2));
    mgr.add_after_load("other-file", Value::Int(3));

    let forms = mgr.take_after_load_forms("my-file");
    assert_eq!(forms.len(), 2);

    // After taking, should be empty
    let forms2 = mgr.take_after_load_forms("my-file");
    assert!(forms2.is_empty());

    // Other file still has its form
    let forms3 = mgr.take_after_load_forms("other-file");
    assert_eq!(forms3.len(), 1);
}

#[test]
fn loaded_files_tracking() {
    let mut mgr = AutoloadManager::new();
    assert!(!mgr.is_loaded("foo.el"));
    mgr.mark_loaded("foo.el");
    assert!(mgr.is_loaded("foo.el"));
    // Duplicate mark is harmless
    mgr.mark_loaded("foo.el");
    assert!(mgr.is_loaded("foo.el"));
}

#[test]
fn obsolete_function_tracking() {
    let mut mgr = AutoloadManager::new();
    assert!(!mgr.is_function_obsolete("old-fn"));
    mgr.make_obsolete("old-fn", "new-fn", "28.1");
    assert!(mgr.is_function_obsolete("old-fn"));
    let info = mgr.get_obsolete_function("old-fn").unwrap();
    assert_eq!(info.0, "new-fn");
    assert_eq!(info.1, "28.1");
}

#[test]
fn obsolete_variable_tracking() {
    let mut mgr = AutoloadManager::new();
    assert!(!mgr.is_variable_obsolete("old-var"));
    mgr.make_variable_obsolete("old-var", "new-var", "27.1");
    assert!(mgr.is_variable_obsolete("old-var"));
    let info = mgr.get_obsolete_variable("old-var").unwrap();
    assert_eq!(info.0, "new-var");
    assert_eq!(info.1, "27.1");
}

// -----------------------------------------------------------------------
// is_autoload_value tests
// -----------------------------------------------------------------------

#[test]
fn is_autoload_value_positive() {
    let val = Value::list(vec![Value::symbol("autoload"), Value::string("my-file")]);
    assert!(is_autoload_value(&val));
}

#[test]
fn is_autoload_value_negative() {
    assert!(!is_autoload_value(&Value::Nil));
    assert!(!is_autoload_value(&Value::Int(42)));
    assert!(!is_autoload_value(&Value::list(vec![
        Value::symbol("lambda"),
        Value::Nil,
    ])));
}

// -----------------------------------------------------------------------
// Special form tests (eval-level)
// -----------------------------------------------------------------------

#[test]
fn autoload_special_form_registers() {
    let results = eval_all(
        r#"(autoload 'my-func "my-file" "A function." t)
           (let ((f (symbol-function 'my-func)))
             (and (consp f) (eq (car f) 'autoload)))"#,
    );
    // autoload should return the function name as a symbol
    assert_eq!(results[0], "OK my-func");
    // The registered definition should be an autoload form.
    assert_eq!(results[1], "OK t");
}

#[test]
fn autoload_minimal_form() {
    // Minimal autoload: just function name and file
    let results = eval_all(
        r#"(autoload 'minimal-fn "min-file")
           (let ((f (symbol-function 'minimal-fn)))
             (and (consp f) (eq (car f) 'autoload)))"#,
    );
    assert_eq!(results[0], "OK minimal-fn");
    assert_eq!(results[1], "OK t");
}

#[test]
fn autoload_with_type() {
    let results = eval_all(
        r#"(autoload 'my-macro "macro-file" nil nil 'macro)
           (let ((f (symbol-function 'my-macro)))
             (and (consp f) (eq (car f) 'autoload)))"#,
    );
    assert_eq!(results[0], "OK my-macro");
    assert_eq!(results[1], "OK t");
}

#[test]
fn autoload_is_callable_subr_surface() {
    let results = bootstrap_eval_all(
        r#"(fboundp 'autoload)
           (special-form-p 'autoload)
           (subrp (symbol-function 'autoload))
           (subr-arity (symbol-function 'autoload))
           (func-arity 'autoload)
           (funcall 'autoload 'my-funcall-fn "my-funcall-file")
           (let ((f (symbol-function 'my-funcall-fn)))
             (and (consp f) (eq (car f) 'autoload)))"#,
    );
    assert_eq!(results[0], "OK t");
    assert_eq!(results[1], "OK nil");
    assert_eq!(results[2], "OK t");
    assert_eq!(results[3], "OK (2 . 5)");
    assert_eq!(results[4], "OK (2 . 5)");
    assert_eq!(results[5], "OK my-funcall-fn");
    assert_eq!(results[6], "OK t");
}

#[test]
fn autoload_rejects_too_many_arguments() {
    let result = eval_one(
        r#"(condition-case err
              (autoload 'too-many "x" nil nil nil nil)
            (error (list (car err) (cdr err))))"#,
    );
    assert_eq!(result, "OK (wrong-number-of-arguments (autoload 6))");
}

#[test]
fn autoload_funcall_type_checks_first_argument() {
    let result = eval_one(
        r#"(condition-case err
              (funcall 'autoload 1 "x")
            (error (list (car err) (cdr err))))"#,
    );
    assert_eq!(result, "OK (wrong-type-argument (symbolp 1))");
}

#[test]
fn eval_when_compile_evaluates_body() {
    let result = bootstrap_eval_one("(eval-when-compile (+ 1 2))");
    assert_eq!(result, "OK 3");
}

#[test]
fn eval_when_compile_multiple_forms() {
    let result = bootstrap_eval_one("(eval-when-compile 1 2 (+ 3 4))");
    assert_eq!(result, "OK 7");
}

#[test]
fn eval_when_compile_propagates_errors() {
    let result = bootstrap_eval_one(
        r#"(condition-case err
              (eval-when-compile (signal 'error '("boom")))
            (error (list (car err) (cdr err))))"#,
    );
    assert_eq!(result, r#"OK (error ("boom"))"#);
}

#[test]
fn eval_and_compile_evaluates_body() {
    let result = bootstrap_eval_one("(eval-and-compile (+ 10 20))");
    assert_eq!(result, "OK 30");
}

#[test]
fn eval_and_compile_multiple_forms() {
    // Should return the last form's value
    let result = bootstrap_eval_one("(eval-and-compile (setq x 1) (setq y 2) (+ x y))");
    assert_eq!(result, "OK 3");
}

#[test]
fn symbol_file_returns_nil() {
    let result = eval_one("(symbol-file 'cons)");
    assert_eq!(result, "OK nil");
}

#[test]
fn symbol_file_returns_autoload_file_for_function() {
    let result = eval_one(
        r#"(progn (autoload 'sym-file-probe "sym-file-probe-file") (symbol-file 'sym-file-probe))"#,
    );
    assert_eq!(result, r#"OK "sym-file-probe-file""#);
}

#[test]
fn symbol_file_type_gate_matches_defun_only() {
    let results = eval_all(
        r#"(autoload 'sym-file-type-probe "sym-file-type-probe-file")
           (symbol-file 'sym-file-type-probe 'defun)
           (symbol-file 'sym-file-type-probe 'var)
           (symbol-file 'sym-file-type-probe 'function)"#,
    );
    assert_eq!(results[1], r#"OK "sym-file-type-probe-file""#);
    assert_eq!(results[2], "OK nil");
    assert_eq!(results[3], "OK nil");
}

#[test]
fn symbol_file_non_symbol_returns_nil() {
    let results = eval_all(
        r#"(symbol-file 1)
           (symbol-file "x")
           (symbol-file 'car 1)"#,
    );
    assert_eq!(results[0], "OK nil");
    assert_eq!(results[1], "OK nil");
    assert_eq!(results[2], "OK nil");
}

#[test]
fn symbol_file_accepts_third_arg_but_not_fourth() {
    let results = eval_all(
        r#"(autoload 'sym-file-arity-probe "sym-file-arity-probe-file")
           (symbol-file 'sym-file-arity-probe 'defun t)
           (condition-case err
               (symbol-file 'sym-file-arity-probe 'defun t :extra)
             (error err))"#,
    );
    assert_eq!(results[1], r#"OK "sym-file-arity-probe-file""#);
    assert_eq!(results[2], "OK (wrong-number-of-arguments symbol-file 4)");
}

#[test]
fn autoload_entry_interactive_flag() {
    let mut mgr = AutoloadManager::new();
    mgr.register(AutoloadEntry {
        name: "cmd".into(),
        file: "cmd-file".into(),
        docstring: None,
        interactive: true,
        autoload_type: AutoloadType::Function,
    });
    let entry = mgr.get_entry("cmd").unwrap();
    assert!(entry.interactive);
}

#[test]
fn autoload_entry_keymap_type() {
    let mut mgr = AutoloadManager::new();
    mgr.register(AutoloadEntry {
        name: "my-map".into(),
        file: "map-file".into(),
        docstring: None,
        interactive: false,
        autoload_type: AutoloadType::Keymap,
    });
    let entry = mgr.get_entry("my-map").unwrap();
    assert_eq!(entry.autoload_type, AutoloadType::Keymap);
}

#[test]
fn autoload_overwrites_previous() {
    let mut mgr = AutoloadManager::new();
    mgr.register(AutoloadEntry {
        name: "f".into(),
        file: "old-file".into(),
        docstring: None,
        interactive: false,
        autoload_type: AutoloadType::Function,
    });
    mgr.register(AutoloadEntry {
        name: "f".into(),
        file: "new-file".into(),
        docstring: None,
        interactive: true,
        autoload_type: AutoloadType::Macro,
    });
    let entry = mgr.get_entry("f").unwrap();
    assert_eq!(entry.file, "new-file");
    assert!(entry.interactive);
    assert_eq!(entry.autoload_type, AutoloadType::Macro);
}

/// GNU Emacs: "If FUNCTION is already defined other than as an autoload,
/// this does nothing and returns nil."
#[test]
fn autoload_does_not_override_real_definition() {
    let results = eval_all(
        r#"(defalias 'already-defined #'(lambda () 42))
           (autoload 'already-defined "some-file")
           ;; autoload should return nil (skipped)
           ;; and the real definition should still be in place
           (already-defined)"#,
    );
    // autoload on an already-defined function returns nil
    assert_eq!(results[1], "OK nil");
    // Real definition still works
    assert_eq!(results[2], "OK 42");
}

#[test]
fn autoload_registers_in_autoload_manager() {
    let mut ev = Context::new();
    let results = eval_all_with(
        &mut ev,
        r#"(autoload 'test-auto-fn "test-auto-file" "Test doc" t 'macro)"#,
    );
    assert_eq!(results[0], "OK test-auto-fn");
    assert!(ev.autoloads.is_autoloaded("test-auto-fn"));
    let entry = ev.autoloads.get_entry("test-auto-fn").unwrap();
    assert_eq!(entry.file, "test-auto-file");
    assert_eq!(entry.docstring.as_deref(), Some("Test doc"));
    assert!(entry.interactive);
    assert_eq!(entry.autoload_type, AutoloadType::Macro);
}

// -----------------------------------------------------------------------
// eval-after-load / provide integration (bootstrap required)
// -----------------------------------------------------------------------

#[test]
fn eval_after_load_deferred_fires_on_provide() {
    // Register eval-after-load BEFORE providing the feature.
    // When provide is called, the deferred callback should fire.
    let results = bootstrap_eval_all(
        r#"(defvar neovm--eal-test-log nil)
           (eval-after-load 'neovm--eal-test-feat
             '(setq neovm--eal-test-log (cons 'deferred neovm--eal-test-log)))
           neovm--eal-test-log
           (provide 'neovm--eal-test-feat)
           neovm--eal-test-log"#,
    );
    // Before provide: log should be nil
    assert_eq!(results[2], "OK nil");
    // After provide: callback should have fired
    assert_eq!(results[4], "OK (deferred)");
}

#[test]
fn eval_after_load_immediate_fires_when_already_provided() {
    // When eval-after-load is called for an already-provided feature,
    // the callback should fire immediately.
    let results = bootstrap_eval_all(
        r#"(defvar neovm--eal-imm-log nil)
           (provide 'neovm--eal-imm-feat)
           (eval-after-load 'neovm--eal-imm-feat
             '(setq neovm--eal-imm-log (cons 'immediate neovm--eal-imm-log)))
           neovm--eal-imm-log"#,
    );
    // Should have fired immediately
    assert_eq!(results[3], "OK (immediate)");
}

#[test]
fn with_eval_after_load_fires_when_already_provided() {
    // with-eval-after-load macro wraps body in a lambda and calls eval-after-load.
    let results = bootstrap_eval_all(
        r#"(defvar neovm--weal-test-result nil)
           (provide 'neovm--weal-test-feat)
           (with-eval-after-load 'neovm--weal-test-feat
             (setq neovm--weal-test-result 'executed))
           neovm--weal-test-result"#,
    );
    assert_eq!(results[3], "OK executed");
}
