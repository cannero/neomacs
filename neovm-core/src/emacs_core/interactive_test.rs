use super::*;
use crate::emacs_core::load::{
    apply_ldefs_boot_autoloads_for_names, apply_runtime_startup_state, bootstrap_load_path_entries,
    create_bootstrap_evaluator_cached,
};
use crate::emacs_core::{Evaluator, format_eval_result, parse_forms};
use std::fs;
use std::path::PathBuf;

/// Create evaluator with minimal Elisp shims for interactive testing.
fn eval_with_interactive_shims() -> Evaluator {
    let mut ev = Evaluator::new();
    install_bare_elisp_shims(&mut ev);
    let shims = r#"
(defalias 'set-mark #'(lambda (pos)
  (if pos (set-marker (mark-marker) pos (current-buffer)))
  nil))
(defalias 'mark #'(lambda (&optional force)
  (let ((m (mark-marker)))
    (if (and m (marker-position m)) (marker-position m)))))
(defalias 'macrop #'(lambda (object)
  (and (consp object) (eq (car object) 'macro))))
(defalias 'special-form-p #'(lambda (object)
  nil))
(defalias 'other-window #'(lambda (count &optional all-frames)
  nil))
"#;
    let forms = parse_forms(shims).expect("parse shims");
    for form in &forms {
        let _ = ev.eval_expr(form);
    }
    ev
}

fn eval_all(src: &str) -> Vec<String> {
    let mut ev = eval_with_interactive_shims();
    let forms = parse_forms(src).expect("parse");
    ev.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

fn eval_one(src: &str) -> String {
    eval_all(src).into_iter().next().expect("at least one form")
}

fn eval_all_with(ev: &mut Evaluator, src: &str) -> Vec<String> {
    let forms = parse_forms(src).expect("parse");
    ev.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

fn bootstrap_eval_all(src: &str) -> Vec<String> {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    eval_all_with(&mut ev, src)
}

fn eval_with_ldefs_boot_autoloads(names: &[&str]) -> Evaluator {
    let mut ev = Evaluator::new();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let lisp_dir = project_root.join("lisp");
    ev.set_variable(
        "load-path",
        Value::list(bootstrap_load_path_entries(&lisp_dir)),
    );
    for name in names {
        ev.obarray_mut().fmakunbound(name);
    }
    apply_ldefs_boot_autoloads_for_names(&mut ev, names).expect("ldefs-boot autoload restore");
    ev
}

fn eval_first_form_after_marker(eval: &mut Evaluator, source: &str, marker: &str) {
    let start = source
        .find(marker)
        .unwrap_or_else(|| panic!("missing GNU subr.el marker: {marker}"));
    let forms = parse_forms(&source[start..])
        .unwrap_or_else(|err| panic!("parse GNU subr.el from {marker} failed: {:?}", err));
    let form = forms
        .first()
        .unwrap_or_else(|| panic!("no GNU subr.el form found after marker: {marker}"));
    eval.eval_expr(form)
        .unwrap_or_else(|err| panic!("evaluate GNU subr.el form {marker} failed: {:?}", err));
}

/// Install minimal `defun`/`defmacro`/`when`/`unless` shims so a bare
/// evaluator can evaluate forms extracted from GNU `.el` source files.
fn install_bare_elisp_shims(ev: &mut Evaluator) {
    let shims = r#"
(defalias 'defun (cons 'macro #'(lambda (name arglist &rest body)
  (list 'defalias (list 'quote name) (cons 'function (list (cons 'lambda (cons arglist body))))))))
(defalias 'defmacro (cons 'macro #'(lambda (name arglist &rest body)
  (list 'defalias (list 'quote name)
        (list 'cons ''macro (cons 'function (list (cons 'lambda (cons arglist body)))))))))
(defalias 'when (cons 'macro #'(lambda (cond &rest body)
  (list 'if cond (cons 'progn body)))))
(defalias 'unless (cons 'macro #'(lambda (cond &rest body)
  (cons 'if (cons cond (cons nil body))))))
"#;
    let forms = parse_forms(shims).expect("parse bare elisp shims");
    for form in &forms {
        ev.eval_expr(form).expect("install bare elisp shim");
    }
}

fn gnu_subr_keymap_eval_all(src: &str) -> Vec<String> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let subr_path = project_root.join("lisp/subr.el");
    let subr_source = fs::read_to_string(&subr_path).expect("read GNU subr.el");

    let mut ev = Evaluator::new();
    install_bare_elisp_shims(&mut ev);
    ev.set_lexical_binding(true);
    for marker in [
        "(defun global-key-binding",
        "(defvar esc-map",
        "(fset 'ESC-prefix esc-map)",
        "(defvar ctl-x-4-map",
        "(defalias 'ctl-x-4-prefix ctl-x-4-map)",
        "(defvar ctl-x-5-map",
        "(defalias 'ctl-x-5-prefix ctl-x-5-map)",
        "(defvar tab-prefix-map",
        "(defvar ctl-x-map",
        "(fset 'Control-X-prefix ctl-x-map)",
        "(defvar global-map",
        "(use-global-map global-map)",
    ] {
        eval_first_form_after_marker(&mut ev, &subr_source, marker);
    }
    eval_all_with(&mut ev, src)
}

fn gnu_simple_command_execute_eval() -> Evaluator {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let simple_path = project_root.join("lisp/simple.el");
    let simple_source = fs::read_to_string(&simple_path).expect("read GNU simple.el");
    let subr_path = project_root.join("lisp/subr.el");
    let subr_source = fs::read_to_string(&subr_path).expect("read GNU subr.el");

    let mut ev = Evaluator::new();
    install_bare_elisp_shims(&mut ev);
    let setup_forms = parse_forms(
        r#"
        (defalias 'when-let* (cons 'macro #'(lambda (bindings &rest body)
          (let ((binding (car bindings)))
            (if (consp binding)
                (list 'let
                      (list (list (car binding) (car (cdr binding))))
                      (list 'if (car binding) (cons 'progn body)))
              (cons 'progn body))))))
        (defalias 'autoloadp #'(lambda (object) (eq 'autoload (car-safe object))))
        (fset 'prefix-command-update (lambda () nil))
        (fset 'add-to-history (lambda (&rest _args) nil))
        (fset 'macroexp--obsolete-warning (lambda (&rest _args) ""))
        (fset 'help--key-description-fontified (lambda (&rest _args) ""))
        (fset 'where-is-internal (lambda (&rest _args) nil))
        "#,
    )
    .expect("parse command-execute test stubs");
    ev.eval_forms(&setup_forms);
    eval_first_form_after_marker(&mut ev, &subr_source, "(defun error (string &rest args)");
    eval_first_form_after_marker(
        &mut ev,
        &simple_source,
        "(defun command-execute (cmd &optional record-flag keys special)",
    );
    eval_first_form_after_marker(
        &mut ev,
        &simple_source,
        "(defun command-execute--query (command)",
    );
    // Load set-mark and its dependencies (needed by interactive region commands).
    // set-mark calls activate-mark, which calls region-active-p and mark.
    eval_first_form_after_marker(&mut ev, &simple_source, "(defun mark (&optional force)");
    eval_first_form_after_marker(&mut ev, &simple_source, "(defun activate-mark");
    eval_first_form_after_marker(&mut ev, &simple_source, "(defun set-mark (pos)");
    ev
}

fn gnu_simple_command_execute_eval_all(src: &str) -> Vec<String> {
    // Use bootstrap evaluator — all Elisp functions (set-mark,
    // activate-mark, region-active-p, command-execute, etc.) are loaded.
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    eval_all_with(&mut ev, src)
}

fn gnu_simple_execute_extended_command_eval() -> Evaluator {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let simple_path = project_root.join("lisp/simple.el");
    let simple_source = fs::read_to_string(&simple_path).expect("read GNU simple.el");

    let mut ev = gnu_simple_command_execute_eval();
    let setup_forms = parse_forms(
        r#"
        (setq suggest-key-bindings nil)
        (setq extended-command-suggest-shorter nil)
        (setq execute-extended-command--binding-timer nil)
        (setq executing-kbd-macro nil)
        (fset 'read-extended-command
              (lambda (&rest _args)
                (signal 'end-of-file '("Error reading from stdin"))))
        "#,
    )
    .expect("parse execute-extended-command test stubs");
    ev.eval_forms(&setup_forms);
    eval_first_form_after_marker(
        &mut ev,
        &simple_source,
        "(defun execute-extended-command (prefixarg &optional command-name typed)",
    );
    ev
}

fn gnu_files_command_eval() -> Evaluator {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let files_path = project_root.join("lisp/files.el");
    let files_source = fs::read_to_string(&files_path).expect("read GNU files.el");

    let mut ev = gnu_simple_command_execute_eval();
    for marker in [
        "(defun find-file-read-args (prompt mustmatch)",
        "(defun find-file (filename &optional wildcards)",
        "(defun save-buffer (&optional arg)",
    ] {
        eval_first_form_after_marker(&mut ev, &files_source, marker);
    }
    ev
}

fn gnu_files_command_eval_all(src: &str) -> Vec<String> {
    let mut ev = gnu_files_command_eval();
    eval_all_with(&mut ev, src)
}

fn gnu_simple_execute_extended_command_eval_all(src: &str) -> Vec<String> {
    let mut ev = gnu_simple_execute_extended_command_eval();
    eval_all_with(&mut ev, src)
}

fn load_gnu_eval_expression_into(ev: &mut Evaluator) {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let simple_path = project_root.join("lisp/simple.el");
    let simple_source = fs::read_to_string(&simple_path).expect("read GNU simple.el");

    let setup_forms = parse_forms(
        r#"
        (defalias 'read--expression #'(lambda (&rest _args)
          (signal 'end-of-file '("Error reading from stdin"))))
        (defalias 'eval-expression-get-print-arguments #'(lambda (&rest _args) nil))
        "#,
    )
    .expect("parse eval-expression test stubs");
    ev.eval_forms(&setup_forms);
    eval_first_form_after_marker(
        ev,
        &simple_source,
        "(defun eval-expression (exp &optional insert-value no-truncate char-print-limit)",
    );
}

fn gnu_simple_eval_expression_eval() -> Evaluator {
    let mut ev = Evaluator::new();
    install_bare_elisp_shims(&mut ev);
    load_gnu_eval_expression_into(&mut ev);
    ev
}

fn read_first_object(ev: &mut Evaluator, src: &str) -> Value {
    let result = crate::emacs_core::reader::builtin_read_from_string(ev, vec![Value::string(src)])
        .unwrap_or_else(|err| panic!("read-from-string failed for {src:?}: {err:?}"));
    let Value::Cons(cell) = result else {
        panic!("expected cons from read-from-string, got {result:?}");
    };
    crate::emacs_core::value::read_cons(cell).car
}

fn gnu_simple_command_execute_with_eval_expression_eval() -> Evaluator {
    let mut ev = gnu_simple_command_execute_eval();
    load_gnu_eval_expression_into(&mut ev);
    ev
}

fn gnu_simple_universal_argument_eval_all(src: &str) -> Vec<String> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let simple_path = project_root.join("lisp/simple.el");
    let simple_source = fs::read_to_string(&simple_path).expect("read GNU simple.el");

    let mut ev = gnu_simple_command_execute_eval();
    let setup_forms = parse_forms(
        r#"
        (fset 'prefix-command-preserve-state (lambda () nil))
        (fset 'universal-argument--mode (lambda () nil))
        "#,
    )
    .expect("parse universal-argument test stubs");
    ev.eval_forms(&setup_forms);
    eval_first_form_after_marker(&mut ev, &simple_source, "(defun universal-argument ()");
    eval_all_with(&mut ev, src)
}

fn gnu_simple_quoted_insert_eval_all(src: &str) -> Vec<String> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let simple_path = project_root.join("lisp/simple.el");
    let simple_source = fs::read_to_string(&simple_path).expect("read GNU simple.el");

    let mut ev = gnu_simple_command_execute_eval();
    let setup_forms = parse_forms(
        r#"
        (defun cadr (x) (car (cdr x)))
        (defmacro with-no-warnings (&rest body) (cons 'progn body))
        (setq overwrite-mode nil)
        (fset 'read-quoted-char
              (lambda (&rest _args)
                (signal 'end-of-file '("Error reading from stdin"))))
        (fset 'read-char
              (lambda (&rest _args)
                (signal 'end-of-file '("Error reading from stdin"))))
        "#,
    )
    .expect("parse quoted-insert test stubs");
    ev.eval_forms(&setup_forms);
    eval_first_form_after_marker(&mut ev, &simple_source, "(defun quoted-insert (arg)");
    eval_all_with(&mut ev, src)
}

// -------------------------------------------------------------------
// InteractiveSpec
// -------------------------------------------------------------------

#[test]
fn interactive_spec_no_args() {
    let spec = InteractiveSpec::no_args();
    assert!(spec.code.is_empty());
    assert!(spec.prompt.is_none());
}

#[test]
fn interactive_spec_with_code() {
    let spec = InteractiveSpec::new("p");
    assert_eq!(spec.code, "p");
}

#[test]
fn interactive_spec_with_prompt() {
    let spec = InteractiveSpec::new("sEnter name: ");
    assert_eq!(spec.code, "sEnter name: ");
    assert_eq!(spec.prompt.as_deref(), Some("Enter name: "));
}

// -------------------------------------------------------------------
// InteractiveRegistry
// -------------------------------------------------------------------

#[test]
fn registry_register_and_query() {
    let mut reg = InteractiveRegistry::new();
    reg.register_interactive("forward-char", InteractiveSpec::new("p"));
    assert!(reg.is_interactive("forward-char"));
    assert!(!reg.is_interactive("nonexistent"));
}

#[test]
fn registry_get_spec() {
    let mut reg = InteractiveRegistry::new();
    reg.register_interactive("find-file", InteractiveSpec::new("FFind file: "));
    let spec = reg.get_spec("find-file").unwrap();
    assert_eq!(spec.code, "FFind file: ");
}

#[test]
fn registry_interactive_call_stack() {
    let mut reg = InteractiveRegistry::new();
    assert!(!reg.is_called_interactively());

    reg.push_interactive_call(true);
    assert!(reg.is_called_interactively());

    reg.push_interactive_call(false);
    assert!(!reg.is_called_interactively());

    reg.pop_interactive_call();
    assert!(reg.is_called_interactively());

    reg.pop_interactive_call();
    assert!(!reg.is_called_interactively());
}

#[test]
fn registry_this_command_keys() {
    let mut reg = InteractiveRegistry::new();
    assert!(reg.this_command_keys().is_empty());

    reg.set_this_command_keys(vec!["C-x".to_string(), "C-f".to_string()]);
    assert_eq!(reg.this_command_keys(), &["C-x", "C-f"]);
}

#[test]
fn registry_default() {
    let reg = InteractiveRegistry::default();
    assert!(!reg.is_called_interactively());
}

// -------------------------------------------------------------------
// GNU mode-definition macro ownership
// -------------------------------------------------------------------

#[test]
fn mode_definition_macros_start_as_gnu_autoloads() {
    assert!(!crate::emacs_core::subr_info::is_special_form(
        "define-minor-mode"
    ));
    assert!(!crate::emacs_core::subr_info::is_special_form(
        "define-derived-mode"
    ));
    assert!(!crate::emacs_core::subr_info::is_special_form(
        "define-generic-mode"
    ));

    let mut ev = eval_with_ldefs_boot_autoloads(&[
        "define-minor-mode",
        "define-derived-mode",
        "define-generic-mode",
    ]);
    let results = eval_all_with(
        &mut ev,
        r#"(list
             (and (consp (symbol-function 'define-minor-mode))
                  (eq (car (symbol-function 'define-minor-mode)) 'autoload)
                  (eq (get 'define-minor-mode 'autoload-macro) 'expand))
             (and (consp (symbol-function 'define-derived-mode))
                  (eq (car (symbol-function 'define-derived-mode)) 'autoload)
                  (eq (get 'define-derived-mode 'autoload-macro) 'expand))
             (and (consp (symbol-function 'define-generic-mode))
                  (eq (car (symbol-function 'define-generic-mode)) 'autoload)
                  (eq (get 'define-generic-mode 'autoload-macro) 'expand)))"#,
    );
    assert_eq!(results[0], "OK (t t t)");
}

// -------------------------------------------------------------------
// commandp (interactive-aware version)
// -------------------------------------------------------------------

#[test]
fn commandp_non_interactive() {
    let mut ev = Evaluator::new();
    eval_all_with(&mut ev, r#"(defalias 'my-plain-fn #'(lambda () 42))"#);
    let result = builtin_commandp_interactive(&mut ev, vec![Value::symbol("my-plain-fn")]);
    assert!(result.unwrap().is_nil());
}

#[test]
fn commandp_true_for_builtin_ignore() {
    let mut ev = Evaluator::new();
    let result = builtin_commandp_interactive(&mut ev, vec![Value::symbol("ignore")]);
    assert!(result.unwrap().is_truthy());
}

#[test]
fn commandp_true_for_execute_extended_command_from_simple_el() {
    assert_eq!(
        gnu_simple_execute_extended_command_eval_all(
            r#"(list (commandp 'execute-extended-command)
                     (subrp (symbol-function 'execute-extended-command)))"#
        ),
        vec!["OK (t nil)".to_string()]
    );
}

#[test]
fn eval_expression_is_real_lisp_function_after_bootstrap() {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let function = ev
        .obarray
        .symbol_function("eval-expression")
        .expect("missing eval-expression bootstrapped function cell");
    assert!(!crate::emacs_core::autoload::is_autoload_value(&function));
    let result = builtin_commandp_interactive(&mut ev, vec![Value::symbol("eval-expression")])
        .expect("commandp should accept eval-expression");
    assert!(result.is_truthy());
}

#[test]
fn commandp_true_for_defun_with_declare_before_interactive() {
    assert_eq!(
        eval_all(
            r#"(progn
                 (defun neo-declare-interactive ()
                   (declare (interactive-only t))
                   (interactive)
                   'ok)
                 (commandp 'neo-declare-interactive))"#
        ),
        vec!["OK t".to_string()]
    );
}

#[test]
fn commandp_true_for_builtin_forward_char() {
    let mut ev = Evaluator::new();
    let result = builtin_commandp_interactive(&mut ev, vec![Value::symbol("forward-char")]);
    assert!(result.unwrap().is_truthy());
}

#[test]
fn commandp_handles_keyboard_macros_and_bytecode_interactive_slots() {
    let mut ev = Evaluator::new();
    let bytecode = crate::emacs_core::builtins::symbols::make_byte_code_from_parts(
        &Value::Nil,
        &Value::string(""),
        &Value::vector(vec![]),
        &Value::Int(0),
        None,
        Some(&Value::vector(vec![Value::Nil, Value::Nil])),
    )
    .expect("make-byte-code should build commandp fixture");

    assert!(
        builtin_commandp_interactive(&mut ev, vec![Value::string("abc")])
            .expect("string keyboard macro should be accepted")
            .is_truthy()
    );
    assert!(
        builtin_commandp_interactive(&mut ev, vec![Value::vector(vec![Value::Int(1)])])
            .expect("vector keyboard macro should be accepted")
            .is_truthy()
    );
    assert!(
        builtin_commandp_interactive(&mut ev, vec![Value::string("abc"), Value::True])
            .expect("FOR-CALL-INTERACTIVELY should reject strings")
            .is_nil()
    );
    assert!(
        builtin_commandp_interactive(
            &mut ev,
            vec![Value::vector(vec![Value::Int(1)]), Value::True]
        )
        .expect("FOR-CALL-INTERACTIVELY should reject vectors")
        .is_nil()
    );
    assert!(
        builtin_commandp_interactive(&mut ev, vec![bytecode])
            .expect("bytecode with interactive slot should be a command")
            .is_truthy()
    );
}

#[test]
fn commandp_true_for_builtin_editing_commands() {
    let mut ev = Evaluator::new();
    for name in [
        "backward-char",
        "delete-char",
        "insert-char",
        "yank",
        "yank-pop",
        "transpose-chars",
        "transpose-lines",
        "transpose-paragraphs",
        "transpose-sentences",
        "transpose-sexps",
        "upcase-word",
        "downcase-word",
        "capitalize-word",
        "upcase-region",
        "downcase-region",
        "capitalize-region",
        "upcase-initials-region",
        "kill-word",
        "backward-kill-word",
        "kill-region",
        "kill-ring-save",
        "kill-whole-line",
        "copy-region-as-kill",
        "open-line",
        "delete-horizontal-space",
        "just-one-space",
        "delete-indentation",
        "transpose-words",
        "scroll-up",
        "scroll-down",
        "scroll-left",
        "scroll-right",
        "scroll-up-command",
        "scroll-down-command",
        "recenter",
        "move-beginning-of-line",
        "move-end-of-line",
    ] {
        let result = builtin_commandp_interactive(&mut ev, vec![Value::symbol(name)])
            .expect("commandp call");
        assert!(result.is_truthy(), "expected commandp true for {name}");
    }
}

#[test]
fn abbrev_mode_is_real_lisp_function_after_bootstrap() {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let function = ev
        .obarray
        .symbol_function("abbrev-mode")
        .expect("missing abbrev-mode bootstrapped function cell");
    assert!(!crate::emacs_core::autoload::is_autoload_value(function));
    let command = builtin_commandp_interactive(&mut ev, vec![Value::symbol("abbrev-mode")])
        .expect("commandp should accept abbrev-mode");
    assert!(command.is_truthy());
}

#[test]
fn bookmark_commands_startup_are_autoloaded() {
    let names = [
        "bookmark-delete",
        "bookmark-jump",
        "bookmark-load",
        "bookmark-rename",
        "bookmark-save",
        "bookmark-set",
    ];
    let ev = eval_with_ldefs_boot_autoloads(&names);
    for name in names {
        let function = ev
            .obarray
            .symbol_function(name)
            .unwrap_or_else(|| panic!("missing {name} startup function cell"));
        assert!(
            crate::emacs_core::autoload::is_autoload_value(function),
            "expected {name} startup function cell to be a GNU autoload"
        );
    }
}

#[test]
fn rectangle_commands_startup_are_autoloaded() {
    let names = [
        "clear-rectangle",
        "delete-rectangle",
        "kill-rectangle",
        "open-rectangle",
        "string-rectangle",
        "yank-rectangle",
    ];
    let ev = eval_with_ldefs_boot_autoloads(&names);
    for name in names {
        let function = ev
            .obarray
            .symbol_function(name)
            .unwrap_or_else(|| panic!("missing {name} startup function cell"));
        assert!(
            crate::emacs_core::autoload::is_autoload_value(function),
            "expected {name} startup function cell to be a GNU autoload"
        );
    }
}

#[test]
fn simple_commands_are_real_lisp_functions_after_bootstrap() {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    for name in [
        "exchange-point-and-mark",
        "list-processes",
        "process-menu-delete-process",
        "process-menu-mode",
    ] {
        let function = ev
            .obarray
            .symbol_function(name)
            .unwrap_or_else(|| panic!("missing {name} startup function cell"));
        assert!(
            !crate::emacs_core::autoload::is_autoload_value(function),
            "expected {name} startup function cell to be loaded, not an autoload"
        );
        let command = builtin_commandp_interactive(&mut ev, vec![Value::symbol(name)])
            .unwrap_or_else(|err| panic!("commandp should accept {name}: {err:?}"));
        assert!(command.is_truthy(), "expected commandp true for {name}");
    }
}

#[test]
fn replace_commands_are_real_lisp_functions_after_bootstrap() {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    for name in [
        "flush-lines",
        "how-many",
        "keep-lines",
        "query-replace",
        "query-replace-regexp",
        "replace-regexp",
        "replace-string",
    ] {
        let function = ev
            .obarray
            .symbol_function(name)
            .unwrap_or_else(|| panic!("missing {name} startup function cell"));
        assert!(
            !crate::emacs_core::autoload::is_autoload_value(function),
            "expected {name} startup function cell to be loaded, not an autoload"
        );
        let command = builtin_commandp_interactive(&mut ev, vec![Value::symbol(name)])
            .unwrap_or_else(|err| panic!("commandp should accept {name}: {err:?}"));
        assert!(command.is_truthy(), "expected commandp true for {name}");
    }
}

#[test]
fn subr_key_binding_commands_are_real_lisp_functions_after_bootstrap() {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    for name in ["global-set-key", "local-set-key"] {
        let function = ev
            .obarray
            .symbol_function(name)
            .unwrap_or_else(|| panic!("missing {name} startup function cell"));
        assert!(
            !crate::emacs_core::autoload::is_autoload_value(function),
            "expected {name} startup function cell to be loaded, not an autoload"
        );
        let command = builtin_commandp_interactive(&mut ev, vec![Value::symbol(name)])
            .unwrap_or_else(|err| panic!("commandp should accept {name}: {err:?}"));
        assert!(command.is_truthy(), "expected commandp true for {name}");
    }
}

#[test]
fn env_command_is_real_lisp_function_after_bootstrap() {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let function = ev
        .obarray
        .symbol_function("setenv")
        .expect("missing setenv bootstrapped function cell");
    assert!(!crate::emacs_core::autoload::is_autoload_value(function));
    let command = builtin_commandp_interactive(&mut ev, vec![Value::symbol("setenv")])
        .expect("commandp should accept setenv");
    assert!(command.is_truthy());
}

#[test]
fn files_command_is_real_lisp_function_after_bootstrap() {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let function = ev
        .obarray
        .symbol_function("load-file")
        .expect("missing load-file bootstrapped function cell");
    assert!(!crate::emacs_core::autoload::is_autoload_value(function));
    let command = builtin_commandp_interactive(&mut ev, vec![Value::symbol("load-file")])
        .expect("commandp should accept load-file");
    assert!(command.is_truthy());
}

#[test]
fn regexp_search_aliases_are_available_after_bootstrap() {
    // search-forward-regexp and search-backward-regexp are defalias'd to
    // re-search-forward / re-search-backward in subr.el (not autoloaded).
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    for name in ["search-forward-regexp", "search-backward-regexp"] {
        let function = ev
            .obarray
            .symbol_function(name)
            .unwrap_or_else(|| panic!("missing {name} startup function cell"));
        assert!(
            !crate::emacs_core::autoload::is_autoload_value(function),
            "expected {name} to be a resolved function (defalias), not an autoload"
        );
    }
}

#[test]
fn upcase_char_startup_is_autoloaded() {
    let ev = eval_with_ldefs_boot_autoloads(&["upcase-char"]);
    let function = ev
        .obarray
        .symbol_function("upcase-char")
        .expect("missing upcase-char startup function cell");
    assert!(crate::emacs_core::autoload::is_autoload_value(function));
}

#[test]
fn mode_and_mark_commands_are_real_lisp_functions_after_bootstrap() {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    for name in ["auto-composition-mode", "set-mark-command"] {
        let function = ev
            .obarray
            .symbol_function(name)
            .unwrap_or_else(|| panic!("missing {name} startup function cell"));
        assert!(
            !crate::emacs_core::autoload::is_autoload_value(function),
            "expected {name} startup function cell to be loaded, not an autoload"
        );
        let command = builtin_commandp_interactive(&mut ev, vec![Value::symbol(name)])
            .unwrap_or_else(|err| panic!("commandp should accept {name}: {err:?}"));
        assert!(command.is_truthy(), "expected commandp true for {name}");
    }
}

#[test]
fn count_matches_is_real_lisp_function_after_bootstrap() {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let function = ev
        .obarray
        .symbol_function("count-matches")
        .expect("missing count-matches bootstrapped function cell");
    assert!(!crate::emacs_core::autoload::is_autoload_value(function));
    let command = builtin_commandp_interactive(&mut ev, vec![Value::symbol("count-matches")])
        .expect("commandp call");
    assert!(command.is_truthy());
}

#[test]
fn kmacro_name_last_macro_startup_is_autoloaded() {
    // kmacro-name-last-macro has a real autoload entry in ldefs-boot.el.
    // name-last-kbd-macro is a defalias to it (not an autoload form), so it
    // is only available after the defalias in ldefs-boot.el is evaluated
    // during bootstrap -- not via the autoload-only loader.
    let mut ev = eval_with_ldefs_boot_autoloads(&["kmacro-name-last-macro"]);
    let function = ev
        .obarray
        .symbol_function("kmacro-name-last-macro")
        .expect("missing kmacro-name-last-macro startup function cell");
    assert!(
        crate::emacs_core::autoload::is_autoload_value(function),
        "expected kmacro-name-last-macro startup function cell to be a GNU autoload"
    );
    let command =
        builtin_commandp_interactive(&mut ev, vec![Value::symbol("kmacro-name-last-macro")])
            .expect("commandp should accept kmacro-name-last-macro");
    assert!(
        command.is_truthy(),
        "expected commandp true for kmacro-name-last-macro"
    );
}

#[test]
fn remove_hook_is_available_after_bootstrap() {
    // remove-hook is a defun in subr.el, not autoloaded in GNU Emacs.
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let function = ev
        .obarray
        .symbol_function("remove-hook")
        .expect("missing remove-hook startup function cell");
    assert!(
        !crate::emacs_core::autoload::is_autoload_value(function),
        "expected remove-hook to be a resolved function, not an autoload"
    );
}

#[test]
fn commandp_true_for_additional_builtin_commands() {
    let mut ev = Evaluator::new();
    for name in [
        "base64-decode-region",
        "base64-encode-region",
        "base64url-encode-region",
        "decode-coding-region",
        "display-buffer",
        "encode-coding-region",
        "eval-buffer",
        "forward-sexp",
        "gui-set-selection",
        "goto-char",
        "isearch-forward",
        "iconify-frame",
        "kill-emacs",
        "lower-frame",
        "make-directory",
        "make-frame-invisible",
        "make-frame-visible",
        "make-indirect-buffer",
        "open-dribble-file",
        "raise-frame",
        "re-search-forward",
        "redirect-debugging-output",
        "rename-buffer",
        "select-frame",
        "set-buffer-process-coding-system",
        "transpose-regions",
        "kill-process",
        "signal-process",
        "suspend-emacs",
        "top-level",
        "unix-sync",
        "write-region",
        "x-menu-bar-open-internal",
    ] {
        let result = builtin_commandp_interactive(&mut ev, vec![Value::symbol(name)])
            .expect("commandp call");
        assert!(result.is_truthy(), "expected commandp true for {name}");
    }
}

#[test]
fn commandp_true_for_loaded_lisp_commands_after_bootstrap() {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    for name in [
        "newline-and-indent",
        "recenter-top-bottom",
        "replace-buffer-contents",
        "tab-to-tab-stop",
        "transient-mark-mode",
    ] {
        let result = builtin_commandp_interactive(&mut ev, vec![Value::symbol(name)])
            .expect("commandp call");
        assert!(result.is_truthy(), "expected commandp true for {name}");
    }
}

#[test]
fn commandp_false_for_noninteractive_builtin() {
    let mut ev = Evaluator::new();
    let result = builtin_commandp_interactive(&mut ev, vec![Value::symbol("car")]);
    assert!(result.unwrap().is_nil());
}

#[test]
fn commandp_rejects_overflow_arity() {
    let mut ev = Evaluator::new();
    let result = builtin_commandp_interactive(
        &mut ev,
        vec![Value::symbol("ignore"), Value::Nil, Value::Nil],
    )
    .expect_err("commandp should reject more than two arguments");
    match result {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn commandp_resolves_aliases_and_symbol_designators() {
    let mut ev = Evaluator::new();
    // Register forward-char as an interactive command for testing.
    ev.interactive
        .register_interactive("forward-char", InteractiveSpec::new("p"));
    ev.obarray
        .set_symbol_function("t", Value::symbol("forward-char"));
    ev.obarray
        .set_symbol_function(":vm-command-alias-keyword", Value::symbol("forward-char"));
    ev.obarray
        .set_symbol_function("vm-command-alias", Value::True);
    ev.obarray.set_symbol_function(
        "vm-command-alias-keyword",
        Value::keyword(":vm-command-alias-keyword"),
    );

    let t_result = builtin_commandp_interactive(&mut ev, vec![Value::True]);
    assert!(t_result.unwrap().is_truthy());
    let keyword_result =
        builtin_commandp_interactive(&mut ev, vec![Value::keyword(":vm-command-alias-keyword")]);
    assert!(keyword_result.unwrap().is_truthy());
    let alias_result =
        builtin_commandp_interactive(&mut ev, vec![Value::symbol("vm-command-alias")]);
    assert!(alias_result.unwrap().is_truthy());
    let keyword_alias_result =
        builtin_commandp_interactive(&mut ev, vec![Value::symbol("vm-command-alias-keyword")]);
    assert!(keyword_alias_result.unwrap().is_truthy());
}

#[test]
fn commandp_true_for_lambda_with_interactive_form() {
    let mut ev = Evaluator::new();
    let lambda = eval_all_with(&mut ev, "(lambda () (interactive) 1)");
    let parsed = super::super::parser::parse_forms("(lambda () (interactive) 1)")
        .expect("lambda form should parse");
    let value = ev.eval(&parsed[0]).expect("lambda form should evaluate");
    assert_eq!(lambda[0], "OK (lambda nil (interactive) 1)");
    let result = builtin_commandp_interactive(&mut ev, vec![value]);
    assert!(result.unwrap().is_truthy());
}

#[test]
fn commandp_true_for_quoted_lambda_with_interactive_form() {
    let mut ev = Evaluator::new();
    let forms = super::super::parser::parse_forms("'(lambda () \"doc\" (interactive) 1)")
        .expect("quoted lambda form should parse");
    let quoted_lambda = ev.eval(&forms[0]).expect("quoted lambda should evaluate");
    let result = builtin_commandp_interactive(&mut ev, vec![quoted_lambda]);
    assert!(result.unwrap().is_truthy());
}

#[test]
fn call_interactively_state_resolution_handles_default_and_noarg_cases() {
    let mut ev = Evaluator::new();
    ev.obarray
        .set_symbol_value("current-prefix-arg", Value::list(vec![Value::Int(4)]));

    let mut builtin_plan = plan_call_interactively_in_state(
        ev.obarray(),
        &ev.interactive,
        ev.read_command_keys(),
        &[Value::symbol("forward-char")],
    )
    .expect("plan builtin default interactive command");
    let (_, builtin_args) = resolve_call_interactively_target_and_args_in_state(
        &mut ev.obarray,
        &mut ev.dynamic,
        &mut ev.buffers,
        &ev.custom,
        &ev.frames,
        &ev.interactive,
        &mut builtin_plan,
    )
    .expect("resolve builtin default args")
    .expect("shared-state builtin default path");
    assert_eq!(builtin_args, vec![Value::Int(4)]);

    let lambda_forms =
        super::super::parser::parse_forms("(lambda () (interactive) 1)").expect("parse lambda");
    let lambda = ev.eval(&lambda_forms[0]).expect("eval lambda");
    let mut lambda_plan = plan_call_interactively_in_state(
        ev.obarray(),
        &ev.interactive,
        ev.read_command_keys(),
        &[lambda],
    )
    .expect("plan interactive lambda");
    let (_, lambda_args) = resolve_call_interactively_target_and_args_in_state(
        &mut ev.obarray,
        &mut ev.dynamic,
        &mut ev.buffers,
        &ev.custom,
        &ev.frames,
        &ev.interactive,
        &mut lambda_plan,
    )
    .expect("resolve lambda args")
    .expect("shared-state no-arg lambda path");
    assert!(lambda_args.is_empty());
}

#[test]
fn call_interactively_state_resolution_defers_prompting_specs_to_eval() {
    let mut ev = Evaluator::new();
    let lambda_forms =
        super::super::parser::parse_forms("(lambda (x) (interactive \"sPrompt: \") x)")
            .expect("parse prompting lambda");
    let lambda = ev.eval(&lambda_forms[0]).expect("eval prompting lambda");
    let mut plan = plan_call_interactively_in_state(
        ev.obarray(),
        &ev.interactive,
        ev.read_command_keys(),
        &[lambda],
    )
    .expect("plan prompting lambda");
    let resolved = resolve_call_interactively_target_and_args_in_state(
        &mut ev.obarray,
        &mut ev.dynamic,
        &mut ev.buffers,
        &ev.custom,
        &ev.frames,
        &ev.interactive,
        &mut plan,
    )
    .expect("resolve prompting lambda");
    assert!(resolved.is_none());
}

#[test]
fn call_interactively_state_resolution_handles_simple_string_codes_without_eval() {
    let mut ev = Evaluator::new();
    ev.obarray
        .set_symbol_value("current-prefix-arg", Value::list(vec![Value::Int(4)]));
    let current = ev.buffers.current_buffer_id().expect("current buffer");
    let _ = ev.buffers.replace_buffer_contents(current, "abcd");
    let _ = ev.buffers.goto_buffer_byte(current, 2);
    let _ = ev.buffers.set_buffer_mark(current, 1);

    let evt_forms = super::super::parser::parse_forms(
        "(list 'mouse-1 (list (list (selected-window) (point) '(0 . 0) 0)))",
    )
    .expect("parse event");
    let event = ev.eval(&evt_forms[0]).expect("eval event");
    let lambda_forms = super::super::parser::parse_forms(
        "(lambda (raw num pt mk beg end evt up ignored)
           (interactive \"P
p
d
m
r
e
U
i\")
           (list raw num pt mk beg end evt up ignored))",
    )
    .expect("parse lambda");
    let lambda = ev.eval(&lambda_forms[0]).expect("eval lambda");
    let mut plan = plan_call_interactively_in_state(
        ev.obarray(),
        &ev.interactive,
        ev.read_command_keys(),
        &[lambda, Value::Nil, Value::vector(vec![event])],
    )
    .expect("plan simple string-code lambda");
    let (_, args) = resolve_call_interactively_target_and_args_in_state(
        &mut ev.obarray,
        &mut ev.dynamic,
        &mut ev.buffers,
        &ev.custom,
        &ev.frames,
        &ev.interactive,
        &mut plan,
    )
    .expect("resolve simple string-code args")
    .expect("shared-state simple string-code path");
    assert_eq!(
        args,
        vec![
            Value::list(vec![Value::Int(4)]),
            Value::Int(4),
            Value::Int(3),
            Value::Int(2),
            Value::Int(2),
            Value::Int(3),
            event,
            Value::Nil,
            Value::Nil,
        ]
    );
}

#[test]
fn call_interactively_state_resolution_applies_shift_selection_prefix_in_state() {
    let mut ev = Evaluator::new();
    let current = ev.buffers.current_buffer_id().expect("current buffer");
    let _ = ev.buffers.replace_buffer_contents(current, "abcd");
    let _ = ev.buffers.goto_buffer_byte(current, 2);
    ev.obarray
        .set_symbol_value("this-command-keys-shift-translated", Value::True);
    ev.obarray
        .set_symbol_value("shift-select-mode", Value::True);

    let lambda_forms = super::super::parser::parse_forms("(lambda (pt) (interactive \"^d\") pt)")
        .expect("parse lambda");
    let lambda = ev.eval(&lambda_forms[0]).expect("eval lambda");
    let mut plan = plan_call_interactively_in_state(
        ev.obarray(),
        &ev.interactive,
        ev.read_command_keys(),
        &[lambda],
    )
    .expect("plan shift-selection lambda");
    let (_, args) = resolve_call_interactively_target_and_args_in_state(
        &mut ev.obarray,
        &mut ev.dynamic,
        &mut ev.buffers,
        &ev.custom,
        &ev.frames,
        &ev.interactive,
        &mut plan,
    )
    .expect("resolve shift-selection args")
    .expect("shared-state shift-selection path");
    assert_eq!(args, vec![Value::Int(3)]);

    let buf = ev.buffers.current_buffer().expect("current buffer");
    assert_eq!(buf.mark(), Some(2));
    assert_eq!(buf.get_buffer_local("mark-active"), Some(&Value::True));
}

#[test]
fn call_interactively_state_resolution_handles_optional_coding_without_prefix() {
    let mut ev = Evaluator::new();
    let lambda_forms =
        super::super::parser::parse_forms("(lambda (coding) (interactive \"ZCoding: \") coding)")
            .expect("parse lambda");
    let lambda = ev.eval(&lambda_forms[0]).expect("eval lambda");
    let mut plan = plan_call_interactively_in_state(
        ev.obarray(),
        &ev.interactive,
        ev.read_command_keys(),
        &[lambda],
    )
    .expect("plan optional coding lambda");
    let (_, args) = resolve_call_interactively_target_and_args_in_state(
        &mut ev.obarray,
        &mut ev.dynamic,
        &mut ev.buffers,
        &ev.custom,
        &ev.frames,
        &ev.interactive,
        &mut plan,
    )
    .expect("resolve optional coding args")
    .expect("shared-state optional coding path");
    assert_eq!(args, vec![Value::Nil]);
}

#[test]
fn interactive_lambda_r_capital_spec_uses_use_region_p_semantics() {
    let mut ev = Evaluator::new();
    let current = ev.buffers.current_buffer_id().expect("current buffer");
    let _ = ev.buffers.replace_buffer_contents(current, "abcd");
    let _ = ev.buffers.goto_buffer_byte(current, 2);
    let _ = ev.buffers.set_buffer_mark(current, 1);

    let mut context = InteractiveInvocationContext::default();
    let _ = ev.eval_forms(
        &parse_forms("(fset 'use-region-p (lambda () nil))").expect("parse use-region-p"),
    );
    let args = interactive_args_from_string_code(
        &mut ev,
        "R",
        CommandInvocationKind::CallInteractively,
        &mut context,
    )
    .expect("resolve inactive R")
    .expect("R should produce args");
    assert_eq!(args, vec![Value::Nil, Value::Nil]);

    let _ = ev.eval_forms(
        &parse_forms("(fset 'use-region-p (lambda () t))").expect("parse use-region-p"),
    );
    let args = interactive_args_from_string_code(
        &mut ev,
        "R",
        CommandInvocationKind::CallInteractively,
        &mut context,
    )
    .expect("resolve active R")
    .expect("R should produce args");
    assert_eq!(args, vec![Value::Int(2), Value::Int(3)]);
}

// -------------------------------------------------------------------
// interactive-p / called-interactively-p
// -------------------------------------------------------------------

#[test]
fn interactive_p_false_by_default() {
    let mut ev = Evaluator::new();
    let result = builtin_interactive_p(&mut ev, vec![]);
    assert!(result.unwrap().is_nil());
}

#[test]
fn interactive_p_nil_when_interactive() {
    let mut ev = Evaluator::new();
    ev.interactive.push_interactive_call(true);
    let result = builtin_interactive_p(&mut ev, vec![]);
    ev.interactive.pop_interactive_call();
    assert!(result.unwrap().is_nil());
}

#[test]
fn called_interactively_p_false_by_default() {
    let mut ev = Evaluator::new();
    let result = builtin_called_interactively_p(&mut ev, vec![]);
    assert!(result.unwrap().is_nil());
}

#[test]
fn called_interactively_p_with_kind() {
    let mut ev = Evaluator::new();
    let result = builtin_called_interactively_p(&mut ev, vec![Value::symbol("any")]);
    assert!(result.unwrap().is_nil());
}

#[test]
fn called_interactively_p_kind_interactive_is_nil_when_interactive() {
    let mut ev = Evaluator::new();
    ev.interactive.push_interactive_call(true);
    let result = builtin_called_interactively_p(&mut ev, vec![Value::symbol("interactive")]);
    ev.interactive.pop_interactive_call();
    assert!(result.unwrap().is_nil());
}

#[test]
fn called_interactively_p_kind_any_is_t_when_interactive() {
    let mut ev = Evaluator::new();
    ev.interactive.push_interactive_call(true);
    let result = builtin_called_interactively_p(&mut ev, vec![Value::symbol("any")]);
    ev.interactive.pop_interactive_call();
    assert!(result.unwrap().is_truthy());
}

#[test]
fn called_interactively_p_unknown_kind_is_t_when_interactive() {
    let mut ev = Evaluator::new();
    ev.interactive.push_interactive_call(true);
    let result = builtin_called_interactively_p(&mut ev, vec![Value::symbol("foo")]);
    ev.interactive.pop_interactive_call();
    assert!(result.unwrap().is_truthy());
}

#[test]
fn called_interactively_p_too_many_args() {
    let mut ev = Evaluator::new();
    let result =
        builtin_called_interactively_p(&mut ev, vec![Value::symbol("any"), Value::symbol("extra")]);
    assert!(result.is_err());
}

// -------------------------------------------------------------------
// this-command-keys / this-command-keys-vector
// -------------------------------------------------------------------

#[test]
fn this_command_keys_empty() {
    let mut ev = Evaluator::new();
    let result = builtin_this_command_keys(&mut ev, vec![]).unwrap();
    assert_eq!(result.as_str(), Some(""));
}

#[test]
fn this_command_keys_after_set() {
    let mut ev = Evaluator::new();
    ev.interactive
        .set_this_command_keys(vec!["C-x".to_string(), "C-f".to_string()]);
    let result = builtin_this_command_keys(&mut ev, vec![]).unwrap();
    assert_eq!(result.as_str(), Some("C-x C-f"));
}

#[test]
fn this_command_keys_vector_empty() {
    let mut ev = Evaluator::new();
    let result = builtin_this_command_keys_vector(&mut ev, vec![]).unwrap();
    assert!(matches!(result, Value::Vector(_)));
}

#[test]
fn this_command_keys_vector_after_set() {
    let mut ev = Evaluator::new();
    ev.interactive
        .set_this_command_keys(vec!["M-x".to_string()]);
    let result = builtin_this_command_keys_vector(&mut ev, vec![]).unwrap();
    if let Value::Vector(v) = result {
        let v = with_heap(|h| h.get_vector(v).clone());
        assert_eq!(v.len(), 1);
    } else {
        panic!("expected vector");
    }
}

#[test]
fn this_command_keys_prefers_read_command_key_chars() {
    let mut ev = Evaluator::new();
    ev.interactive
        .set_this_command_keys(vec!["C-x".to_string(), "C-f".to_string()]);
    ev.set_read_command_keys(vec![Value::Int(97)]);

    let text = builtin_this_command_keys(&mut ev, vec![]).unwrap();
    assert_eq!(text.as_str(), Some("a"));

    let vec_result = builtin_this_command_keys_vector(&mut ev, vec![]).unwrap();
    match vec_result {
        Value::Vector(v) => {
            let items = with_heap(|h| h.get_vector(v).clone());
            assert_eq!(items.as_slice(), &[Value::Int(97)]);
        }
        other => panic!("expected vector, got {other:?}"),
    }
}

#[test]
fn this_command_keys_returns_vector_for_non_char_read_command_keys() {
    let mut ev = Evaluator::new();
    ev.set_read_command_keys(vec![Value::list(vec![Value::symbol("mouse-1")])]);

    let result = builtin_this_command_keys(&mut ev, vec![]).unwrap();
    match result {
        Value::Vector(v) => {
            let items = with_heap(|h| h.get_vector(v).clone());
            assert_eq!(items.len(), 1);
            assert!(matches!(items[0], Value::Cons(_)));
        }
        other => panic!("expected vector, got {other:?}"),
    }
}

#[test]
fn this_single_command_keys_prefers_read_command_key_vector() {
    let mut ev = Evaluator::new();
    ev.set_read_command_keys(vec![Value::Int(97)]);

    let result = builtin_this_single_command_keys(&mut ev, vec![]).unwrap();
    match result {
        Value::Vector(v) => {
            let items = with_heap(|h| h.get_vector(v).clone());
            assert_eq!(items.as_slice(), &[Value::Int(97)]);
        }
        other => panic!("expected vector, got {other:?}"),
    }
}

#[test]
fn this_single_command_keys_falls_back_to_interactive_descriptions() {
    let mut ev = Evaluator::new();
    ev.interactive
        .set_this_command_keys(vec!["C-x".to_string(), "C-f".to_string()]);

    let result = builtin_this_single_command_keys(&mut ev, vec![]).unwrap();
    match result {
        Value::Vector(v) => {
            let items = with_heap(|h| h.get_vector(v).clone());
            assert_eq!(items.as_slice(), &[Value::Int(24), Value::Int(6)]);
        }
        other => panic!("expected vector, got {other:?}"),
    }
}

#[test]
fn clear_this_command_keys_clears_read_key_context() {
    let mut ev = Evaluator::new();
    ev.set_read_command_keys(vec![Value::Int(97)]);

    let result = builtin_clear_this_command_keys(&mut ev, vec![]).unwrap();
    assert!(result.is_nil());
    assert_eq!(ev.read_command_keys(), &[]);

    let vec_result = builtin_this_command_keys_vector(&mut ev, vec![]).unwrap();
    match vec_result {
        Value::Vector(v) => {
            let items = with_heap(|h| h.get_vector(v).clone());
            assert!(items.is_empty());
        }
        other => panic!("expected vector, got {other:?}"),
    }
}

#[test]
fn clear_this_command_keys_clears_interactive_fallback_context() {
    let mut ev = Evaluator::new();
    ev.interactive
        .set_this_command_keys(vec!["C-x".to_string(), "C-f".to_string()]);

    let result = builtin_clear_this_command_keys(&mut ev, vec![Value::Int(1)]).unwrap();
    assert!(result.is_nil());

    let keys = builtin_this_command_keys(&mut ev, vec![]).unwrap();
    assert_eq!(keys.as_str(), Some(""));
}

#[test]
fn clear_this_command_keys_without_keep_record_clears_recent_input_history() {
    let mut ev = Evaluator::new();
    ev.record_input_event(Value::Int(97));
    assert_eq!(ev.recent_input_events(), &[Value::Int(97)]);

    let result = builtin_clear_this_command_keys(&mut ev, vec![]).unwrap();
    assert!(result.is_nil());
    assert!(ev.recent_input_events().is_empty());
}

#[test]
fn clear_this_command_keys_with_nil_keep_record_clears_recent_input_history() {
    let mut ev = Evaluator::new();
    ev.record_input_event(Value::Int(98));
    assert_eq!(ev.recent_input_events(), &[Value::Int(98)]);

    let result = builtin_clear_this_command_keys(&mut ev, vec![Value::Nil]).unwrap();
    assert!(result.is_nil());
    assert!(ev.recent_input_events().is_empty());
}

#[test]
fn clear_this_command_keys_with_keep_record_preserves_recent_input_history() {
    let mut ev = Evaluator::new();
    ev.record_input_event(Value::Int(99));
    assert_eq!(ev.recent_input_events(), &[Value::Int(99)]);

    let result = builtin_clear_this_command_keys(&mut ev, vec![Value::symbol("t")]).unwrap();
    assert!(result.is_nil());
    assert_eq!(ev.recent_input_events(), &[Value::Int(99)]);
}

#[test]
fn clear_this_command_keys_rejects_more_than_one_arg() {
    let mut ev = Evaluator::new();
    let result = builtin_clear_this_command_keys(&mut ev, vec![Value::Int(1), Value::Int(2)]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-number-of-arguments"
                && sig.data
                    == vec![Value::symbol("clear-this-command-keys"), Value::Int(2)]
    ));
}

// -------------------------------------------------------------------
// key-binding / local-key-binding / global-key-binding
// -------------------------------------------------------------------

#[test]
fn key_binding_global() {
    let mut ev = Evaluator::new();
    let km = make_list_keymap();
    ev.obarray.set_symbol_value("global-map", km);
    // ctrl-f = char 6
    let ctrl_f = Value::Int(6);
    crate::emacs_core::keymap::list_keymap_define(km, ctrl_f, Value::symbol("forward-char"));

    let result = builtin_key_binding(&mut ev, vec![Value::string("\x06")]).unwrap();
    assert_eq!(result.as_symbol_name(), Some("forward-char"));
}

#[test]
fn key_binding_prefers_minor_and_emulation_mode_maps() {
    assert_eq!(
        eval_one(
            r#"(let ((g (make-sparse-keymap))
                     (l (make-sparse-keymap))
                     (m (make-sparse-keymap))
                     (minor-mode-map-alist nil)
                 (demo-mode t))
                 (use-global-map g)
                 (use-local-map l)
                 (define-key m "\x01" 'forward-char)
                 (define-key l "\x01" 'self-insert-command)
                 (setq minor-mode-map-alist (list (cons 'demo-mode m)))
                 (key-binding "\x01"))"#
        ),
        "OK forward-char"
    );
    assert_eq!(
        eval_one(
            r#"(let ((g (make-sparse-keymap))
                     (m-minor (make-sparse-keymap))
                     (m-emu (make-sparse-keymap))
                     (minor-mode-map-alist nil)
                 (emulation-mode-map-alists nil)
                 (minor-mode t)
                 (emu-mode t))
                 (use-global-map g)
                 (define-key m-minor "\x01" 'self-insert-command)
                 (define-key m-emu "\x01" 'forward-char)
                 (setq minor-mode-map-alist (list (cons 'minor-mode m-minor)))
                 (setq emulation-mode-map-alists (list (list (cons 'emu-mode m-emu))))
                 (key-binding "\x01"))"#
        ),
        "OK forward-char"
    );
}

#[test]
fn key_binding_ignores_invalid_active_minor_emulation_entries() {
    assert_eq!(
        eval_one(
            r#"(let ((g (make-sparse-keymap))
                     (minor-mode-map-alist '((demo-mode . 999999)))
                     (demo-mode t))
                 (use-global-map g)
                 (define-key g "\x01" 'self-insert-command)
                 (key-binding "\x01"))"#
        ),
        "OK self-insert-command"
    );
    assert_eq!(
        eval_one(
            r#"(let ((g (make-sparse-keymap))
                     (emulation-mode-map-alists (list (list (cons 'demo-mode 999999))))
                     (demo-mode t))
                 (use-global-map g)
                 (define-key g "\x01" 'self-insert-command)
                 (key-binding "\x01"))"#
        ),
        "OK self-insert-command"
    );
}

#[test]
fn key_binding_applies_command_remapping_unless_no_remap() {
    assert_eq!(
        eval_one(
            r#"(let ((g (make-sparse-keymap)))
                 (use-global-map g)
                 (define-key g "a" 'self-insert-command)
                 (define-key g [remap self-insert-command] 'forward-char)
                 (list (key-binding "a")
                       (key-binding "a" t nil)
                       (key-binding "a" t t)))"#
        ),
        "OK (forward-char forward-char self-insert-command)"
    );
    assert_eq!(
        eval_one(
            r#"(let ((g (make-sparse-keymap)))
                 (use-global-map g)
                 (define-key g "a" 'self-insert-command)
                 (define-key g [remap self-insert-command] t)
                 (key-binding "a"))"#
        ),
        "OK self-insert-command"
    );
}

#[test]
fn key_binding_unbound() {
    let mut ev = Evaluator::new();
    let result = builtin_key_binding(&mut ev, vec![Value::string("\x1a")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn key_binding_empty_returns_keymap_list() {
    assert_eq!(
        eval_one(r#"(let ((m (key-binding ""))) (and (consp m) (keymapp (car m))))"#),
        "OK t"
    );
}

#[test]
fn key_binding_empty_vector_is_nil() {
    assert_eq!(eval_one(r#"(key-binding [])"#), "OK nil");
}

#[test]
fn key_binding_default_plain_char_self_insert() {
    let mut ev = Evaluator::new();
    let result = builtin_key_binding(&mut ev, vec![Value::string("a")]).unwrap();
    assert_eq!(result.as_symbol_name(), Some("self-insert-command"));
}

#[test]
fn key_binding_too_many_args_errors() {
    let mut ev = Evaluator::new();
    let result = builtin_key_binding(
        &mut ev,
        vec![
            Value::string("\x03"),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
        ],
    );
    assert!(result.is_err());
}

#[test]
fn key_binding_integer_position_out_of_range_signals_args_out_of_range() {
    assert_eq!(
        eval_one(
            r#"(with-temp-buffer
                 (let ((g (make-sparse-keymap)))
                   (use-global-map g)
                   (define-key g "a" 'forward-char)
                   (let ((err (condition-case e
                                  (key-binding "a" t nil 0)
                                (error e))))
                     (list (car err) (bufferp (nth 1 err)) (nth 2 err)))))"#
        ),
        "OK (args-out-of-range t 0)"
    );
    assert_eq!(
        eval_one(
            r#"(with-temp-buffer
                 (let ((g (make-sparse-keymap)))
                   (use-global-map g)
                   (define-key g "a" 'forward-char)
                   (let ((err (condition-case e
                                  (key-binding "a" t nil 2)
                                (error e))))
                     (list (car err) (bufferp (nth 1 err)) (nth 2 err)))))"#
        ),
        "OK (args-out-of-range t 2)"
    );
}

#[test]
fn key_binding_non_integer_position_is_accepted_and_ignored() {
    assert_eq!(
        eval_one(
            r#"(with-temp-buffer
                 (let ((g (make-sparse-keymap)))
                   (use-global-map g)
                   (define-key g "a" 'forward-char)
                   (list
                    (key-binding "a" t nil 1)
                    (key-binding "a" t nil t)
                    (key-binding "a" t nil 'foo)
                    (key-binding "a" t nil "x")
                    (key-binding "a" t nil [1])
                    (key-binding "a" t nil '(1))
                    (key-binding "a" t nil 1.5)
                    (key-binding "a" t nil (copy-marker (point))))))"#
        ),
        "OK (forward-char forward-char forward-char forward-char forward-char forward-char forward-char forward-char)"
    );
}

#[test]
fn global_key_binding_bootstrap_matches_subr_el() {
    assert_eq!(
        gnu_subr_keymap_eval_all(
            r#"(list (subrp (symbol-function 'global-key-binding))
                     (keymapp (global-key-binding ""))
                     (global-key-binding "\ex")
                     (global-key-binding "a")
                     (global-key-binding "ab"))"#
        ),
        vec!["OK (nil t execute-extended-command self-insert-command 1)".to_string()]
    );
}

#[test]
fn global_key_binding_bootstrap_wrong_arity_matches_lisp() {
    assert_eq!(
        gnu_subr_keymap_eval_all(
            r#"(let ((err (condition-case e
                             (global-key-binding "\x03" nil nil)
                           (error e))))
                 (car err))"#
        ),
        vec!["OK wrong-number-of-arguments".to_string()]
    );
}

#[test]
fn key_binding_and_lookup_key_follow_meta_prefix_char() {
    assert_eq!(
        eval_one(
            r#"(let ((g (make-sparse-keymap))
                     (esc (make-sparse-keymap)))
                 (define-key esc "x" 'execute-extended-command)
                 (define-key g "\e" esc)
                 (use-global-map g)
                 (list (key-binding [134217848])
                       (lookup-key g [134217848])))"#
        ),
        "OK (execute-extended-command execute-extended-command)"
    );
}

#[test]
fn key_binding_and_lookup_key_follow_ctl_x_prefix_map() {
    assert_eq!(
        eval_one(
            r#"(let ((g (make-sparse-keymap))
                     (ctlx (make-sparse-keymap)))
                 (define-key ctlx "2" 'split-window-below)
                 (define-key ctlx "3" 'split-window-right)
                 (define-key g "\C-x" ctlx)
                 (use-global-map g)
                 (list (key-binding [24 50])
                       (lookup-key g [24 50])
                       (key-binding [24 51])
                       (lookup-key g [24 51])))"#
        ),
        "OK (split-window-below split-window-below split-window-right split-window-right)"
    );
}

#[test]
fn define_key_sequence_preserves_gnu_prefix_symbol_bindings() {
    assert_eq!(
        eval_one(
            r#"(let ((esc (make-keymap))
                     (ctlx (make-keymap))
                     (g (make-keymap)))
                 (fset 'ESC-prefix esc)
                 (fset 'Control-X-prefix ctlx)
                 (define-key esc "x" 'execute-extended-command)
                 (define-key ctlx "2" 'split-window-below)
                 (define-key ctlx "3" 'split-window-right)
                 (define-key g "\e" 'ESC-prefix)
                 (define-key g "\C-x" 'Control-X-prefix)
                 (define-key g "\e\e\e" 'keyboard-escape-quit)
                 (define-key g "\C-x\C-z" 'suspend-emacs)
                 (use-global-map g)
                 (list (lookup-key g "\e")
                       (lookup-key esc "x")
                       (lookup-key g "\C-x")
                       (lookup-key ctlx "2")
                       (lookup-key ctlx "3")
                       (lookup-key g "\e\e\e")
                       (lookup-key g "\C-x\C-z")
                       (key-binding [134217848])
                       (key-binding [24 50])
                       (key-binding [24 51])))"#
        ),
        "OK (ESC-prefix execute-extended-command Control-X-prefix split-window-below split-window-right keyboard-escape-quit suspend-emacs execute-extended-command split-window-below split-window-right)"
    );
}

#[test]
fn local_key_binding_nil_when_no_local_map() {
    let mut ev = Evaluator::new();
    let result = builtin_local_key_binding(&mut ev, vec![Value::string("\x03")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn local_key_binding_too_many_args_errors() {
    let mut ev = Evaluator::new();
    let result =
        builtin_local_key_binding(&mut ev, vec![Value::string("\x03"), Value::Nil, Value::Nil]);
    assert!(result.is_err());
}

#[test]
fn minor_mode_key_binding_returns_nil_when_no_modes_are_active() {
    let mut ev = Evaluator::new();
    let result = builtin_minor_mode_key_binding(&mut ev, vec![Value::string("\x03")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn minor_mode_key_binding_returns_first_matching_mode_binding() {
    assert_eq!(
        eval_one(
            r#"(let* ((m1 (make-sparse-keymap))
                      (m2 (make-sparse-keymap)))
                 (define-key m1 "\x01" 'ignore)
                 (define-key m2 "\x01" 'forward-char)
                 (let ((minor-mode-map-alist (list (cons 'mode1 m1)
                                                   (cons 'mode2 m2)))
                       (mode1 t)
                       (mode2 t))
                   (minor-mode-key-binding "\x01")))"#
        ),
        "OK ((mode1 . ignore))"
    );
}

#[test]
fn minor_mode_key_binding_invalid_keymap_id_errors_for_active_mode() {
    assert_eq!(
        eval_one(
            r#"(let ((minor-mode-map-alist '((demo-mode . 999999)))
                     (demo-mode t))
                 (condition-case err
                     (minor-mode-key-binding "\x01")
                   (error err)))"#
        ),
        "OK (wrong-type-argument keymapp 999999)"
    );
}

#[test]
fn minor_mode_key_binding_prefers_emulation_mode_maps() {
    assert_eq!(
        eval_one(
            r#"(let* ((m-minor (make-sparse-keymap))
                      (m-emu (make-sparse-keymap)))
                 (define-key m-minor "\x01" 'ignore)
                 (define-key m-emu "\x01" 'forward-char)
                 (let ((emulation-mode-map-alists (list (list (cons 'emu-mode m-emu))))
                       (minor-mode-map-alist (list (cons 'minor-mode m-minor)))
                       (emu-mode t)
                       (minor-mode t))
                   (minor-mode-key-binding "\x01")))"#
        ),
        "OK ((emu-mode . forward-char))"
    );
}

#[test]
fn minor_mode_key_binding_prefers_overriding_mode_maps() {
    assert_eq!(
        eval_one(
            r#"(let* ((m-over (make-sparse-keymap))
                      (m-minor (make-sparse-keymap)))
                 (define-key m-over "\x01" 'forward-char)
                 (define-key m-minor "\x01" 'ignore)
                 (let ((minor-mode-overriding-map-alist (list (cons 'minor-mode m-over)))
                       (minor-mode-map-alist (list (cons 'minor-mode m-minor)))
                       (minor-mode t))
                   (minor-mode-key-binding "\x01")))"#
        ),
        "OK ((minor-mode . forward-char))"
    );
}

#[test]
fn minor_mode_key_binding_resolves_symbol_emulation_alists() {
    assert_eq!(
        eval_one(
            r#"(let* ((m (make-sparse-keymap)))
                 (define-key m "\x01" 'ignore)
                 (let ((emu-alist (list (cons 'emu-mode m)))
                       (emulation-mode-map-alists '(emu-alist))
                       (emu-mode t))
                   (minor-mode-key-binding "\x01")))"#
        ),
        "OK ((emu-mode . ignore))"
    );
}

#[test]
fn minor_mode_key_binding_invalid_emulation_keymap_id_errors() {
    assert_eq!(
        eval_one(
            r#"(let ((emulation-mode-map-alists (list (list (cons 'emu-mode 999999))))
                     (emu-mode t))
                 (condition-case err
                     (minor-mode-key-binding "\x01")
                   (error err)))"#
        ),
        "OK (wrong-type-argument keymapp 999999)"
    );
}

#[test]
fn minor_mode_key_binding_too_many_args_errors() {
    let mut ev = Evaluator::new();
    let result = builtin_minor_mode_key_binding(
        &mut ev,
        vec![Value::string("\x03"), Value::True, Value::symbol("extra")],
    );
    assert!(result.is_err());
}

// -------------------------------------------------------------------
// describe-key-briefly
// -------------------------------------------------------------------

#[test]
fn describe_key_briefly_is_real_lisp_function_after_bootstrap() {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let function = ev
        .obarray
        .symbol_function("describe-key-briefly")
        .expect("missing describe-key-briefly bootstrapped function cell");
    assert!(!crate::emacs_core::autoload::is_autoload_value(&function));
}

#[test]
fn describe_key_briefly_loads_from_gnu_help_el() {
    let result = bootstrap_eval_all(
        r#"(let ((g (make-sparse-keymap)))
             (define-key g (kbd "C-f") #'forward-char)
             (use-global-map g)
             (describe-key-briefly (kbd "C-f")))"#,
    );
    assert_eq!(result[0], r#"OK "C-f runs the command forward-char""#);
}

#[test]
fn describe_key_briefly_loaded_insert_writes_message() {
    let result = bootstrap_eval_all(
        r#"(with-temp-buffer
             (let ((g (make-sparse-keymap)))
               (define-key g (kbd "C-f") #'forward-char)
               (use-global-map g)
               (list (describe-key-briefly (kbd "C-f") t)
                     (buffer-string)
                     (subrp (symbol-function 'describe-key-briefly)))))"#,
    );
    assert_eq!(
        result[0],
        r#"OK (nil "C-f runs the command forward-char" nil)"#
    );
}

#[test]
fn describe_key_briefly_loaded_wrong_type_matches_gnu() {
    let result = bootstrap_eval_all(
        r#"(condition-case err
               (describe-key-briefly 1)
             (error (list 'err (car err))))"#,
    );
    assert_eq!(result[0], r#"OK (err wrong-type-argument)"#);
}

// -------------------------------------------------------------------
// thing-at-point / symbol-at-point / word-at-point
// -------------------------------------------------------------------

#[test]
fn thingatpt_startup_functions_are_autoloaded() {
    let ev = eval_with_ldefs_boot_autoloads(&[
        "bounds-of-thing-at-point",
        "thing-at-point",
        "symbol-at-point",
    ]);

    for name in [
        "bounds-of-thing-at-point",
        "thing-at-point",
        "symbol-at-point",
    ] {
        let function = ev
            .obarray
            .symbol_function(name)
            .unwrap_or_else(|| panic!("missing startup function cell for {name}"));
        assert!(
            crate::emacs_core::autoload::is_autoload_value(&function),
            "{name} should come from GNU thingatpt.el"
        );
    }

    assert!(!ev.obarray.fboundp("word-at-point"));
}

#[test]
fn word_at_point_starts_unbound_before_thingatpt_load() {
    let mut ev = Evaluator::new();
    let result = eval_all_with(
        &mut ev,
        r#"(progn
             (get-buffer-create "word-at-point-startup")
             (set-buffer "word-at-point-startup")
             (insert "alpha beta")
             (goto-char 3)
             (word-at-point))"#,
    );
    assert_eq!(result[0], "ERR (void-function (word-at-point))");
}

#[test]
fn thingatpt_functions_load_from_gnu_elisp() {
    let result = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "foo bar")
             (goto-char 2)
             (list (thing-at-point 'word)
                   (symbol-name (symbol-at-point))
                   (fboundp 'word-at-point)
                   (bounds-of-thing-at-point 'word)))"#,
    );
    assert_eq!(result[0], r#"OK ("foo" "foo" t (1 . 4))"#);
}

// -------------------------------------------------------------------
// command-execute
// -------------------------------------------------------------------

#[test]
fn command_execute_builtin_ignore() {
    let mut ev = gnu_simple_command_execute_eval();
    let result = ev.apply(
        Value::symbol("command-execute"),
        vec![Value::symbol("ignore")],
    );
    let result = result.unwrap();
    assert!(result.is_nil());
}

#[test]
fn command_execute_rejects_non_vector_keys_argument() {
    let mut ev = gnu_simple_command_execute_eval();
    let result = ev
        .apply(
            Value::symbol("command-execute"),
            vec![Value::symbol("ignore"), Value::Nil, Value::string("a")],
        )
        .expect_err("command-execute should reject non-vector keys argument");
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("vectorp"), Value::string("a")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn command_execute_accepts_vector_keys_argument() {
    let mut ev = gnu_simple_command_execute_eval();
    let result = ev
        .apply(
            Value::symbol("command-execute"),
            vec![
                Value::symbol("ignore"),
                Value::Nil,
                Value::vector(vec![Value::Int(97)]),
            ],
        )
        .expect("command-execute should accept vector keys argument");
    assert!(result.is_nil());
}

#[test]
fn command_execute_does_not_record_keys_argument_in_recent_history() {
    let mut ev = gnu_simple_command_execute_eval();
    let result = ev
        .apply(
            Value::symbol("command-execute"),
            vec![
                Value::symbol("ignore"),
                Value::Nil,
                Value::vector(vec![Value::Int(97), Value::symbol("mouse-1")]),
            ],
        )
        .expect("command-execute should accept vector keys argument");
    assert!(result.is_nil());
    assert!(ev.recent_input_events().is_empty());
}

#[test]
fn command_execute_keys_vector_keeps_this_command_keys_empty_in_batch() {
    let mut ev = gnu_simple_command_execute_eval();
    let _ = eval_all_with(
        &mut ev,
        "(fset 'neo-rk-loop-probe
                (lambda ()
                  (interactive)
                  (list (this-command-keys) (this-command-keys-vector))))",
    );

    let result = ev
        .apply(
            Value::symbol("command-execute"),
            vec![
                Value::symbol("neo-rk-loop-probe"),
                Value::Nil,
                Value::vector(vec![Value::Int(97), Value::Int(98)]),
            ],
        )
        .expect("command-execute should accept vector keys argument");
    let output = list_to_vec(&result).expect("probe result should be list");
    assert_eq!(output, vec![Value::string(""), Value::vector(vec![])]);
    assert!(ev.recent_input_events().is_empty());
}

#[test]
fn command_execute_rejects_list_keys_argument_without_recording_recent_history() {
    let mut ev = gnu_simple_command_execute_eval();
    let keys = Value::list(vec![Value::Int(97), Value::Int(98)]);
    let result = ev
        .apply(
            Value::symbol("command-execute"),
            vec![Value::symbol("ignore"), Value::Nil, keys],
        )
        .expect_err("command-execute should reject list keys argument");
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("vectorp"), keys]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
    assert!(ev.recent_input_events().is_empty());
}

#[test]
fn command_execute_rejects_too_many_arguments() {
    let mut ev = gnu_simple_command_execute_eval();
    let result = ev
        .apply(
            Value::symbol("command-execute"),
            vec![
                Value::symbol("ignore"),
                Value::Nil,
                Value::Nil,
                Value::Nil,
                Value::Nil,
            ],
        )
        .expect_err("command-execute should reject too many arguments");
    match result {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn command_execute_builtin_eval_expression_reads_stdin_in_batch() {
    let mut ev = gnu_simple_command_execute_with_eval_expression_eval();
    let result = ev
        .apply(
            Value::symbol("command-execute"),
            vec![Value::symbol("eval-expression")],
        )
        .expect_err("command-execute eval-expression should signal end-of-file in batch");
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "end-of-file");
            assert_eq!(sig.data, vec![Value::string("Error reading from stdin")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn command_execute_builtin_self_insert_command_is_noop() {
    let mut ev = gnu_simple_command_execute_eval();
    let result = ev
        .apply(
            Value::symbol("command-execute"),
            vec![Value::symbol("self-insert-command")],
        )
        .expect("self-insert-command should execute");
    assert!(result.is_nil());
}

#[test]
fn command_execute_builtin_delete_char_uses_default_prefix_arg() {
    let results = gnu_simple_command_execute_eval_all(
        r#"(with-temp-buffer
             (insert "abc")
             (goto-char 1)
             (command-execute 'delete-char)
             (buffer-string))"#,
    );
    assert_eq!(results[0], "OK \"bc\"");
}

#[test]
fn call_interactively_builtin_delete_char_uses_default_prefix_arg() {
    let mut ev = Evaluator::new();
    let results = eval_all_with(
        &mut ev,
        r#"(with-temp-buffer
             (insert "abc")
             (goto-char 1)
             (call-interactively 'delete-char)
             (buffer-string))"#,
    );
    assert_eq!(results[0], "OK \"bc\"");
}

#[test]
fn call_interactively_rejects_non_vector_keys_argument() {
    let mut ev = Evaluator::new();
    let result = builtin_call_interactively(
        &mut ev,
        vec![Value::symbol("ignore"), Value::Nil, Value::string("b")],
    )
    .expect_err("call-interactively should reject non-vector keys argument");
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("vectorp"), Value::string("b")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn call_interactively_accepts_vector_keys_argument() {
    let mut ev = Evaluator::new();
    let result = builtin_call_interactively(
        &mut ev,
        vec![
            Value::symbol("ignore"),
            Value::Nil,
            Value::vector(vec![Value::Int(98)]),
        ],
    )
    .expect("call-interactively should accept vector keys argument");
    assert!(result.is_nil());
}

#[test]
fn call_interactively_does_not_record_keys_argument_in_recent_history() {
    let mut ev = Evaluator::new();
    let result = builtin_call_interactively(
        &mut ev,
        vec![
            Value::symbol("ignore"),
            Value::Nil,
            Value::vector(vec![Value::Int(98)]),
        ],
    )
    .expect("call-interactively should accept vector keys argument");
    assert!(result.is_nil());
    assert!(ev.recent_input_events().is_empty());
}

#[test]
fn call_interactively_keys_vector_keeps_this_command_keys_empty_in_batch() {
    let mut ev = Evaluator::new();
    let _ = eval_all_with(
        &mut ev,
        "(fset 'neo-rk-loop-probe
                (lambda ()
                  (interactive)
                  (list (this-command-keys) (this-command-keys-vector))))",
    );

    let result = builtin_call_interactively(
        &mut ev,
        vec![
            Value::symbol("neo-rk-loop-probe"),
            Value::Nil,
            Value::vector(vec![Value::symbol("foo")]),
        ],
    )
    .expect("call-interactively should accept vector keys argument");
    let output = list_to_vec(&result).expect("probe result should be list");
    assert_eq!(output, vec![Value::string(""), Value::vector(vec![])]);
    assert!(ev.recent_input_events().is_empty());
}

#[test]
fn call_interactively_rejects_list_keys_argument_without_recording_recent_history() {
    let mut ev = Evaluator::new();
    let keys = Value::list(vec![Value::Int(97), Value::Int(98)]);
    let result =
        builtin_call_interactively(&mut ev, vec![Value::symbol("ignore"), Value::Nil, keys])
            .expect_err("call-interactively should reject list keys argument");
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("vectorp"), keys]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
    assert!(ev.recent_input_events().is_empty());
}

#[test]
fn call_interactively_rejects_too_many_arguments() {
    let mut ev = Evaluator::new();
    let result = builtin_call_interactively(
        &mut ev,
        vec![Value::symbol("ignore"), Value::Nil, Value::Nil, Value::Nil],
    )
    .expect_err("call-interactively should reject too many arguments");
    match result {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn command_execute_builtin_upcase_word_uses_default_prefix_arg() {
    let results = gnu_simple_command_execute_eval_all(
        r#"(with-temp-buffer
             (insert "abc def")
             (goto-char 1)
             (command-execute 'upcase-word)
             (buffer-string))"#,
    );
    assert_eq!(results[0], "OK \"ABC def\"");
}

#[test]
fn call_interactively_builtin_capitalize_word_uses_default_prefix_arg() {
    let mut ev = Evaluator::new();
    let results = eval_all_with(
        &mut ev,
        r#"(with-temp-buffer
             (insert "abc def")
             (goto-char 1)
             (call-interactively 'capitalize-word)
             (buffer-string))"#,
    );
    assert_eq!(results[0], "OK \"Abc def\"");
}

#[test]
fn command_execute_builtin_transpose_words_uses_default_prefix_arg() {
    let results = gnu_simple_command_execute_eval_all(
        r#"(with-temp-buffer
             (insert "aa bb")
             (goto-char 1)
             (command-execute 'transpose-words)
             (buffer-string))"#,
    );
    assert_eq!(results[0], "OK \"bb aa\"");
}

#[test]
fn command_execute_builtin_other_window_uses_default_prefix_arg() {
    let results = bootstrap_eval_all(
        r#"(let ((w1 (selected-window)))
             (split-window)
             (command-execute 'other-window)
             (not (eq (selected-window) w1)))"#,
    );
    assert_eq!(results[0], "OK t");
}

#[test]
fn call_interactively_builtin_transpose_words_uses_default_prefix_arg() {
    let results = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "aa bb")
             (goto-char 1)
             (call-interactively 'transpose-words)
             (buffer-string))"#,
    );
    assert_eq!(results[0], "OK \"bb aa\"");
}

#[test]
fn call_interactively_builtin_other_window_uses_default_prefix_arg() {
    let results = bootstrap_eval_all(
        r#"(let ((w1 (selected-window)))
             (split-window)
             (call-interactively 'other-window)
             (not (eq (selected-window) w1)))"#,
    );
    assert_eq!(results[0], "OK t");
}

#[test]
fn command_execute_builtin_transpose_sexps_uses_default_prefix_arg() {
    let results = gnu_simple_command_execute_eval_all(
        r#"(with-temp-buffer
             (insert "(aa) (bb)")
             (goto-char 5)
             (command-execute 'transpose-sexps)
             (buffer-string))"#,
    );
    assert_eq!(results[0], "OK \"(bb) (aa)\"");
}

#[test]
fn call_interactively_builtin_transpose_sexps_uses_default_prefix_arg() {
    let results = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "(aa) (bb)")
             (goto-char 5)
             (call-interactively 'transpose-sexps)
             (buffer-string))"#,
    );
    assert_eq!(results[0], "OK \"(bb) (aa)\"");
}

#[test]
fn command_execute_builtin_transpose_sentences_uses_default_prefix_arg() {
    let results = gnu_simple_command_execute_eval_all(
        r#"(with-temp-buffer
             (insert "One.  Two.")
             (goto-char 1)
             (command-execute 'transpose-sentences)
             (buffer-string))"#,
    );
    assert_eq!(results[0], "OK \"Two.  One.\"");
}

#[test]
fn call_interactively_builtin_transpose_sentences_uses_default_prefix_arg() {
    let results = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "One.  Two.")
             (goto-char 1)
             (call-interactively 'transpose-sentences)
             (buffer-string))"#,
    );
    assert_eq!(results[0], "OK \"Two.  One.\"");
}

#[test]
fn command_execute_builtin_transpose_paragraphs_swaps_paragraphs() {
    let results = gnu_simple_command_execute_eval_all(
        r#"(with-temp-buffer
             (insert "A\n\nB")
             (goto-char 1)
             (command-execute 'transpose-paragraphs)
             (buffer-string))"#,
    );
    assert_eq!(results[0], "OK \"\nBA\n\"");
}

#[test]
fn command_execute_builtin_kill_region_uses_marked_region() {
    let results = gnu_simple_command_execute_eval_all(
        r#"(with-temp-buffer
             (insert "abc")
             (goto-char 1)
             (set-mark 3)
             (command-execute 'kill-region)
             (buffer-string))"#,
    );
    assert_eq!(results[0], "OK \"c\"");
}

#[test]
fn call_interactively_builtin_kill_ring_save_uses_marked_region() {
    let results = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "abc")
             (goto-char 1)
             (set-mark 3)
             (call-interactively 'kill-ring-save)
             (current-kill 0 t))"#,
    );
    assert_eq!(results[0], "OK \"ab\"");
}

#[test]
fn command_execute_builtin_copy_region_as_kill_uses_marked_region() {
    let results = gnu_simple_command_execute_eval_all(
        r#"(let ((kill-ring nil))
             (with-temp-buffer
               (insert "abc")
               (goto-char 1)
               (set-mark 3)
               (command-execute 'copy-region-as-kill)
               (current-kill 0 t)))"#,
    );
    assert_eq!(results[0], "OK \"ab\"");
}

#[test]
fn call_interactively_builtin_copy_region_as_kill_uses_marked_region() {
    let results = bootstrap_eval_all(
        r#"(let ((kill-ring nil))
             (with-temp-buffer
               (insert "abc")
               (goto-char 1)
               (set-mark 3)
               (call-interactively 'copy-region-as-kill)
               (current-kill 0 t)))"#,
    );
    assert_eq!(results[0], "OK \"ab\"");
}

#[test]
fn call_interactively_builtin_upcase_region_uses_marked_region() {
    let results = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "abc")
             (goto-char 1)
             (set-mark 3)
             (call-interactively 'upcase-region)
             (buffer-string))"#,
    );
    assert_eq!(results[0], "OK \"ABc\"");
}

#[test]
fn call_interactively_builtin_downcase_region_uses_marked_region() {
    let results = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "ABC")
             (goto-char 1)
             (set-mark 3)
             (call-interactively 'downcase-region)
             (buffer-string))"#,
    );
    assert_eq!(results[0], "OK \"abC\"");
}

#[test]
fn call_interactively_builtin_capitalize_region_uses_marked_region() {
    let results = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "abc")
             (goto-char 1)
             (set-mark 3)
             (call-interactively 'capitalize-region)
             (buffer-string))"#,
    );
    assert_eq!(results[0], "OK \"Abc\"");
}

#[test]
fn command_execute_builtin_upcase_region_signals_args_out_of_range() {
    let results = gnu_simple_command_execute_eval_all(
        r#"(with-temp-buffer
             (insert "abc")
             (goto-char 1)
             (set-mark 3)
             (condition-case err
                 (command-execute 'upcase-region)
               (error err)))"#,
    );
    assert_eq!(results[0], "OK (args-out-of-range \"\" 0)");
}

#[test]
fn command_execute_builtin_downcase_region_signals_args_out_of_range() {
    let results = gnu_simple_command_execute_eval_all(
        r#"(with-temp-buffer
             (insert "ABC")
             (goto-char 1)
             (set-mark 3)
             (condition-case err
                 (command-execute 'downcase-region)
               (error err)))"#,
    );
    assert_eq!(results[0], "OK (args-out-of-range \"\" 0)");
}

#[test]
fn command_execute_builtin_capitalize_region_uses_marked_region() {
    let results = gnu_simple_command_execute_eval_all(
        r#"(with-temp-buffer
             (insert "abc")
             (goto-char 1)
             (set-mark 3)
             (command-execute 'capitalize-region)
             (buffer-string))"#,
    );
    assert_eq!(results[0], "OK \"Abc\"");
}

#[test]
fn command_execute_builtin_capitalize_region_without_mark_signals_error() {
    let results = gnu_simple_command_execute_eval_all(
        r#"(with-temp-buffer
             (insert "abc")
             (goto-char 1)
             (condition-case err
                 (command-execute 'capitalize-region)
               (error err)))"#,
    );
    assert_eq!(
        results[0],
        "OK (error \"The mark is not set now, so there is no region\")"
    );
}

#[test]
fn call_interactively_builtin_upcase_initials_region_uses_marked_region() {
    let results = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "abc")
             (goto-char 1)
             (set-mark 3)
             (call-interactively 'upcase-initials-region)
             (buffer-string))"#,
    );
    assert_eq!(results[0], "OK \"Abc\"");
}

#[test]
fn command_execute_builtin_upcase_initials_region_uses_marked_region() {
    let results = gnu_simple_command_execute_eval_all(
        r#"(with-temp-buffer
             (insert "abc")
             (goto-char 1)
             (set-mark 3)
             (command-execute 'upcase-initials-region)
             (buffer-string))"#,
    );
    assert_eq!(results[0], "OK \"Abc\"");
}

#[test]
fn command_execute_builtin_upcase_initials_region_without_mark_signals_error() {
    let results = gnu_simple_command_execute_eval_all(
        r#"(with-temp-buffer
             (insert "abc")
             (goto-char 1)
             (condition-case err
                 (command-execute 'upcase-initials-region)
               (error err)))"#,
    );
    assert_eq!(
        results[0],
        "OK (error \"The mark is not set now, so there is no region\")"
    );
}

#[test]
fn command_execute_builtin_kill_region_without_mark_signals_user_error() {
    let results = gnu_simple_command_execute_eval_all(
        r#"(with-temp-buffer
             (insert "abc")
             (goto-char 1)
             (condition-case err
                 (command-execute 'kill-region)
               (error err)))"#,
    );
    assert_eq!(
        results[0],
        "OK (user-error \"The mark is not set now, so there is no region\")"
    );
}

#[test]
fn command_execute_builtin_kill_ring_save_without_mark_signals_error() {
    let results = gnu_simple_command_execute_eval_all(
        r#"(with-temp-buffer
             (insert "abc")
             (goto-char 1)
             (condition-case err
                 (command-execute 'kill-ring-save)
               (error err)))"#,
    );
    assert_eq!(
        results[0],
        "OK (error \"The mark is not set now, so there is no region\")"
    );
}

#[test]
fn call_interactively_builtin_kill_ring_save_without_mark_signals_error() {
    let mut ev = Evaluator::new();
    let results = eval_all_with(
        &mut ev,
        r#"(with-temp-buffer
             (insert "abc")
             (goto-char 1)
             (condition-case err
                 (call-interactively 'kill-ring-save)
               (error err)))"#,
    );
    assert_eq!(
        results[0],
        "OK (error \"The mark is not set now, so there is no region\")"
    );
}

#[test]
fn command_execute_builtin_copy_region_as_kill_without_mark_signals_error() {
    let results = gnu_simple_command_execute_eval_all(
        r#"(with-temp-buffer
             (insert "abc")
             (goto-char 1)
             (condition-case err
                 (command-execute 'copy-region-as-kill)
               (error err)))"#,
    );
    assert_eq!(
        results[0],
        "OK (error \"The mark is not set now, so there is no region\")"
    );
}

#[test]
fn call_interactively_builtin_copy_region_as_kill_without_mark_signals_error() {
    let mut ev = Evaluator::new();
    let results = eval_all_with(
        &mut ev,
        r#"(with-temp-buffer
             (insert "abc")
             (goto-char 1)
             (condition-case err
                 (call-interactively 'copy-region-as-kill)
               (error err)))"#,
    );
    assert_eq!(
        results[0],
        "OK (error \"The mark is not set now, so there is no region\")"
    );
}

#[test]
fn find_file_is_owned_by_gnu_files_el() {
    let results = gnu_files_command_eval_all(
        r#"(list
             (commandp 'find-file)
             (subrp (symbol-function 'find-file)))"#,
    );
    assert_eq!(results[0], "OK (t nil)");
}

#[test]
fn save_buffer_is_owned_by_gnu_files_el() {
    let results = gnu_files_command_eval_all(
        r#"(list
             (commandp 'save-buffer)
             (subrp (symbol-function 'save-buffer)))"#,
    );
    assert_eq!(results[0], "OK (t nil)");
}

#[test]
fn command_execute_builtin_set_mark_command_returns_nil() {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let result = ev
        .apply(
            Value::symbol("command-execute"),
            vec![Value::symbol("set-mark-command")],
        )
        .expect("set-mark-command should execute");
    assert!(result.is_nil());
}

#[test]
fn bootstrap_command_execute_quoted_insert_uses_simple_el() {
    let results = gnu_simple_quoted_insert_eval_all(
        r#"(list (commandp 'quoted-insert)
                 (subrp (symbol-function 'quoted-insert))
                 (condition-case err
                     (progn (command-execute 'quoted-insert) 'no-error)
                   (error (list (car err) (cadr err)))))"#,
    );
    assert_eq!(
        results[0],
        r#"OK (t nil (end-of-file "Error reading from stdin"))"#
    );
}

#[test]
fn command_execute_is_not_dispatch_builtin() {
    assert!(
        !super::super::builtin_registry::is_dispatch_builtin_name("command-execute"),
        "command-execute should come from GNU simple.el"
    );
}

#[test]
fn prefix_argument_commands_are_not_dispatch_builtins() {
    for name in ["universal-argument", "digit-argument", "negative-argument"] {
        assert!(
            !super::super::builtin_registry::is_dispatch_builtin_name(name),
            "{name} should come from GNU simple.el"
        );
    }
}

#[test]
fn bootstrap_command_execute_universal_argument_sets_prefix_arg() {
    let results = gnu_simple_universal_argument_eval_all(
        r#"(progn
             (setq prefix-arg nil)
             (command-execute 'universal-argument)
             (list (consp prefix-arg)
                   (equal prefix-arg '(4))
                   (subrp (symbol-function 'universal-argument))))"#,
    );
    assert_eq!(results[0], "OK (t t nil)");
}

#[test]
fn command_execute_calls_function() {
    let mut ev = gnu_simple_command_execute_eval();
    eval_all_with(
        &mut ev,
        r#"(defvar exec-ran nil)
           (defun test-cmd () (setq exec-ran t))"#,
    );
    ev.interactive
        .register_interactive("test-cmd", InteractiveSpec::no_args());

    let result = ev
        .apply(
            Value::symbol("command-execute"),
            vec![Value::symbol("test-cmd")],
        )
        .unwrap();
    assert!(result.is_truthy());

    let ran = *ev.obarray.symbol_value("exec-ran").unwrap();
    assert!(ran.is_truthy());
}

#[test]
fn command_execute_non_command_signals_commandp_error() {
    let mut ev = gnu_simple_command_execute_eval();
    let result = ev
        .apply(Value::symbol("command-execute"), vec![Value::symbol("car")])
        .expect_err("command-execute should reject non-command symbols");
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("commandp"), Value::symbol("car")]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn call_interactively_builtin_ignore() {
    let mut ev = Evaluator::new();
    let result = builtin_call_interactively(&mut ev, vec![Value::symbol("ignore")]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn call_interactively_lambda_interactive_p_uses_current_prefix_arg() {
    let mut ev = Evaluator::new();
    let results = eval_all_with(
        &mut ev,
        r#"(let ((f (lambda (n) (interactive "p") n))
                 (current-prefix-arg '(4)))
             (call-interactively f))"#,
    );
    assert_eq!(results[0], "OK 4");
}

#[test]
fn command_execute_lambda_interactive_p_uses_prefix_arg() {
    let mut ev = gnu_simple_command_execute_eval();
    let results = eval_all_with(
        &mut ev,
        r#"(list
             (let ((f (lambda (n) (interactive "p") n))
                   (current-prefix-arg '(4)))
               (command-execute f))
             (let ((f (lambda (n) (interactive "p") n))
                   (current-prefix-arg '(4))
                   (prefix-arg '(5)))
               (command-execute f)))"#,
    );
    assert_eq!(results[0], "OK (1 5)");
}

#[test]
fn call_interactively_lambda_interactive_p_prefers_current_prefix_arg() {
    let mut ev = Evaluator::new();
    let results = eval_all_with(
        &mut ev,
        r#"(let ((f (lambda (n) (interactive "p") n))
                 (current-prefix-arg '(4))
                 (prefix-arg '(5)))
             (call-interactively f))"#,
    );
    assert_eq!(results[0], "OK 4");
}

#[test]
fn interactive_lambda_forms_support_p_p_and_expression_specs() {
    let mut ev = gnu_simple_command_execute_eval();
    let results = eval_all_with(
        &mut ev,
        r#"(list
             (let ((f (lambda (arg) (interactive "P") arg))
                   (current-prefix-arg '(4)))
               (call-interactively f))
             (let ((f (lambda (arg) (interactive "P") arg))
                   (prefix-arg '(5)))
               (command-execute f))
             (call-interactively (lambda (x) (interactive (list 7)) x))
             (command-execute (lambda (x) (interactive (list 8)) x))
             (condition-case err
                 (call-interactively (lambda (x) (interactive 7) x))
               (error err)))"#,
    );
    assert_eq!(results[0], "OK ((4) (5) 7 8 (wrong-type-argument listp 7))");
}

#[test]
fn call_interactively_accepts_quoted_lambda_commands() {
    let mut ev = Evaluator::new();
    let results = eval_all_with(
        &mut ev,
        r#"(let ((current-prefix-arg 3))
             (call-interactively '(lambda (n) (interactive "p") n)))"#,
    );
    assert_eq!(results[0], "OK 3");
}

#[test]
fn interactive_lambda_r_spec_reads_region_for_call_interactively() {
    // set-mark is defined in simple.el — need bootstrap evaluator.
    let results = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "abc")
             (goto-char 2)
             (set-mark 3)
             (let ((f (lambda (b e) (interactive "r") (list b e))))
               (call-interactively f)))"#,
    );
    assert_eq!(results[0], "OK (2 3)");

    let results = bootstrap_eval_all(
        r#"(with-temp-buffer
             (insert "abc")
             (goto-char 2)
             (condition-case err
                 (let ((f (lambda (b e) (interactive "r") (list b e))))
                   (call-interactively f))
               (error err)))"#,
    );
    assert_eq!(
        results[0],
        "OK (error \"The mark is not set now, so there is no region\")"
    );
}

#[test]
fn interactive_lambda_s_spec_reads_prompt_and_signals_eof_in_batch() {
    let mut ev = gnu_simple_command_execute_eval();
    let results = eval_all_with(
        &mut ev,
        r#"(list
             (condition-case err
                 (call-interactively (lambda (s) (interactive "sPrompt: ") s))
               (error err))
             (condition-case err
                 (command-execute (lambda (s) (interactive "sPrompt: ") s))
               (error err)))"#,
    );
    assert_eq!(
        results[0],
        "OK ((end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\"))"
    );
}

#[test]
fn interactive_lambda_extended_string_codes_cover_point_mark_ignored_and_key_readers() {
    let results = bootstrap_eval_all(
        r#"(list
             (let ((unread-command-events (list 97 98 99)))
               (with-temp-buffer
                 (insert "abcd")
                 (goto-char 3)
                 (set-mark 2)
                 (call-interactively
                  (lambda (pt mk ignored ch keys keyvec)
                    (interactive "d
m
i
c
k
K")
                    (list pt mk ignored ch keys keyvec)))))
             (let ((unread-command-events (list 97 98 99)))
               (with-temp-buffer
                 (insert "abcd")
                 (goto-char 3)
                 (set-mark 2)
                 (command-execute
                  (lambda (pt mk ignored ch keys keyvec)
                    (interactive "d
m
i
c
k
K")
                    (list pt mk ignored ch keys keyvec)))))
             (with-temp-buffer
               (insert "abc")
               (goto-char 2)
               (condition-case err
                   (call-interactively (lambda (mk) (interactive "m") mk))
                 (error err))))"#,
    );
    assert_eq!(
        results[0],
        "OK ((3 2 nil 97 \"b\" [99]) (3 2 nil 97 \"b\" [99]) (error \"The mark is not set now\"))"
    );
}

#[test]
fn interactive_lambda_extended_reader_prompt_codes_signal_eof_in_batch() {
    let mut ev = gnu_simple_command_execute_eval();
    let results = eval_all_with(
        &mut ev,
        r#"(list
             (condition-case err
                 (call-interactively (lambda (x) (interactive "aFunction: ") x))
               (error err))
             (condition-case err
                 (call-interactively (lambda (x) (interactive "bBuffer: ") x))
               (error err))
             (condition-case err
                 (call-interactively (lambda (x) (interactive "BBuffer: ") x))
               (error err))
             (condition-case err
                 (call-interactively (lambda (x) (interactive "CCommand: ") x))
               (error err))
             (condition-case err
                 (call-interactively (lambda (x) (interactive "DDirectory: ") x))
               (error err))
             (condition-case err
                 (call-interactively (lambda (x) (interactive "fFind file: ") x))
               (error err))
             (condition-case err
                 (call-interactively (lambda (x) (interactive "FFind file: ") x))
               (error err))
             (condition-case err
                 (call-interactively (lambda (x) (interactive "vVariable: ") x))
               (error err))
             (condition-case err
                 (command-execute (lambda (x) (interactive "bBuffer: ") x))
               (error err))
             (condition-case err
                 (command-execute (lambda (x) (interactive "fFind file: ") x))
               (error err)))"#,
    );
    assert_eq!(
        results[0],
        "OK ((end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\"))"
    );
}

#[test]
fn interactive_lambda_n_and_optional_coding_specs_follow_prefix_and_batch_behavior() {
    let mut ev = gnu_simple_command_execute_eval();
    let results = eval_all_with(
        &mut ev,
        r#"(list
             (let ((current-prefix-arg '(4))
                   (prefix-arg nil))
               (call-interactively (lambda (n) (interactive "NNumber: ") n)))
             (let ((current-prefix-arg nil)
                   (prefix-arg '(5)))
               (command-execute (lambda (n) (interactive "NNumber: ") n)))
             (let ((current-prefix-arg nil)
                   (prefix-arg nil))
               (condition-case err
                   (call-interactively (lambda (n) (interactive "NNumber: ") n))
                 (error err)))
             (let ((current-prefix-arg nil)
                   (prefix-arg nil))
               (condition-case err
                   (command-execute (lambda (n) (interactive "NNumber: ") n))
                 (error err)))
             (let ((unread-command-events (list 97)))
               (list
                (call-interactively (lambda (c) (interactive "ZCoding: ") c))
                unread-command-events))
             (let ((unread-command-events (list 97)))
               (list
                (command-execute (lambda (c) (interactive "ZCoding: ") c))
                unread-command-events)))"#,
    );
    assert_eq!(
        results[0],
        "OK (4 5 (end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\") (nil (97)) (nil (97)))"
    );
}

#[test]
fn interactive_lambda_m_s_x_x_and_z_specs_signal_eof_in_batch() {
    let mut ev = gnu_simple_command_execute_eval();
    let results = eval_all_with(
        &mut ev,
        r#"(list
             (condition-case err
                 (call-interactively (lambda (s) (interactive "MString: ") s))
               (error err))
             (condition-case err
                 (call-interactively (lambda (s) (interactive "SSymbol: ") s))
               (error err))
             (condition-case err
                 (call-interactively (lambda (x) (interactive "xExpr: ") x))
               (error err))
             (condition-case err
                 (call-interactively (lambda (x) (interactive "XExpr: ") x))
               (error err))
             (condition-case err
                 (call-interactively (lambda (c) (interactive "zCoding: ") c))
               (error err))
             (condition-case err
                 (command-execute (lambda (s) (interactive "MString: ") s))
               (error err))
             (condition-case err
                 (command-execute (lambda (s) (interactive "SSymbol: ") s))
               (error err))
             (condition-case err
                 (command-execute (lambda (x) (interactive "xExpr: ") x))
               (error err))
             (condition-case err
                 (command-execute (lambda (x) (interactive "XExpr: ") x))
               (error err))
             (condition-case err
                 (command-execute (lambda (c) (interactive "zCoding: ") c))
               (error err)))"#,
    );
    assert_eq!(
        results[0],
        "OK ((end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\"))"
    );
}

#[test]
fn interactive_lambda_g_e_and_u_specs_follow_batch_behavior() {
    let mut ev = gnu_simple_command_execute_eval();
    let results = eval_all_with(
        &mut ev,
        r#"(list
             (condition-case err
                 (call-interactively (lambda (x) (interactive "GFind file: ") x))
               (error err))
             (condition-case err
                 (command-execute (lambda (x) (interactive "GFind file: ") x))
               (error err))
             (let ((unread-command-events (list 97)))
               (list
                (call-interactively (lambda (x) (interactive "U") x))
                unread-command-events))
             (let ((unread-command-events (list 97)))
               (list
                (command-execute (lambda (x) (interactive "U") x))
                unread-command-events))
             (let ((unread-command-events (list 97)))
               (condition-case err
                   (call-interactively (lambda (x) (interactive "e") x))
                 (error (list err unread-command-events))))
             (let ((unread-command-events (list 97)))
               (condition-case err
                   (command-execute (lambda (x) (interactive "e") x))
                 (error (list err unread-command-events)))))"#,
    );
    assert_eq!(
        results[0],
        "OK ((end-of-file \"Error reading from stdin\") (end-of-file \"Error reading from stdin\") (nil (97)) (nil (97)) ((error \"command must be bound to an event with parameters\") (97)) ((error \"command must be bound to an event with parameters\") (97)))"
    );
}

#[test]
fn interactive_lambda_k_k_capital_and_u_specs_match_gnu_batch_mouse_up_event_behavior() {
    let mut ev = gnu_simple_command_execute_eval();
    let results = eval_all_with(
        &mut ev,
        r#"(list
             (let ((unread-command-events (list '(down-mouse-1) '(mouse-1))))
               (call-interactively
                (lambda (keys up) (interactive "k
U") (list keys up))))
             (let ((unread-command-events (list '(down-mouse-1) '(mouse-1))))
               (call-interactively
                (lambda (keys up) (interactive "K
U") (list keys up))))
             (let ((unread-command-events (list '(down-mouse-1) '(mouse-1))))
               (command-execute
                (lambda (keys up) (interactive "k
U") (list keys up))))
             (let ((unread-command-events (list '(down-mouse-1) '(mouse-1))))
               (command-execute
                (lambda (keys up) (interactive "K
U") (list keys up)))))"#,
    );
    assert_eq!(
        results[0],
        "OK (([(down-mouse-1)] [(mouse-1)]) ([(down-mouse-1)] [(mouse-1)]) ([(down-mouse-1)] [(mouse-1)]) ([(down-mouse-1)] [(mouse-1)]))"
    );
}

#[test]
fn interactive_lambda_invalid_control_letter_signals_error() {
    let mut ev = Evaluator::new();
    let results = eval_all_with(
        &mut ev,
        r#"(list
             (let ((r (condition-case err
                          (call-interactively (lambda (x) (interactive "q") x))
                        (error err))))
               (list (if (consp r) (car r) 'non-error)
                     (and (consp r)
                          (stringp (nth 1 r))
                          (>= (length (nth 1 r)) 22)
                          (equal (substring (nth 1 r) 0 22) "Invalid control letter"))))
             (let ((called nil)
                   (r nil))
               (setq r (condition-case err
                           (call-interactively (lambda () (interactive "q") (setq called t)))
                         (error err)))
               (list (if (consp r) (car r) 'non-error)
                     (and (consp r)
                          (stringp (nth 1 r))
                          (>= (length (nth 1 r)) 22)
                          (equal (substring (nth 1 r) 0 22) "Invalid control letter"))
                     called))
             (let ((r (condition-case err
                          (call-interactively (lambda (x) (interactive "*q") x))
                        (error err))))
               (list (if (consp r) (car r) 'non-error)
                     (and (consp r)
                          (stringp (nth 1 r))
                          (>= (length (nth 1 r)) 22)
                          (equal (substring (nth 1 r) 0 22) "Invalid control letter")))))"#,
    );
    assert_eq!(results[0], "OK ((error t) (error t nil) (error t))");
}

#[test]
fn interactive_shift_selection_prefix_sets_mark_and_mark_active() {
    let mut ev = Evaluator::new();
    {
        let buf = ev.buffers.current_buffer_mut().expect("current buffer");
        buf.insert("abcd");
        buf.goto_char(2);
    }
    ev.obarray
        .set_symbol_value("this-command-keys-shift-translated", Value::True);
    ev.obarray
        .set_symbol_value("shift-select-mode", Value::True);

    interactive_apply_shift_selection_prefix(&mut ev);

    let buf = ev.buffers.current_buffer().expect("current buffer");
    assert_eq!(buf.mark(), Some(2));
    assert_eq!(buf.get_buffer_local("mark-active"), Some(&Value::True));
}

#[test]
fn interactive_lambda_prefix_flags_star_hat_and_at_follow_batch_semantics() {
    let results = bootstrap_eval_all(
        r#"(list
             (with-temp-buffer
               (let ((buffer-read-only nil))
                 (call-interactively (lambda () (interactive "*") 'ok))))
             (with-temp-buffer
               (let ((buffer-read-only t))
                 (condition-case err
                     (call-interactively (lambda () (interactive "*") 'ok))
                   (error (car err)))))
             (with-temp-buffer
               (let ((buffer-read-only t)
                     (inhibit-read-only t))
                 (call-interactively (lambda () (interactive "*") 'ok))))
             (with-temp-buffer
               (insert "abcd")
               (goto-char 3)
               (setq mark-active nil)
               (let ((this-command-keys-shift-translated t)
                     (shift-select-mode t))
                 (list
                  (call-interactively (lambda (pt) (interactive "^d") pt))
                  mark-active
                  (mark t))))
             (with-temp-buffer
               (insert "abcd")
               (goto-char 3)
               (setq mark-active nil)
               (let ((this-command-keys-shift-translated t)
                     (shift-select-mode t))
                 (list
                  (command-execute (lambda (pt) (interactive "^d") pt))
                  mark-active
                  (mark t))))
             (with-temp-buffer
               (insert "abcd")
               (goto-char 3)
               (set-mark 1)
               (setq mark-active nil)
               (let ((this-command-keys-shift-translated nil)
                     (shift-select-mode t))
                 (list
                  (call-interactively (lambda (pt) (interactive "^d") pt))
                  mark-active
                  (mark t))))
             (with-temp-buffer
               (insert "ab")
               (goto-char 2)
               (call-interactively (lambda (pt) (interactive "@d") pt)))
             (with-temp-buffer
               (insert "ab")
               (goto-char 2)
               (command-execute (lambda (pt) (interactive "@d") pt)))
             (with-temp-buffer
               (insert "abcd")
               (goto-char 3)
               (setq buffer-read-only t)
               (setq mark-active nil)
               (let ((this-command-keys-shift-translated t)
                     (shift-select-mode t))
                 (condition-case err
                     (progn
                       (call-interactively (lambda (x) (interactive "*^d") x))
                       (list 'ok mark-active (mark t)))
                   (error (list (car err) mark-active (mark t))))))
             (with-temp-buffer
               (insert "abcd")
               (goto-char 3)
               (setq buffer-read-only t)
               (setq mark-active nil)
               (let ((this-command-keys-shift-translated t)
                     (shift-select-mode t))
                 (condition-case err
                     (progn
                       (call-interactively (lambda (x) (interactive "^*d") x))
                       (list 'ok mark-active (mark t)))
                   (error (list (car err) mark-active (mark t)))))))"#,
    );
    // With SymbolValue::BufferLocal, setq on buffer-read-only correctly
    // creates a buffer-local binding. The "*" interactive prefix detects
    // the buffer is read-only and signals buffer-read-only, matching GNU.
    assert_eq!(
        results[0],
        "OK (ok buffer-read-only ok (3 t 3) (3 t 3) (3 nil 1) 2 2 (buffer-read-only nil nil) (buffer-read-only t 3))"
    );
}

#[test]
fn interactive_lambda_e_spec_reads_parameterized_events_from_keys_vector() {
    let mut ev = gnu_simple_command_execute_eval();
    let results = eval_all_with(
        &mut ev,
        r#"(list
             (let ((evt (list 'mouse-1 (list (list (selected-window) (point) '(0 . 0) 0))))
                   (r nil))
               (setq r (call-interactively (lambda (x) (interactive "e") x) nil (vector evt)))
               (and (consp r) (eq (car r) 'mouse-1)))
             (let ((evt (list 'mouse-1 (list (list (selected-window) (point) '(0 . 0) 0))))
                   (r nil))
               (setq r (command-execute (lambda (x) (interactive "e") x) nil (vector evt)))
               (and (consp r) (eq (car r) 'mouse-1)))
             (let ((evt (list 'mouse-1 (list (list (selected-window) (point) '(0 . 0) 0))))
                   (r nil))
               (setq r (call-interactively (lambda (x) (interactive "e") x) nil (vector 97 evt)))
               (and (consp r) (eq (car r) 'mouse-1)))
             (equal
              (call-interactively (lambda (x) (interactive "e") x) nil (vector '(mouse-1)))
              '(mouse-1))
             (condition-case err
                 (call-interactively (lambda (x) (interactive "e") x) nil [mouse-1])
               (error (car err)))
             (condition-case err
                 (command-execute (lambda (x) (interactive "e") x) nil [mouse-1])
               (error (car err)))
             (condition-case err
                 (call-interactively (lambda (x) (interactive "e") x) nil (vector [mouse-1]))
               (error (car err))))"#,
    );
    assert_eq!(results[0], "OK (t t t t error error error)");
}

#[test]
fn interactive_lambda_e_spec_uses_command_key_context_for_event_dispatch() {
    let mut ev = gnu_simple_command_execute_eval();
    let results = eval_all_with(
        &mut ev,
        r#"(list
             (let ((unread-command-events (list '(mouse-1))))
               (list
                (read-event)
                (call-interactively (lambda (x) (interactive "e") x))))
             (let ((unread-command-events (list '(mouse-1))))
               (list
                (read-event)
                (command-execute (lambda (x) (interactive "e") x))))
             (let ((unread-command-events (list 97 '(mouse-1))))
               (list
                (read-event)
                (call-interactively (lambda (x) (interactive "e") x))
                unread-command-events))
             (let ((unread-command-events (list 97 '(mouse-1))))
               (list
                (read-event)
                (command-execute (lambda (x) (interactive "e") x))
                unread-command-events)))"#,
    );
    assert_eq!(
        results[0],
        "OK (((mouse-1) (mouse-1)) ((mouse-1) (mouse-1)) (97 (mouse-1) ((mouse-1))) (97 (mouse-1) ((mouse-1))))"
    );
}

#[test]
fn interactive_lambda_e_spec_does_not_use_unread_queue_without_command_key_context() {
    let mut ev = gnu_simple_command_execute_eval();
    let results = eval_all_with(
        &mut ev,
        r#"(list
             (let ((start (length (recent-keys))))
               (let ((unread-command-events (list '(mouse-1))))
                 (condition-case err
                     (call-interactively (lambda (x) (interactive "e") x))
                   (error (list (car err)
                                unread-command-events
                                (append (nthcdr start (append (recent-keys) nil)) nil))))))
             (let ((start (length (recent-keys))))
               (let ((unread-command-events (list '(mouse-1))))
                 (condition-case err
                     (command-execute (lambda (x) (interactive "e") x))
                   (error (list (car err)
                                unread-command-events
                                (append (nthcdr start (append (recent-keys) nil)) nil))))))
             (let ((start (length (recent-keys))))
               (let ((unread-command-events (list 97 '(mouse-1))))
                 (condition-case err
                     (call-interactively (lambda (x) (interactive "e") x))
                   (error (list (car err)
                                unread-command-events
                                (append (nthcdr start (append (recent-keys) nil)) nil))))))
             (let ((start (length (recent-keys))))
               (let ((unread-command-events (list 97 '(mouse-1))))
                 (condition-case err
                     (command-execute (lambda (x) (interactive "e") x))
                   (error (list (car err)
                                unread-command-events
                                (append (nthcdr start (append (recent-keys) nil)) nil)))))))"#,
    );
    assert_eq!(
        results[0],
        "OK ((error ((mouse-1)) nil) (error ((mouse-1)) nil) (error (97 (mouse-1)) nil) (error (97 (mouse-1)) nil))"
    );
}

#[test]
fn interactive_lambda_e_spec_prefers_existing_command_keys_context() {
    let mut ev = Evaluator::new();
    let results = eval_all_with(
        &mut ev,
        r#"(list
             (let ((unread-command-events (list 97)))
               (read-key-sequence ""))
             (this-command-keys-vector)
             (let ((unread-command-events (list '(mouse-1))))
               (condition-case err
                   (call-interactively (lambda (x) (interactive "e") x))
                 (error (car err))))
             (let ((unread-command-events (list '(mouse-1))))
               (condition-case err
                   (call-interactively (lambda (x) (interactive "e") x) nil [])
                 (error (car err))))
             (call-interactively
              (lambda (x) (interactive "e") x)
              nil
              (vector '(mouse-1))))"#,
    );
    assert_eq!(results[0], "OK (\"a\" [97] error error (mouse-1))");
}

#[test]
fn call_interactively_non_command_signals_commandp_error() {
    let mut ev = Evaluator::new();
    let result = builtin_call_interactively(&mut ev, vec![Value::symbol("car")])
        .expect_err("call-interactively should reject non-command symbols");
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("commandp"), Value::symbol("car")]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn call_interactively_eval_expression_reads_stdin_in_batch() {
    let mut ev = gnu_simple_eval_expression_eval();
    let result = builtin_call_interactively(&mut ev, vec![Value::symbol("eval-expression")])
        .expect_err("call-interactively eval-expression should signal end-of-file in batch");
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "end-of-file");
            assert_eq!(sig.data, vec![Value::string("Error reading from stdin")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn bootstrap_read_expression_internal_map_installs_try_read_binding() {
    let result = bootstrap_eval_all(
        r#"(list (lookup-key read-expression-map (kbd "RET"))
                 (lookup-key read--expression-map (kbd "RET"))
                 (lookup-key read--expression-map (kbd "C-j")))"#,
    )
    .into_iter()
    .next()
    .expect("bootstrap read--expression-map result");
    assert_eq!(
        result,
        "OK (exit-minibuffer read--expression-try-read read--expression-try-read)"
    );
}

#[test]
fn eval_expression_rejects_too_many_args() {
    let mut ev = gnu_simple_eval_expression_eval();
    let result = ev
        .apply(
            Value::symbol("eval-expression"),
            vec![
                Value::Int(1),
                Value::Nil,
                Value::Nil,
                Value::Nil,
                Value::Nil,
            ],
        )
        .expect_err("eval-expression should reject more than four args");
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(sig.data.len(), 2);
            assert_eq!(sig.data[1], Value::Int(5));
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn eval_expression_apply_executes_form_argument() {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");

    let expr = read_first_object(&mut ev, r#"(message "NEO-OK")"#);
    let result = ev
        .apply(
            Value::symbol("eval-expression"),
            vec![expr, Value::Nil, Value::Nil, Value::Int(127)],
        )
        .expect("eval-expression should evaluate message form");
    let rendered = crate::emacs_core::print::print_value(&result);
    let current_message = ev
        .apply(Value::symbol("current-message"), vec![])
        .expect("current-message should be readable after eval-expression");

    assert_eq!(
        result.as_str(),
        Some("NEO-OK"),
        "unexpected eval-expression result={rendered} current-message={:?}",
        current_message.as_str()
    );
}

#[test]
fn call_interactively_eval_expression_executes_read_expression_result() {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");

    eval_all_with(
        &mut ev,
        r#"
        (defun read--expression (&rest _args) '(message "NEO-OK"))
        (defun eval-expression-get-print-arguments (&rest _args) nil)
        "#,
    );

    let result = builtin_call_interactively(&mut ev, vec![Value::symbol("eval-expression")])
        .expect("call-interactively eval-expression should succeed");
    let rendered = crate::emacs_core::print::print_value(&result);
    let current_message = ev
        .apply(Value::symbol("current-message"), vec![])
        .expect("current-message should be readable after call-interactively");

    assert_eq!(
        result.as_str(),
        Some("NEO-OK"),
        "unexpected interactive eval-expression result={rendered} current-message={:?}",
        current_message.as_str()
    );
}

#[test]
fn self_insert_command_argument_validation() {
    let mut ev = Evaluator::new();

    let missing = builtin_self_insert_command(&mut ev, vec![])
        .expect_err("self-insert-command should require one arg");
    match missing {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("self-insert-command"), Value::Int(0)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let too_many =
        builtin_self_insert_command(&mut ev, vec![Value::Int(1), Value::Nil, Value::Nil])
            .expect_err("self-insert-command should reject too many args");
    match too_many {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("self-insert-command"), Value::Int(3)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let wrong_type = builtin_self_insert_command(&mut ev, vec![Value::symbol("x")])
        .expect_err("self-insert-command should type check arg");
    match wrong_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("fixnump"), Value::symbol("x")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let negative = builtin_self_insert_command(&mut ev, vec![Value::Int(-1)])
        .expect_err("self-insert-command should reject negative repetition");
    match negative {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Negative repetition argument -1")]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn self_insert_command_uses_last_command_event_character() {
    let mut ev = Evaluator::new();
    let results = eval_all_with(
        &mut ev,
        r#"(with-temp-buffer
             (let ((last-command-event 97))
               (self-insert-command 2)
               (buffer-string)))"#,
    );
    assert_eq!(results[0], "OK \"aa\"");
}

#[test]
fn self_insert_command_non_nil_second_arg_is_noop() {
    let mut ev = Evaluator::new();
    let results = eval_all_with(
        &mut ev,
        r#"(with-temp-buffer
             (let ((last-command-event 97))
               (self-insert-command 2 t)
               (buffer-string)))"#,
    );
    assert_eq!(results[0], "OK \"\"");
}

#[test]
fn command_execute_self_insert_uses_last_command_event_when_available() {
    let mut ev = gnu_simple_command_execute_eval();
    let results = eval_all_with(
        &mut ev,
        r#"(with-temp-buffer
             (let ((last-command-event 98))
               (command-execute 'self-insert-command nil [98])
               (buffer-string)))"#,
    );
    assert_eq!(results[0], "OK \"b\"");
}

#[test]
fn keyboard_quit_signals_quit() {
    let results = bootstrap_eval_all(
        r#"(condition-case err
               (command-execute 'keyboard-quit)
             (quit (cons (car err) (cdr err))))"#,
    );
    assert_eq!(results[0], "OK (quit)");
}

// -------------------------------------------------------------------
// execute-extended-command
// -------------------------------------------------------------------

#[test]
fn execute_extended_command_with_command_name() {
    let results = gnu_simple_execute_extended_command_eval_all(
        r#"(progn
             (defun neo-eec-noargs ()
               (interactive)
               'neo-eec-noargs-ran)
             (execute-extended-command nil "neo-eec-noargs"))"#,
    );
    assert_eq!(results[0], "OK nil");
}

#[test]
fn execute_extended_command_returns_nil_and_seeds_current_prefix_arg() {
    let results = gnu_simple_execute_extended_command_eval_all(
        r#"(progn
             (setq neo-eec-seen-vars :unset)
             (defun neo-eec-vars ()
               (interactive)
               (setq neo-eec-seen-vars (list current-prefix-arg prefix-arg))
               'neo-eec-vars-ret)
             (list
              (execute-extended-command nil "neo-eec-vars")
              neo-eec-seen-vars
              (let ((prefix-arg '(5))
                    (current-prefix-arg '(6)))
                (execute-extended-command 7 "neo-eec-vars")
                neo-eec-seen-vars)))"#,
    );
    assert_eq!(results[0], "OK (nil (nil nil) (7 nil))");
}

#[test]
fn execute_extended_command_applies_prefix_arg_for_p_and_p_specs() {
    let results = gnu_simple_execute_extended_command_eval_all(
        r#"(progn
             (setq neo-eec-seen-p :unset)
             (setq neo-eec-seen-P :unset)
             (defun neo-eec-p (arg)
               (interactive "p")
               (setq neo-eec-seen-p arg)
               'neo-eec-p-ret)
             (defun neo-eec-P (arg)
               (interactive "P")
               (setq neo-eec-seen-P arg)
               'neo-eec-P-ret)
             (list
              (list (execute-extended-command 7 "neo-eec-p") neo-eec-seen-p)
              (list (execute-extended-command '(4) "neo-eec-p") neo-eec-seen-p)
              (list (execute-extended-command '- "neo-eec-p") neo-eec-seen-p)
              (list (execute-extended-command 7 "neo-eec-P") neo-eec-seen-P)
              (list (execute-extended-command '(4) "neo-eec-P") neo-eec-seen-P)
              (list (execute-extended-command '- "neo-eec-P") neo-eec-seen-P)))"#,
    );
    assert_eq!(
        results[0],
        "OK ((nil 7) (nil 4) (nil -1) (nil 7) (nil (4)) (nil -))"
    );
}

#[test]
fn execute_extended_command_no_name_signals_end_of_file() {
    let mut ev = gnu_simple_execute_extended_command_eval();
    let result = ev
        .apply(Value::symbol("execute-extended-command"), vec![Value::Nil])
        .expect_err("execute-extended-command should signal end-of-file in batch");
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "end-of-file");
            assert_eq!(sig.data, vec![Value::string("Error reading from stdin")]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn execute_extended_command_rejects_symbol_name_payload() {
    let mut ev = gnu_simple_execute_extended_command_eval();
    let result = ev
        .apply(
            Value::symbol("execute-extended-command"),
            vec![Value::Nil, Value::symbol("ignore")],
        )
        .expect_err("symbol payload should not be accepted as a command name");
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string(
                    "\u{2018}ignore\u{2019} is not a valid command name"
                )]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn execute_extended_command_rejects_non_command_name() {
    let mut ev = gnu_simple_execute_extended_command_eval();
    let result = ev
        .apply(
            Value::symbol("execute-extended-command"),
            vec![Value::Nil, Value::string("car")],
        )
        .expect_err("non-command names should be rejected");
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string(
                    "\u{2018}car\u{2019} is not a valid command name"
                )]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn execute_extended_command_rejects_non_string_name_payload() {
    let mut ev = gnu_simple_execute_extended_command_eval();
    let result = ev
        .apply(
            Value::symbol("execute-extended-command"),
            vec![Value::Nil, Value::Int(1)],
        )
        .expect_err("non-string command names should be rejected");
    match result {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string(
                    "\u{2018}1\u{2019} is not a valid command name"
                )]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn execute_extended_command_rejects_overflow_arity() {
    let mut ev = gnu_simple_execute_extended_command_eval();
    let result = ev
        .apply(
            Value::symbol("execute-extended-command"),
            vec![Value::Nil, Value::Nil, Value::Nil, Value::Nil],
        )
        .expect_err("execute-extended-command should reject more than three arguments");
    match result {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn define_key_accepts_optional_remove_arg() {
    let result = eval_one(
        r#"(let ((m (make-sparse-keymap)))
             (eq (define-key m "a" 'ignore t) 'ignore))"#,
    );
    assert_eq!(result, "OK t");
}

// -------------------------------------------------------------------
// where-is-internal
// -------------------------------------------------------------------

#[test]
fn where_is_internal_finds_binding_in_explicit_map() {
    let result = eval_one(
        r#"(let ((m (make-sparse-keymap)))
             (define-key m "a" 'ignore)
             (equal (car (where-is-internal 'ignore m)) [97]))"#,
    );
    assert_eq!(result, "OK t");
}

#[test]
fn where_is_internal_first_only_returns_vector() {
    let result = eval_one(
        r#"(let ((m (make-sparse-keymap)))
             (define-key m "a" 'ignore)
             (equal (where-is-internal 'ignore m t) [97]))"#,
    );
    assert_eq!(result, "OK t");
}

#[test]
fn where_is_internal_accepts_list_of_keymaps() {
    let result = eval_one(
        r#"(let ((m1 (make-sparse-keymap))
                 (m2 (make-sparse-keymap)))
             (define-key m1 "a" 'ignore)
             (define-key m2 "b" 'ignore)
             (list (equal (where-is-internal 'ignore (list m2 m1) t) [98])
                   (equal (where-is-internal 'ignore (list m2 m1))
                          '([98] [97]))))"#,
    );
    assert_eq!(result, "OK (t t)");
}

#[test]
fn where_is_internal_keymap_type_errors() {
    let result = eval_one(
        r#"(condition-case err
               (where-is-internal 'ignore 'not-a-map)
             (error err))"#,
    );
    assert_eq!(result, "OK (wrong-type-argument keymapp not-a-map)");
}

#[test]
fn where_is_internal_non_definition_returns_nil() {
    let result = eval_one("(where-is-internal 1)");
    assert_eq!(result, "OK nil");
}

#[test]
fn where_is_internal_follows_symbol_function_prefix_maps_like_gnu_help_command() {
    let result = eval_one(
        r#"(let ((m (make-keymap))
                 (prefix (make-sparse-keymap)))
             (define-key prefix [1] 'about-emacs)
             (fset 'test-where-is-prefix prefix)
             (define-key m [8] 'test-where-is-prefix)
             (list (keymapp (symbol-function 'test-where-is-prefix))
                   (equal (where-is-internal 'about-emacs prefix t) [1])
                   (equal (where-is-internal 'about-emacs m t) [8 1])
                   (equal (key-description (where-is-internal 'about-emacs m t))
                          "C-h C-a")))"#,
    );
    assert_eq!(result, "OK (t t t t)");
}

#[test]
fn bootstrap_define_keymap_populates_help_style_bindings() {
    let result = bootstrap_eval_all(
        r#"(let ((m (define-keymap
                      "C-a" #'about-emacs
                      "a" #'describe-bindings
                      "RET" #'view-order-manuals)))
             (list (lookup-key m [1])
                   (lookup-key m [97])
                   (lookup-key m [13])))"#,
    )
    .into_iter()
    .next()
    .expect("bootstrap define-keymap result");
    assert_eq!(
        result,
        "OK (about-emacs describe-bindings view-order-manuals)"
    );
}

#[test]
fn bootstrap_defvar_keymap_and_fset_share_populated_keymap_object() {
    let result = bootstrap_eval_all(
        r#"(progn
             (defvar-keymap test-help-map
               "C-a" #'about-emacs
               "a" #'describe-bindings)
             (fset 'test-help-command test-help-map)
             (list (lookup-key test-help-map [1])
                   (lookup-key (symbol-function 'test-help-command) [1])
                   (eq test-help-map (symbol-function 'test-help-command))))"#,
    )
    .into_iter()
    .next()
    .expect("bootstrap defvar-keymap result");
    assert_eq!(result, "OK (about-emacs about-emacs t)");
}

#[test]
fn bootstrap_runtime_help_map_and_help_command_are_populated_like_gnu() {
    let result = bootstrap_eval_all(
        r#"(list (lookup-key help-map [1])
                 (lookup-key help-map [97])
                 (lookup-key (current-global-map) [8])
                 (lookup-key (current-global-map) [8 1])
                 (lookup-key (symbol-function 'help-command) [1]))"#,
    )
    .into_iter()
    .next()
    .expect("bootstrap help-map result");
    assert_eq!(
        result,
        "OK (about-emacs apropos-command help-command about-emacs about-emacs)"
    );
}

#[test]
fn command_modes_extracts_modes_and_preserves_arity_checks() {
    assert_eq!(eval_one("(command-modes 'ignore)"), "OK nil");
    assert_eq!(eval_one("(command-modes nil)"), "OK nil");
    assert_eq!(eval_one("(command-modes 0)"), "OK nil");
    assert_eq!(eval_one("(command-modes \"ignore\")"), "OK nil");
    assert_eq!(
        eval_one("(command-modes '(lambda () (interactive)))"),
        "OK nil"
    );
    assert_eq!(
        eval_one("(command-modes '(lambda () (interactive \"p\" text-mode prog-mode) t))"),
        "OK (text-mode prog-mode)"
    );
    assert_eq!(eval_one("(command-modes '(lambda (x) x))"), "OK nil");
    assert_eq!(
        eval_one(
            "(progn
               (fset 'vm-command-modes-target '(lambda () t))
               (fset 'vm-command-modes-alias 'vm-command-modes-target)
               (put 'vm-command-modes-alias 'command-modes '(foo-mode bar-mode))
               (command-modes 'vm-command-modes-alias))"
        ),
        "OK (foo-mode bar-mode)"
    );
    assert_eq!(
        eval_one(
            "(let ((f (make-byte-code '() \"\" [] 0 nil [nil '(rust-ts-mode c-mode)])))
               (fset 'vm-command-modes-bytecode f)
               (command-modes 'vm-command-modes-bytecode))"
        ),
        "OK (rust-ts-mode c-mode)"
    );
    assert_eq!(
        eval_one(
            r#"(condition-case err
                   (command-modes)
                 (wrong-number-of-arguments (car err)))"#
        ),
        "OK wrong-number-of-arguments"
    );
    assert_eq!(
        eval_one(
            r#"(condition-case err
                   (command-modes 'ignore nil)
                 (wrong-number-of-arguments (car err)))"#
        ),
        "OK wrong-number-of-arguments"
    );
}

#[test]
fn command_remapping_nil_and_keymap_type_checks() {
    assert_eq!(eval_one("(command-remapping 'ignore)"), "OK nil");
    assert_eq!(eval_one("(command-remapping 'ignore nil)"), "OK nil");
    assert_eq!(eval_one("(command-remapping 'ignore nil nil)"), "OK nil");
    assert_eq!(
        eval_one("(command-remapping 'ignore nil (list 'keymap))"),
        "OK nil"
    );
    assert_eq!(
        eval_one("(command-remapping 'ignore nil '(1 2 3))"),
        "OK nil"
    );
    assert_eq!(eval_one("(command-remapping 'ignore nil '(foo))"), "OK nil");
    assert_eq!(
        eval_one("(command-remapping 'ignore nil '(foo bar))"),
        "OK nil"
    );
    assert_eq!(eval_one("(command-remapping nil)"), "OK nil");
    assert_eq!(eval_one("(command-remapping 0)"), "OK nil");
    assert_eq!(eval_one("(command-remapping \"ignore\")"), "OK nil");
    assert_eq!(
        eval_one("(command-remapping '(lambda () (interactive)))"),
        "OK nil"
    );
    assert_eq!(eval_one("(command-remapping '(lambda (x) x))"), "OK nil");
    assert_eq!(eval_one("(command-remapping 'ignore '(x) nil)"), "OK nil");
    assert_eq!(
        eval_one("(command-remapping 'ignore '(x) (make-sparse-keymap))"),
        "OK nil"
    );
    assert_eq!(
        eval_one("(command-remapping 'ignore [x] (make-sparse-keymap))"),
        "OK nil"
    );

    assert_eq!(
        eval_one(
            r#"(condition-case err
                   (command-remapping 'ignore nil t)
                 (wrong-type-argument (list (car err) (cdr err))))"#
        ),
        "OK (wrong-type-argument (keymapp t))"
    );
    assert_eq!(
        eval_one(
            r#"(condition-case err
                   (command-remapping 'ignore nil [1])
                 (wrong-type-argument (list (car err) (cdr err))))"#
        ),
        "OK (wrong-type-argument (keymapp [1]))"
    );
    assert_eq!(
        eval_one(
            r#"(condition-case err
                   (command-remapping 'ignore nil "x")
                 (wrong-type-argument (list (car err) (cdr err))))"#
        ),
        "OK (wrong-type-argument (keymapp \"x\"))"
    );
    assert_eq!(
        eval_one(
            r#"(condition-case err
                   (command-remapping 'ignore nil 1)
                 (wrong-type-argument (list (car err) (cdr err))))"#
        ),
        "OK (wrong-type-argument (keymapp 1))"
    );
    assert_eq!(
        eval_one(
            r#"(condition-case err
                   (command-remapping 'ignore nil 'foo)
                 (wrong-type-argument (list (car err) (cdr err))))"#
        ),
        "OK (wrong-type-argument (keymapp foo))"
    );

    assert_eq!(
        eval_one(
            r#"(condition-case err
                   (command-remapping)
                 (wrong-number-of-arguments (car err)))"#
        ),
        "OK wrong-number-of-arguments"
    );
    assert_eq!(
        eval_one(
            r#"(condition-case err
                   (command-remapping 'ignore nil nil nil)
                 (wrong-number-of-arguments (car err)))"#
        ),
        "OK wrong-number-of-arguments"
    );
}

#[test]
fn command_remapping_integer_position_range_and_ordering_semantics() {
    assert_eq!(
        eval_one(
            r#"(with-temp-buffer
                 (let ((err (condition-case e
                                (command-remapping 'ignore 0)
                              (error e))))
                   (list (car err) (bufferp (nth 1 err)) (nth 2 err))))"#
        ),
        "OK (args-out-of-range t 0)"
    );
    assert_eq!(
        eval_one(
            r#"(with-temp-buffer
                 (let ((err (condition-case e
                                (command-remapping 'ignore -1)
                              (error e))))
                   (list (car err) (bufferp (nth 1 err)) (nth 2 err))))"#
        ),
        "OK (args-out-of-range t -1)"
    );
    assert_eq!(
        eval_one(
            r#"(with-temp-buffer
                 (let ((err (condition-case e
                                (command-remapping 'ignore 2)
                              (error e))))
                   (list (car err) (bufferp (nth 1 err)) (nth 2 err))))"#
        ),
        "OK (args-out-of-range t 2)"
    );
    assert_eq!(
        eval_one(
            r#"(with-temp-buffer
                 (let ((m (make-sparse-keymap)))
                   (define-key m [remap ignore] 'self-insert-command)
                   (list
                    (command-remapping 'ignore 1 m)
                    (command-remapping 'ignore t m)
                    (command-remapping 'ignore 'foo m)
                    (command-remapping 'ignore "x" m)
                    (command-remapping 'ignore [1] m)
                    (command-remapping 'ignore '(1) m)
                    (command-remapping 'ignore 1.5 m)
                    (command-remapping 'ignore (copy-marker (point)) m))))"#
        ),
        "OK (self-insert-command self-insert-command self-insert-command self-insert-command self-insert-command self-insert-command self-insert-command self-insert-command)"
    );
    assert_eq!(
        eval_one(
            r#"(with-temp-buffer
                 (let ((err (condition-case e
                                (command-remapping nil 0)
                              (error e))))
                   (list (car err) (bufferp (nth 1 err)) (nth 2 err))))"#
        ),
        "OK (args-out-of-range t 0)"
    );
    assert_eq!(
        eval_one(
            r#"(with-temp-buffer
                 (condition-case e
                     (command-remapping 'ignore 0 t)
                   (error e)))"#
        ),
        "OK (wrong-type-argument keymapp t)"
    );
    assert_eq!(
        eval_one("(with-temp-buffer (command-remapping 0 0))"),
        "OK nil"
    );
}

#[test]
fn command_remapping_resolves_remap_bindings_on_keymap_handles() {
    assert_eq!(
        eval_one(
            r#"(let ((m (make-sparse-keymap)))
                 (define-key m [remap ignore] 'self-insert-command)
                 (command-remapping 'ignore nil m))"#
        ),
        "OK self-insert-command"
    );
    assert_eq!(
        eval_one(
            r#"(let ((m (make-sparse-keymap)))
                 (define-key m [remap ignore] [x])
                 (command-remapping 'ignore nil m))"#
        ),
        "OK [x]"
    );
    assert_eq!(
        eval_one(
            r#"(let ((m (make-sparse-keymap)))
                 (define-key m [remap ignore] 1)
                 (command-remapping 'ignore nil m))"#
        ),
        "OK nil"
    );
    assert_eq!(
        eval_one(
            r#"(let ((m (make-sparse-keymap)))
                 (define-key m [remap ignore] t)
                 (command-remapping 'ignore nil m))"#
        ),
        "OK nil"
    );
    assert_eq!(
        eval_one(
            r#"(let ((m (make-sparse-keymap)))
                 (define-key m [remap ignore] '(menu-item "x" ignore))
                 (command-remapping 'ignore nil m))"#
        ),
        "OK ignore"
    );
    assert_eq!(
        eval_one(
            r#"(let ((m (make-sparse-keymap)))
                 (define-key m [remap ignore] '(menu-item "x" 1))
                 (command-remapping 'ignore nil m))"#
        ),
        "OK nil"
    );
    assert_eq!(
        eval_one(
            r#"(let ((m (make-sparse-keymap)))
                 (define-key m [remap ignore] '(menu-item))
                 (command-remapping 'ignore nil m))"#
        ),
        "OK (menu-item)"
    );
    assert_eq!(
        eval_one(
            r#"(let ((m (make-sparse-keymap)))
                 (define-key m [remap ignore] 'self-insert-command)
                 (command-remapping 0 nil m))"#
        ),
        "OK nil"
    );
    assert_eq!(
        eval_one(
            r#"(let ((m (make-sparse-keymap)))
                 (define-key (current-global-map) [remap ignore] 'self-insert-command)
                 (command-remapping 'ignore))"#
        ),
        "OK self-insert-command"
    );
}

#[test]
fn command_remapping_global_map_remap_binding() {
    assert_eq!(
        eval_one(
            r#"(let ((g (make-sparse-keymap))
                     (l (make-sparse-keymap)))
                 (use-global-map g)
                 (use-local-map l)
                 (define-key g [remap ignore] 'self-insert-command)
                 (list (keymapp (current-global-map))
                       (lookup-key (current-global-map) [remap ignore])
                       (command-remapping 'ignore nil (current-global-map))
                       (command-remapping 'ignore)))"#
        ),
        "OK (t self-insert-command self-insert-command self-insert-command)"
    );
}

#[test]
fn command_remapping_prefers_local_map_when_keymap_omitted_or_nil() {
    assert_eq!(
        eval_one(
            r#"(let ((g (make-sparse-keymap))
                     (l (make-sparse-keymap)))
                 (use-global-map g)
                 (use-local-map l)
                 (define-key l [remap ignore] 'self-insert-command)
                 (command-remapping 'ignore))"#
        ),
        "OK self-insert-command"
    );
    assert_eq!(
        eval_one(
            r#"(let ((g (make-sparse-keymap))
                     (l (make-sparse-keymap)))
                 (use-global-map g)
                 (use-local-map l)
                 (define-key l [remap ignore] 'self-insert-command)
                 (command-remapping 'ignore nil nil))"#
        ),
        "OK self-insert-command"
    );
    assert_eq!(
        eval_one(
            r#"(let ((g (make-sparse-keymap))
                     (l (make-sparse-keymap)))
                 (use-global-map g)
                 (use-local-map l)
                 (define-key g [remap ignore] 'forward-char)
                 (define-key l [remap ignore] 'self-insert-command)
                 (command-remapping 'ignore))"#
        ),
        "OK self-insert-command"
    );
    assert_eq!(
        eval_one(
            r#"(let ((g (make-sparse-keymap))
                     (l (make-sparse-keymap)))
                 (use-global-map g)
                 (use-local-map l)
                 (define-key g [remap ignore] 'self-insert-command)
                 (command-remapping 'ignore))"#
        ),
        "OK self-insert-command"
    );
    assert_eq!(
        eval_one(
            r#"(with-temp-buffer
                 (let ((g (make-sparse-keymap))
                       (l (make-sparse-keymap)))
                   (use-global-map g)
                   (use-local-map l)
                   (define-key l [remap ignore] 'self-insert-command)
                   (command-remapping 'ignore (point-min))))"#
        ),
        "OK self-insert-command"
    );
}

#[test]
fn switch_to_buffer_restores_buffer_local_map_for_key_lookup() {
    assert_eq!(
        eval_one(
            r#"(let* ((b1 (get-buffer-create "*km-a*"))
                      (b2 (get-buffer-create "*km-b*"))
                      (m1 (make-sparse-keymap))
                      (m2 (make-sparse-keymap)))
                 (define-key m1 "a" 'forward-char)
                 (define-key m2 "h" 'describe-mode)
                 (set-buffer b1)
                 (use-local-map m1)
                 (set-buffer b2)
                 (use-local-map m2)
                 (set-buffer b1)
                 (list (eq (current-local-map) m1)
                       (key-binding "a")
                       (key-binding "h")))"#
        ),
        "OK (t forward-char self-insert-command)"
    );
}

#[test]
fn command_remapping_checks_minor_mode_maps_before_local_and_global() {
    assert_eq!(
        eval_one(
            r#"(let ((g (make-sparse-keymap))
                     (m (make-sparse-keymap))
                     (minor-mode-map-alist nil)
                     (demo-mode t))
                 (use-global-map g)
                 (define-key m [remap ignore] 'self-insert-command)
                 (setq minor-mode-map-alist (list (cons 'demo-mode m)))
                 (command-remapping 'ignore))"#
        ),
        "OK self-insert-command"
    );
    assert_eq!(
        eval_one(
            r#"(let ((g (make-sparse-keymap))
                     (l (make-sparse-keymap))
                     (m (make-sparse-keymap))
                     (minor-mode-map-alist nil)
                     (demo-mode t))
                 (use-global-map g)
                 (use-local-map l)
                 (define-key m [remap ignore] 'forward-char)
                 (define-key l [remap ignore] 'self-insert-command)
                 (setq minor-mode-map-alist (list (cons 'demo-mode m)))
                 (command-remapping 'ignore))"#
        ),
        "OK forward-char"
    );
    assert_eq!(
        eval_one(
            r#"(let ((g (make-sparse-keymap))
                     (l (make-sparse-keymap))
                     (m (make-sparse-keymap))
                     (minor-mode-overriding-map-alist nil)
                     (minor-mode-map-alist nil)
                     (demo-mode t))
                 (use-global-map g)
                 (use-local-map l)
                 (define-key m [remap ignore] 'forward-char)
                 (define-key l [remap ignore] 'self-insert-command)
                 (setq minor-mode-overriding-map-alist (list (cons 'demo-mode m)))
                 (setq minor-mode-map-alist (list (cons 'demo-mode l)))
                 (command-remapping 'ignore))"#
        ),
        "OK forward-char"
    );
    assert_eq!(
        eval_one(
            r#"(let ((minor-mode-map-alist '((demo-mode . 999999)))
                     (demo-mode t))
                 (command-remapping 'ignore))"#
        ),
        "OK nil"
    );
}

#[test]
fn command_remapping_resolves_remap_bindings_on_lisp_keymaps() {
    assert_eq!(
        eval_one(
            "(command-remapping 'ignore nil '(keymap (remap keymap (ignore . self-insert-command))))"
        ),
        "OK self-insert-command"
    );
    assert_eq!(
        eval_one(
            "(command-remapping 'ignore nil '(keymap (remap keymap (ignore menu-item \"x\" ignore))))"
        ),
        "OK ignore"
    );
    assert_eq!(
        eval_one("(command-remapping 'ignore nil '(keymap (remap keymap (ignore . 1))))"),
        "OK nil"
    );
    assert_eq!(
        eval_one("(command-remapping 'ignore nil '(keymap (remap keymap (ignore . t))))"),
        "OK nil"
    );
    assert_eq!(
        eval_one("(command-remapping 'ignore nil '(keymap (remap keymap (ignore))))"),
        "OK nil"
    );
    assert_eq!(
        eval_one("(command-remapping 'ignore nil '(keymap (remap keymap (ignore . [x]))))"),
        "OK [x]"
    );
    assert_eq!(
        eval_one("(command-remapping 'ignore nil '(keymap (remap keymap (ignore . \"x\"))))"),
        "OK \"x\""
    );
    assert_eq!(
        eval_one(
            "(command-remapping 'not-bound nil '(keymap (remap keymap (not-bound . self-insert-command))))"
        ),
        "OK self-insert-command"
    );
    assert_eq!(
        eval_one(
            "(command-remapping 'ignore nil '(keymap (remap keymap (foo . self-insert-command))))"
        ),
        "OK nil"
    );
}
