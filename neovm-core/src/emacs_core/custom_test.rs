use super::*;
use crate::emacs_core::builtins::symbols::{builtin_set, builtin_symbol_value};
use crate::emacs_core::intern::{intern, intern_uninterned};
use crate::emacs_core::load::{apply_runtime_startup_state, create_bootstrap_evaluator_cached};
use crate::emacs_core::{Evaluator, format_eval_result, parse_forms};

fn eval_all(src: &str) -> Vec<String> {
    let mut ev = Evaluator::new();
    let forms = parse_forms(src).expect("parse");
    ev.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

fn bootstrap_eval_all(src: &str) -> Vec<String> {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let forms = parse_forms(src).expect("parse");
    ev.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

// -- CustomManager unit tests ------------------------------------------

#[test]
fn custom_manager_new_is_empty() {
    let cm = CustomManager::new();
    assert!(cm.variables.is_empty());
    assert!(cm.groups.is_empty());
    assert!(cm.auto_buffer_local.is_empty());
}

#[test]
fn custom_manager_define_variable() {
    let mut cm = CustomManager::new();
    cm.define_variable(CustomVariable {
        name: "my-var".into(),
        custom_type: Value::symbol("integer"),
        group: Some("my-group".into()),
        documentation: Some("A variable.".into()),
        standard_value: Value::Int(42),
        set_function: None,
        get_function: None,
        initialize: None,
    });
    assert!(cm.is_custom_variable("my-var"));
    assert!(!cm.is_custom_variable("other"));
    assert_eq!(cm.get_variable("my-var").unwrap().name, "my-var");
}

#[test]
fn custom_manager_define_group() {
    let mut cm = CustomManager::new();
    cm.define_group(CustomGroup {
        name: "my-group".into(),
        members: vec![],
        documentation: Some("A group.".into()),
        parent: None,
    });
    assert!(cm.is_custom_group("my-group"));
    assert!(!cm.is_custom_group("other"));
}

#[test]
fn custom_manager_buffer_local() {
    let mut cm = CustomManager::new();
    assert!(!cm.is_auto_buffer_local("tab-width"));
    cm.make_variable_buffer_local("tab-width");
    assert!(cm.is_auto_buffer_local("tab-width"));
}

// -- defcustom special form tests ----------------------------------------

#[test]
fn defcustom_basic() {
    let results = eval_all(r#"(defcustom my-var 42 "My variable.")"#);
    assert_eq!(results[0], "OK my-var");
}

#[test]
fn defcustom_sets_value() {
    let results = eval_all(r#"(defcustom my-var 42 "My variable.") my-var"#);
    assert_eq!(results[1], "OK 42");
}

#[test]
fn defcustom_with_type() {
    let results = eval_all(r#"(defcustom my-var 42 "Docs." :type 'integer) my-var"#);
    assert_eq!(results[1], "OK 42");
}

#[test]
fn defcustom_with_group() {
    let results = eval_all(r#"(defcustom my-var 10 "Docs." :group 'my-group) my-var"#);
    assert_eq!(results[1], "OK 10");
}

#[test]
fn defcustom_does_not_override_existing() {
    let results = eval_all(r#"(setq my-var 99) (defcustom my-var 42 "Docs.") my-var"#);
    // defcustom should not override an existing value, like defvar
    assert_eq!(results[2], "OK 99");
}

#[test]
fn defcustom_marks_special() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(r#"(defcustom my-var 42 "Docs.")"#).expect("parse");
    let _result = ev.eval_expr(&forms[0]);
    assert!(ev.obarray().is_special("my-var"));
}

#[test]
fn defcustom_custom_variable_p() {
    let results = bootstrap_eval_all(
        r#"(defcustom my-var 42 "Docs.") (custom-variable-p 'my-var) (custom-variable-p 'other)"#,
    );
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK nil");
}

// -- defgroup special form tests -----------------------------------------

#[test]
fn defgroup_basic() {
    let results = eval_all(r#"(defgroup my-group nil "My group.")"#);
    assert_eq!(results[0], "OK my-group");
}

#[test]
fn defgroup_registers_group() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(r#"(defgroup my-group nil "Docs.")"#).expect("parse");
    let _result = ev.eval_expr(&forms[0]);
    assert!(ev.custom.is_custom_group("my-group"));
    assert!(!ev.custom.is_custom_group("other"));
}

#[test]
fn custom_group_p_unavailable_without_custom_library() {
    let results = eval_all(
        r#"(defgroup my-group nil "Docs.")
           (fboundp 'custom-group-p)
           (custom-group-p 'my-group)
           (custom-group-p 'other)"#,
    );
    assert_eq!(results[1], "OK nil");
    assert_eq!(results[2], "ERR (void-function (custom-group-p))");
    assert_eq!(results[3], "ERR (void-function (custom-group-p))");
}

#[test]
fn defgroup_with_parent_records_parent_group() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        r#"(defgroup parent-group nil "Parent.")
           (defgroup child-group nil "Child." :group 'parent-group)"#,
    )
    .expect("parse");
    let _results: Vec<_> = ev.eval_forms(&forms);
    let child = ev
        .custom
        .get_group("child-group")
        .expect("child-group should be registered");
    assert_eq!(child.parent.as_deref(), Some("parent-group"));
}

// -- defvar-local special form tests ------------------------------------

#[test]
fn defvar_local_basic() {
    let results = eval_all(r#"(defvar-local my-local 42) my-local"#);
    assert_eq!(results[0], "OK my-local");
    assert_eq!(results[1], "OK 42");
}

#[test]
fn defvar_local_marks_special() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(r#"(defvar-local my-local 42)"#).expect("parse");
    let _result = ev.eval_expr(&forms[0]);
    assert!(ev.obarray().is_special("my-local"));
}

#[test]
fn defvar_local_marks_buffer_local() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(r#"(defvar-local my-local 42)"#).expect("parse");
    let _result = ev.eval_expr(&forms[0]);
    assert!(ev.custom.is_auto_buffer_local("my-local"));
}

#[test]
fn defvar_local_does_not_override() {
    let results = eval_all(r#"(setq my-local 99) (defvar-local my-local 42) my-local"#);
    assert_eq!(results[2], "OK 99");
}

#[test]
fn defvar_local_with_docstring() {
    let results = eval_all(r#"(defvar-local my-local 42 "Documentation.") my-local"#);
    assert_eq!(results[1], "OK 42");
}

// -- setq-default special form tests -----------------------------------

#[test]
fn setq_default_basic() {
    let results = eval_all(r#"(defvar x 10) (setq-default x 42) x"#);
    assert_eq!(results[2], "OK 42");
}

#[test]
fn setq_default_multiple_pairs() {
    let results = eval_all(r#"(defvar a 1) (defvar b 2) (setq-default a 10 b 20) a"#);
    assert_eq!(results[3], "OK 10");
}

#[test]
fn setq_default_returns_last_value() {
    let results = eval_all(r#"(setq-default x 42)"#);
    assert_eq!(results[0], "OK 42");
}

#[test]
fn setq_default_follows_alias_resolution() {
    let results = eval_all(
        r#"(defvaralias 'vm-setq-default-alias 'vm-setq-default-base)
           (setq-default vm-setq-default-alias 3)
           (list (default-value 'vm-setq-default-base)
                 (default-value 'vm-setq-default-alias))"#,
    );
    assert_eq!(results[2], "OK (3 3)");
}

#[test]
fn setq_default_rejects_constant_symbols() {
    let results = eval_all(
        r#"(list
             (condition-case err (setq-default nil 1) (error err))
             (condition-case err (setq-default :foo 1) (error err)))"#,
    );
    assert_eq!(
        results[0],
        "OK ((setting-constant nil) (setting-constant :foo))"
    );
}

#[test]
fn setq_default_alias_triggers_variable_watchers_twice() {
    let results = eval_all(
        r#"(setq vm-setq-default-watch-events nil)
           (fset 'vm-setq-default-watch-rec
                 (lambda (symbol newval operation where)
                   (setq vm-setq-default-watch-events
                         (cons (list symbol newval operation where)
                               vm-setq-default-watch-events))))
           (defvaralias 'vm-setq-default-watch 'vm-setq-default-watch-base)
           (add-variable-watcher 'vm-setq-default-watch-base 'vm-setq-default-watch-rec)
           (setq-default vm-setq-default-watch 7)
           (length vm-setq-default-watch-events)"#,
    );
    assert_eq!(results[5], "OK 2");
}

// -- default-value and set-default builtins ----------------------------

#[test]
fn default_value_returns_global() {
    let results = eval_all(r#"(defvar my-var 42) (default-value 'my-var)"#);
    assert_eq!(results[1], "OK 42");
}

#[test]
fn default_value_void_signals_error() {
    let results = eval_all(r#"(default-value 'nonexistent-var)"#);
    assert!(results[0].starts_with("ERR"));
}

#[test]
fn keyword_defaults_and_symbol_values_self_evaluate() {
    let results = eval_all(
        r#"(list (default-value :foo) (default-toplevel-value :foo) (symbol-value :foo))"#,
    );
    assert_eq!(results[0], "OK (:foo :foo :foo)");
}

#[test]
fn uninterned_keyword_defaults_do_not_self_evaluate() {
    let results = eval_all(
        r#"(let ((s (make-symbol ":vm-k")))
             (list (condition-case e (eval s nil) (error (car e)))
                   (condition-case e (symbol-value s) (error (car e)))
                   (condition-case e (default-value s) (error (car e)))))"#,
    );
    assert_eq!(results[0], "OK (void-variable void-variable void-variable)");
}

#[test]
fn uninterned_value_cells_ignore_buffer_local_namesakes() {
    let mut eval = Evaluator::new();
    let canonical = intern("depth-alist");
    let uninterned = intern_uninterned("depth-alist");
    eval.buffers
        .current_buffer_mut()
        .expect("current buffer")
        .set_buffer_local("depth-alist", Value::Int(7));

    builtin_set(&mut eval, vec![Value::Symbol(uninterned), Value::Nil])
        .expect("set should bind uninterned symbol");

    assert_eq!(
        eval.obarray().symbol_value_id(uninterned).copied(),
        Some(Value::Nil)
    );
    assert_eq!(eval.obarray().symbol_value_id(canonical).copied(), None);
    assert_eq!(
        eval.buffers
            .current_buffer()
            .expect("current buffer")
            .get_buffer_local("depth-alist")
            .copied(),
        Some(Value::Int(7))
    );

    let value = builtin_default_value(&mut eval, vec![Value::Symbol(uninterned)])
        .expect("default-value should read uninterned symbol");
    assert_eq!(value, Value::Nil);
    let symbol_value = builtin_symbol_value(&mut eval, vec![Value::Symbol(uninterned)])
        .expect("symbol-value should read uninterned symbol");
    assert_eq!(symbol_value, Value::Nil);
}

#[test]
fn set_default_sets_global() {
    let results = eval_all(r#"(set-default 'my-var 99) (default-value 'my-var)"#);
    assert_eq!(results[1], "OK 99");
}

#[test]
fn set_default_and_default_value_follow_alias_resolution() {
    let results = eval_all(
        r#"(defvaralias 'vm-set-default-alias 'vm-set-default-base)
           (set-default 'vm-set-default-alias 5)
           (list (default-value 'vm-set-default-base)
                 (default-value 'vm-set-default-alias))"#,
    );
    assert_eq!(results[2], "OK (5 5)");
}

#[test]
fn default_value_alias_void_uses_original_symbol_in_error_payload() {
    let results = eval_all(
        r#"(defvaralias 'vm-default-alias-unbound 'vm-default-base-unbound)
           (condition-case err
               (default-value 'vm-default-alias-unbound)
             (error err))"#,
    );
    assert_eq!(results[1], "OK (void-variable vm-default-alias-unbound)");
}

#[test]
fn set_default_rejects_constant_symbols() {
    let results = eval_all(
        r#"(list
             (condition-case err (set-default nil 1) (error err))
             (condition-case err (set-default t 1) (error err))
             (condition-case err (set-default :foo 1) (error err)))"#,
    );
    assert_eq!(
        results[0],
        "OK ((setting-constant nil) (setting-constant t) (setting-constant :foo))"
    );
}

#[test]
fn set_default_triggers_variable_watchers() {
    let results = eval_all(
        r#"(fset 'vm-set-default-watch-rec
                 (lambda (symbol newval operation where)
                   (setq vm-set-default-watch-last
                         (list symbol newval operation where))))
           (add-variable-watcher 'vm-set-default-watch-target 'vm-set-default-watch-rec)
           (set-default 'vm-set-default-watch-target 42)
           vm-set-default-watch-last"#,
    );
    assert_eq!(results[3], "OK (vm-set-default-watch-target 42 set nil)");
}

#[test]
fn set_default_alias_triggers_variable_watchers_twice() {
    let results = eval_all(
        r#"(setq vm-set-default-alias-watch-events nil)
           (fset 'vm-set-default-alias-watch-rec
                 (lambda (symbol newval operation where)
                   (setq vm-set-default-alias-watch-events
                         (cons (list symbol newval operation where)
                               vm-set-default-alias-watch-events))))
           (defvaralias 'vm-set-default-alias-watch 'vm-set-default-alias-base)
           (add-variable-watcher 'vm-set-default-alias-base 'vm-set-default-alias-watch-rec)
           (set-default 'vm-set-default-alias-watch 9)
           (length vm-set-default-alias-watch-events)"#,
    );
    assert_eq!(results[5], "OK 2");
}

#[test]
fn set_default_toplevel_alias_triggers_variable_watchers_twice() {
    let results = eval_all(
        r#"(setq vm-set-default-top-watch-events nil)
           (fset 'vm-set-default-top-watch-rec
                 (lambda (symbol newval operation where)
                   (setq vm-set-default-top-watch-events
                         (cons (list symbol newval operation where)
                               vm-set-default-top-watch-events))))
           (defvaralias 'vm-set-default-top-watch 'vm-set-default-top-base)
           (add-variable-watcher 'vm-set-default-top-base 'vm-set-default-top-watch-rec)
           (set-default-toplevel-value 'vm-set-default-top-watch 7)
           (length vm-set-default-top-watch-events)"#,
    );
    assert_eq!(results[5], "OK 2");
}

// -- make-variable-buffer-local builtin --------------------------------

#[test]
fn make_variable_buffer_local_works() {
    let results = eval_all(r#"(make-variable-buffer-local 'my-var)"#);
    assert_eq!(results[0], "OK my-var");
}

#[test]
fn make_variable_buffer_local_binds_unbound_symbol_to_nil_like_gnu() {
    let result = eval_all(
        r#"(progn
             (makunbound 'vm-mvbl-unbound)
             (make-variable-buffer-local 'vm-mvbl-unbound)
             (list (boundp 'vm-mvbl-unbound)
                   (default-value 'vm-mvbl-unbound)
                   (with-temp-buffer
                     (local-variable-p 'vm-mvbl-unbound))))"#,
    );
    assert_eq!(result[0], "OK (t nil nil)");
}

#[test]
fn make_variable_buffer_local_resolves_alias_for_auto_local_assignment() {
    let result = eval_all(
        r#"(setq vm-mvbl-base 1)
           (defvaralias 'vm-mvbl-alias 'vm-mvbl-base)
           (make-variable-buffer-local 'vm-mvbl-alias)
           (with-temp-buffer
             (setq vm-mvbl-alias 7)
             (list (local-variable-p 'vm-mvbl-alias)
                   (local-variable-p 'vm-mvbl-base)
                   vm-mvbl-alias
                   vm-mvbl-base
                   (default-value 'vm-mvbl-base)))"#,
    );
    assert_eq!(result[3], "OK (t t 7 7 1)");
}

#[test]
fn make_variable_buffer_local_constant_and_keyword_payloads_match_oracle() {
    let result = eval_all(
        r#"(list
             (condition-case err (make-variable-buffer-local nil) (error err))
             (condition-case err (make-variable-buffer-local t) (error err))
             (condition-case err (make-variable-buffer-local :vm-mvbl-k) (error err))
             (condition-case err (make-variable-buffer-local 1) (error err)))"#,
    );
    assert_eq!(
        result[0],
        "OK ((setting-constant nil) (setting-constant t) (setting-constant :vm-mvbl-k) (wrong-type-argument symbolp 1))"
    );
}

// -- make-local-variable builtin ---------------------------------------

#[test]
fn make_local_variable_in_buffer() {
    let results = eval_all(
        r#"(defvar my-var 42)
           (get-buffer-create "test-buf")
           (set-buffer "test-buf")
           (make-local-variable 'my-var)
           (local-variable-p 'my-var)"#,
    );
    assert_eq!(results[4], "OK t");
}

#[test]
fn make_local_variable_resolves_alias_bindings() {
    let result = eval_all(
        r#"(setq vm-mlv-base 4)
           (defvaralias 'vm-mlv-alias 'vm-mlv-base)
           (with-temp-buffer
             (make-local-variable 'vm-mlv-alias)
             (list (local-variable-p 'vm-mlv-alias)
                   (local-variable-p 'vm-mlv-base)
                   (symbol-value 'vm-mlv-alias)
                   (symbol-value 'vm-mlv-base)
                   (default-value 'vm-mlv-base)))"#,
    );
    assert_eq!(result[2], "OK (t t 4 4 4)");
}

#[test]
fn make_local_variable_preserves_existing_buffer_local_binding() {
    let result = eval_all(
        r#"(progn
             (setq vm-mlv-preserve-global 1)
             (with-temp-buffer
               (setq-local vm-mlv-preserve-global 9)
               (make-local-variable 'vm-mlv-preserve-global)
               (list vm-mlv-preserve-global
                     (default-value 'vm-mlv-preserve-global))))"#,
    );
    assert_eq!(result[0], "OK (9 1)");
}

#[test]
fn make_local_variable_captures_dynamic_value_in_new_local_binding() {
    let result = eval_all(
        r#"(let ((buf (get-buffer-create "vm-mlv-buf")))
             (let ((vm-mlv-cross 5))
               (set-buffer buf)
               (make-local-variable 'vm-mlv-cross))
             (set-buffer buf)
             (condition-case err vm-mlv-cross (error err)))"#,
    );
    assert_eq!(result[0], "OK 5");
}

#[test]
fn make_local_variable_on_void_symbol_creates_local_void_binding() {
    let result = bootstrap_eval_all(
        r#"(with-temp-buffer
             (makunbound 'vm-mlv-void)
             (make-local-variable 'vm-mlv-void)
             (list (local-variable-p 'vm-mlv-void (current-buffer))
                   (buffer-local-boundp 'vm-mlv-void (current-buffer))
                   (condition-case err (symbol-value 'vm-mlv-void) (error (car err)))
                   (condition-case err
                       (buffer-local-value 'vm-mlv-void (current-buffer))
                     (error (car err)))
                   (not (null (memq 'vm-mlv-void (buffer-local-variables))))
                   (assoc 'vm-mlv-void (buffer-local-variables))))"#,
    );
    assert_eq!(result[0], "OK (t nil void-variable void-variable t nil)");
}

#[test]
fn make_local_variable_ignores_lexical_bindings_like_gnu() {
    let result = bootstrap_eval_all(
        r#"(let ((lexical-binding t))
             (eval
              '(progn
                 (setq vm-mlv-lex-global 'global)
                 (with-temp-buffer
                   (let ((vm-mlv-lex-global 'lex))
                     (make-local-variable 'vm-mlv-lex-global)
                     (list vm-mlv-lex-global
                           (symbol-value 'vm-mlv-lex-global)
                           (buffer-local-value 'vm-mlv-lex-global (current-buffer))
                           (local-variable-p 'vm-mlv-lex-global (current-buffer))
                           (buffer-local-boundp 'vm-mlv-lex-global (current-buffer))
                           (default-value 'vm-mlv-lex-global)))))
              t))"#,
    );
    assert_eq!(result[0], "OK (lex global global t t global)");
}

#[test]
fn make_local_variable_constant_and_keyword_payloads_match_oracle() {
    let result = eval_all(
        r#"(list
             (condition-case err (with-temp-buffer (make-local-variable nil)) (error err))
             (condition-case err (with-temp-buffer (make-local-variable t)) (error err))
             (condition-case err (with-temp-buffer (make-local-variable :vm-k)) (error err))
             (condition-case err (with-temp-buffer (make-local-variable 1)) (error err)))"#,
    );
    assert_eq!(
        result[0],
        "OK ((setting-constant nil) (setting-constant t) (setting-constant :vm-k) (wrong-type-argument symbolp 1))"
    );
}

// -- local-variable-p builtin ------------------------------------------

#[test]
fn local_variable_p_returns_nil_when_not_local() {
    let results = eval_all(
        r#"(get-buffer-create "test-buf")
           (set-buffer "test-buf")
           (local-variable-p 'nonexistent)"#,
    );
    assert_eq!(results[2], "OK nil");
}

#[test]
fn local_variable_p_reports_builtin_buffer_locals() {
    let results = eval_all(
        r#"(with-temp-buffer
             (list (local-variable-p 'major-mode)
                   (local-variable-p 'mode-name)
                   (local-variable-p 'buffer-undo-list)))"#,
    );
    assert_eq!(results[0], "OK (t t t)");
}

#[test]
fn local_variable_p_enforces_buffer_and_symbol_contracts() {
    let results = eval_all(
        r#"(list
             (condition-case err (local-variable-p 'x) (error err))
             (condition-case err (local-variable-p 'x nil) (error err))
             (condition-case err (local-variable-p 'x (current-buffer)) (error err))
             (condition-case err (local-variable-p 'x 1) (error err))
             (condition-case err (local-variable-p 1 (current-buffer)) (error err))
             (condition-case err (local-variable-p :vm-k (current-buffer)) (error err))
             (condition-case err (local-variable-p nil (current-buffer)) (error err))
             (condition-case err (local-variable-p t (current-buffer)) (error err))
             (condition-case err (local-variable-p 'x (current-buffer) nil) (error err)))"#,
    );
    assert_eq!(
        results[0],
        "OK (nil nil nil (wrong-type-argument bufferp 1) (wrong-type-argument symbolp 1) nil nil nil (wrong-number-of-arguments local-variable-p 3))"
    );
}

#[test]
fn local_and_buffer_local_predicates_follow_alias_resolution() {
    let results = bootstrap_eval_all(
        r#"(defvaralias 'vm-local-p-alias 'vm-local-p-base)
           (let ((buf (get-buffer-create "vm-local-p-buf")))
             (set-buffer buf)
             (setq-local vm-local-p-alias 8)
             (list (local-variable-p 'vm-local-p-alias buf)
                   (local-variable-p 'vm-local-p-base buf)
                   (buffer-local-boundp 'vm-local-p-alias buf)
                   (buffer-local-boundp 'vm-local-p-base buf)))"#,
    );
    assert_eq!(results[1], "OK (t t t t)");
}

#[test]
fn buffer_local_bound_p_matches_emacs_shape() {
    let results = bootstrap_eval_all(
        r#"(defvar neomacs-buffer-local-boundp-global 1)
           (let ((buf (get-buffer-create "test-buf")))
             (buffer-local-boundp 'neomacs-buffer-local-boundp-global buf))
           (let ((buf (get-buffer-create "test-buf")))
             (buffer-local-boundp 'neomacs-buffer-local-boundp-missing buf))
           (let ((buf (get-buffer-create "test-buf-local")))
             (set-buffer buf)
             (make-local-variable 'neomacs-buffer-local-boundp-local)
             (setq neomacs-buffer-local-boundp-local 7)
             (buffer-local-boundp 'neomacs-buffer-local-boundp-local buf))
           (let ((buf (get-buffer-create "dead-buf")))
             (kill-buffer buf)
             (buffer-local-boundp 'neomacs-buffer-local-boundp-global buf))
           (condition-case err (buffer-local-boundp 1 (current-buffer)) (error (car err)))
           (condition-case err (buffer-local-boundp 'x nil) (error (car err)))
           (condition-case err (buffer-local-boundp 'x (current-buffer) nil)
             (error (car err)))"#,
    );
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK nil");
    assert_eq!(results[3], "OK t");
    assert_eq!(results[4], r#"ERR (error ("No such buffer"))"#);
    assert_eq!(results[5], "OK wrong-type-argument");
    assert_eq!(results[6], "OK wrong-type-argument");
    assert_eq!(results[7], "OK wrong-number-of-arguments");
}

// -- buffer-local-variables builtin ------------------------------------

#[test]
fn buffer_local_variables_include_default_entries() {
    let results = eval_all(
        r#"(get-buffer-create "test-buf")
           (set-buffer "test-buf")
           (let ((locals (buffer-local-variables)))
             (and (listp locals)
                  (assq 'buffer-read-only locals)))"#,
    );
    assert_eq!(results[2], "OK (buffer-read-only)");
}

#[test]
fn buffer_local_variables_argument_validation() {
    let results = eval_all(
        r#"(condition-case err (buffer-local-variables 1) (error err))
           (condition-case err (buffer-local-variables "test-buf") (error err))
           (condition-case err (buffer-local-variables nil nil) (error err))"#,
    );
    assert_eq!(results[0], "OK (wrong-type-argument bufferp 1)");
    assert_eq!(results[1], "OK (wrong-type-argument bufferp \"test-buf\")");
    assert_eq!(
        results[2],
        "OK (wrong-number-of-arguments buffer-local-variables 2)"
    );
}

// -- kill-local-variable builtin ----------------------------------------

#[test]
fn kill_local_variable_removes_binding() {
    let results = eval_all(
        r#"(defvar my-var 42)
           (get-buffer-create "test-buf")
           (set-buffer "test-buf")
           (make-local-variable 'my-var)
           (local-variable-p 'my-var)
           (kill-local-variable 'my-var)
           (local-variable-p 'my-var)"#,
    );
    assert_eq!(results[4], "OK t");
    assert_eq!(results[6], "OK nil");
}

#[test]
fn kill_local_variable_resolves_alias_bindings() {
    let results = eval_all(
        r#"(defvaralias 'vm-klv-alias 'vm-klv-base)
           (with-temp-buffer
             (setq-local vm-klv-alias 3)
             (kill-local-variable 'vm-klv-alias)
             (list (local-variable-p 'vm-klv-alias)
                   (local-variable-p 'vm-klv-base)
                   (condition-case err
                       (symbol-value 'vm-klv-alias)
                     (error (car err)))))"#,
    );
    assert_eq!(results[1], "OK (nil nil void-variable)");
}

#[test]
fn kill_local_variable_accepts_keywords_like_oracle() {
    let result = eval_all(
        r#"(list
             (condition-case err (with-temp-buffer (kill-local-variable nil)) (error err))
             (condition-case err (with-temp-buffer (kill-local-variable t)) (error err))
             (condition-case err (with-temp-buffer (kill-local-variable :vm-k)) (error err))
             (condition-case err (with-temp-buffer (kill-local-variable 1)) (error err)))"#,
    );
    assert_eq!(
        result[0],
        "OK (nil t :vm-k (wrong-type-argument symbolp 1))"
    );
}

#[test]
fn kill_local_variable_triggers_makunbound_watcher_with_buffer_where() {
    let result = eval_all(
        r#"(progn
             (setq vm-klv-a-events nil)
             (fset 'vm-klv-a-rec
                   (lambda (symbol newval operation where)
                     (setq vm-klv-a-events
                           (cons (list symbol newval operation (bufferp where) (buffer-live-p where))
                                 vm-klv-a-events))))
             (defvaralias 'vm-klv-a-alias 'vm-klv-a-base)
             (add-variable-watcher 'vm-klv-a-base 'vm-klv-a-rec)
             (with-temp-buffer
               (setq-local vm-klv-a-alias 7)
               (kill-local-variable 'vm-klv-a-alias))
             vm-klv-a-events)"#,
    );
    assert_eq!(
        result[0],
        "OK ((vm-klv-a-base nil makunbound t t) (vm-klv-a-base 7 set t t))"
    );
}

// -- custom-set-variables builtin --------------------------------------

#[test]
fn custom_set_variables_basic() {
    let results = bootstrap_eval_all(
        r#"(defvar my-var 1)
           (custom-set-variables '(my-var 42))
           (default-value 'my-var)"#,
    );
    assert_eq!(results[2], "OK 42");
}

#[test]
fn custom_set_variables_ignores_unknown_variable() {
    let results = eval_all(
        r#"(custom-set-variables '(my-var 42))
           (condition-case err (default-value 'my-var) (error err))"#,
    );
    assert_eq!(results[1], "OK (void-variable my-var)");
}

// -- custom-set-faces --------------------------------------------------

#[test]
fn custom_set_faces_returns_nil() {
    let results = bootstrap_eval_all(r#"(custom-set-faces '(default ((t (:height 120)))))"#);
    assert_eq!(results[0], "OK nil");
}

#[test]
fn custom_set_faces_non_list_spec_errors() {
    let results = bootstrap_eval_all(r#"(condition-case err (custom-set-faces 1) (error err))"#);
    assert_eq!(results[0], r#"OK (error "Incompatible Custom theme spec")"#);
}

#[test]
fn custom_set_faces_requires_symbol_face_name() {
    let results =
        bootstrap_eval_all(r#"(condition-case err (custom-set-faces '(1 2)) (error err))"#);
    assert_eq!(results[0], "OK (wrong-type-argument symbolp 1)");
}

#[test]
fn custom_set_variables_errors_for_non_list_spec() {
    let results =
        bootstrap_eval_all(r#"(condition-case err (custom-set-variables 1) (error err))"#);
    assert_eq!(results[0], "OK (wrong-type-argument listp 1)");
}

#[test]
fn custom_set_variables_errors_for_non_symbol_variable_name() {
    let results =
        bootstrap_eval_all(r#"(condition-case err (custom-set-variables '(1 2)) (error err))"#);
    assert_eq!(results[0], "OK (wrong-type-argument symbolp 1)");
}

// -- Integration tests -------------------------------------------------

#[test]
fn defcustom_then_setq_default() {
    let results = eval_all(
        r#"(defcustom my-opt 10 "Opt." :type 'integer)
           (setq-default my-opt 20)
           my-opt"#,
    );
    assert_eq!(results[2], "OK 20");
}

#[test]
fn defvar_local_then_buffer_local_check() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        r#"(defvar-local my-local-var 99)
           (make-variable-buffer-local 'other-var)"#,
    )
    .expect("parse");
    let _results: Vec<_> = ev.eval_forms(&forms);
    assert!(ev.custom.is_auto_buffer_local("my-local-var"));
    assert!(ev.custom.is_auto_buffer_local("other-var"));
}

#[test]
fn defcustom_keyword_args_ignored_gracefully() {
    // Extra keywords like :initialize should not cause errors
    let results = eval_all(
        r#"(defcustom my-var 5 "Docs." :type 'integer :group 'editing :initialize 'custom-initialize-default) my-var"#,
    );
    assert_eq!(results[1], "OK 5");
}

#[test]
fn defgroup_multiple_groups() {
    let mut ev = Evaluator::new();
    let forms = parse_forms(
        r#"(defgroup g1 nil "Group 1.")
           (defgroup g2 nil "Group 2.")"#,
    )
    .expect("parse");
    let _results: Vec<_> = ev.eval_forms(&forms);
    assert!(ev.custom.is_custom_group("g1"));
    assert!(ev.custom.is_custom_group("g2"));
}

#[test]
fn setq_default_works_on_new_variable() {
    let results = eval_all(r#"(setq-default new-var 100) new-var"#);
    assert_eq!(results[1], "OK 100");
}
