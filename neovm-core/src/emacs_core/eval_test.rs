use super::*;
use crate::emacs_core::error::Flow;
use crate::emacs_core::eval::{ConditionFrame, ResumeTarget, SpecBinding};
use crate::emacs_core::format_eval_result;
use crate::heap_types::LispString;
use crate::test_utils::{
    eval_with_ldefs_boot_autoloads, load_minimal_gnu_backquote_runtime, runtime_startup_context,
    runtime_startup_eval_all,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn eval_one(src: &str) -> String {
    let mut ev = Context::new();
    let result = ev.eval_str(src);
    format_eval_result(&result)
}

fn eval_all(src: &str) -> Vec<String> {
    let mut ev = Context::new();
    let forms = crate::emacs_core::value_reader::read_all(src).expect("parse");
    // Root all parsed forms across the eval loop. Without rooting,
    // any intervening GC reclaims the cons cells in the unrooted
    // `forms` Vec<Value> (malloc heap, invisible to conservative
    // stack scanning).
    let roots = ev.save_specpdl_roots();
    for form in &forms {
        ev.push_specpdl_root(*form);
    }
    let result = forms
        .iter()
        .map(|form| {
            let result = ev.eval_form(*form);
            format_eval_result(&result)
        })
        .collect();
    ev.restore_specpdl_roots(roots);
    result
}

fn eval_one_with_frame(src: &str) -> String {
    let mut ev = Context::new();
    let buf = ev.buffers.create_buffer("*scratch*");
    ev.buffers.set_current(buf);
    ev.frames.create_frame("F1", 800, 600, buf);
    let result = ev.eval_str(src);
    format_eval_result(&result)
}

fn eval_all_with_subr(src: &str) -> Vec<String> {
    let mut ev = Context::new();
    load_minimal_gnu_backquote_runtime(&mut ev);
    ev.eval_str_each(&src)
        .iter()
        .map(format_eval_result)
        .collect()
}

fn eval_one_with_subr(src: &str) -> String {
    eval_all_with_subr(src).into_iter().next().expect("result")
}

fn bootstrap_eval_all(src: &str) -> Vec<String> {
    runtime_startup_eval_all(src)
}

fn bootstrap_eval_one(src: &str) -> String {
    bootstrap_eval_all(src).into_iter().next().expect("result")
}

#[test]
fn symbols_with_pos_enabled_makes_lisp_comparison_primitives_transparent() {
    let result = eval_one(
        r#"(progn
             (setq symbols-with-pos-enabled t)
             (let* ((head (position-symbol 'indent 42))
                    (items (list 'indent))
                    (alist (list (cons 'indent 'ok)))
                    (rlist (list (cons 'ok 'indent))))
               (list
                (eq head 'indent)
                (eql head 'indent)
                (equal head 'indent)
                (memq head items)
                (memql head items)
                (member head items)
                (assq head alist)
                (assoc head alist)
                (rassq head rlist)
                (rassoc head rlist)
                (delq head (list 'a 'indent 'b))
                (delete head (list 'a 'indent 'b)))))"#,
    );

    assert_eq!(
        result,
        "OK (t t t (indent) (indent) (indent) (indent . ok) (indent . ok) (ok . indent) (ok . indent) (a b) (a b))"
    );
}

#[test]
fn keywordp_treats_positioned_keywords_like_gnu_when_enabled() {
    let result = eval_one(
        r#"(let ((pos-kw (position-symbol :neo-keyword 42)))
             (list
              (let ((symbols-with-pos-enabled t))
                (list (symbolp pos-kw) (keywordp pos-kw) (eq pos-kw :neo-keyword)))
              (let ((symbols-with-pos-enabled nil))
                (list (symbolp pos-kw) (keywordp pos-kw) (eq pos-kw :neo-keyword)))))"#,
    );

    assert_eq!(result, "OK ((t t t) (nil nil nil))");
}

#[test]
fn symbols_with_pos_enabled_makes_hash_table_keys_transparent() {
    let result = eval_one(
        r#"(progn
             (setq symbols-with-pos-enabled t)
             (let* ((head (position-symbol 'indent 42))
                    (eqtab (make-hash-table :test 'eq))
                    (eqltab (make-hash-table :test 'eql))
                    (equaltab (make-hash-table :test 'equal)))
               (puthash head 'pos eqtab)
               (puthash 'indent 'bare eqtab)
               (puthash head 'pos eqltab)
               (puthash 'indent 'bare eqltab)
               (puthash head 'pos equaltab)
               (puthash 'indent 'bare equaltab)
               (let ((before
                      (list
                       (hash-table-count eqtab)
                       (gethash head eqtab)
                       (gethash 'indent eqtab)
                       (hash-table-count eqltab)
                       (gethash head eqltab)
                       (gethash 'indent eqltab)
                       (hash-table-count equaltab)
                       (gethash head equaltab)
                       (gethash 'indent equaltab))))
                 (remhash head eqtab)
                 (remhash head eqltab)
                 (remhash head equaltab)
                 (append before
                         (list
                          (hash-table-count eqtab)
                          (hash-table-count eqltab)
                          (hash-table-count equaltab))))))"#,
    );

    assert_eq!(result, "OK (1 bare bare 1 bare bare 1 bare bare 0 0 0)");
}

#[test]
fn get_honors_overriding_plist_environment() {
    let result = eval_one(
        r#"(progn
             (put 'neo-plist-probe 'pcase-macroexpander 'obarray)
             (list
              (let ((overriding-plist-environment
                     '((neo-plist-probe pcase-macroexpander override))))
                (get 'neo-plist-probe 'pcase-macroexpander))
              (let ((overriding-plist-environment
                     '((neo-plist-probe pcase-macroexpander nil))))
                (get 'neo-plist-probe 'pcase-macroexpander))))"#,
    );

    assert_eq!(result, "OK (override obarray)");
}

#[test]
fn get_and_put_accept_non_symbol_property_keys() {
    let result = eval_one(
        r#"(let ((key (copy-sequence "a")))
             (put 'neo-nonsymbol-prop key 7)
             (list
              (get 'neo-nonsymbol-prop key)
              (get 'neo-nonsymbol-prop (copy-sequence "a"))
              (symbol-plist 'neo-nonsymbol-prop)))"#,
    );

    assert_eq!(result, "OK (7 nil (\"a\" 7))");
}

#[test]
fn symbol_with_pos_property_keys_follow_gnu_eq_rules() {
    let result = eval_one(
        r#"(progn
             (put 'neo-swp-prop 'a 'bare)
             (list
              (let ((symbols-with-pos-enabled t))
                (put 'neo-swp-prop (position-symbol 'a 1) 'pos)
                (list
                 (get 'neo-swp-prop 'a)
                 (get 'neo-swp-prop (position-symbol 'a 2))
                 (length (symbol-plist 'neo-swp-prop))))
              (progn
                (setplist 'neo-swp-prop nil)
                (put 'neo-swp-prop 'a 'bare)
                (let ((symbols-with-pos-enabled nil))
                  (put 'neo-swp-prop (position-symbol 'a 1) 'pos)
                  (list
                   (get 'neo-swp-prop 'a)
                   (get 'neo-swp-prop (position-symbol 'a 2))
                   (length (symbol-plist 'neo-swp-prop)))))))"#,
    );

    assert_eq!(result, "OK ((pos pos 2) (bare nil 4))");
}

#[test]
fn skip_debugger_matches_raw_unibyte_ignored_error_regex() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    crate::emacs_core::errors::init_standard_errors(&mut ev.obarray);
    let raw = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![0xFF]));
    ev.obarray
        .set_symbol_value("debug-ignored-errors", Value::list(vec![raw]));
    let sig = match crate::emacs_core::error::signal("error", vec![raw]) {
        Flow::Signal(sig) => sig,
        other => panic!("expected signal flow, got {other:?}"),
    };
    let conditions = ev.signal_conditions_value(&sig);
    assert!(
        ev.skip_debugger(&sig, &conditions)
            .expect("skip_debugger should evaluate")
    );
}

fn install_minimal_special_event_command_runtime(ev: &mut Context) {
    ev.eval_str(
        r#"
(fset 'command-execute
      (lambda (cmd &optional _record keys _special)
        (funcall cmd (aref keys 0))))
(fset 'handle-delete-frame
      (lambda (event)
        (setq neo-last-delete-frame-event event)
        nil))
(fset 'handle-focus-in
      (lambda (event)
        (internal-handle-focus-in event)))
(fset 'handle-focus-out
      (lambda (_event)
        nil))
"#,
    )
    .expect("eval forms");
}

fn find_bin(name: &str) -> String {
    for dir in &["/bin", "/usr/bin", "/run/current-system/sw/bin"] {
        let path = format!("{}/{}", dir, name);
        if std::path::Path::new(&path).exists() {
            return path;
        }
    }
    if let Ok(output) = std::process::Command::new("which").arg(name).output()
        && output.status.success()
    {
        return String::from_utf8_lossy(&output.stdout).trim().to_string();
    }
    name.to_string()
}

fn gnu_timer_after(delay: Duration, callback: &str) -> Value {
    let when = SystemTime::now()
        .checked_add(delay)
        .expect("timer deadline should fit in system time")
        .duration_since(UNIX_EPOCH)
        .expect("timer deadline should be after unix epoch");
    let secs = when.as_secs() as i64;

    Value::vector(vec![
        Value::NIL,
        Value::fixnum(secs >> 16),
        Value::fixnum(secs & 0xFFFF),
        Value::fixnum(when.subsec_micros() as i64),
        Value::NIL,
        Value::symbol(callback),
        Value::NIL,
        Value::NIL,
        Value::fixnum(0),
        Value::NIL,
    ])
}

fn gnu_timer_before(delay: Duration, callback: &str) -> Value {
    let when = SystemTime::now()
        .checked_sub(delay)
        .expect("timer deadline should fit in system time")
        .duration_since(UNIX_EPOCH)
        .expect("timer deadline should be after unix epoch");
    let secs = when.as_secs() as i64;

    Value::vector(vec![
        Value::NIL,
        Value::fixnum(secs >> 16),
        Value::fixnum(secs & 0xFFFF),
        Value::fixnum(when.subsec_micros() as i64),
        Value::NIL,
        Value::symbol(callback),
        Value::NIL,
        Value::NIL,
        Value::fixnum(0),
        Value::NIL,
    ])
}

fn gnu_idle_timer_after(delay: Duration, callback: &str) -> Value {
    let secs = delay.as_secs() as i64;

    Value::vector(vec![
        Value::NIL,
        Value::fixnum(secs >> 16),
        Value::fixnum(secs & 0xFFFF),
        Value::fixnum(delay.subsec_micros() as i64),
        Value::NIL,
        Value::symbol(callback),
        Value::NIL,
        Value::symbol("idle"),
        Value::fixnum(0),
        Value::NIL,
    ])
}

#[derive(Clone, Default)]
struct RecordingDisplayHost {
    primary_size: Option<GuiFrameHostSize>,
    opening_frame_pending: bool,
}

impl RecordingDisplayHost {
    fn opening_with_primary_size(width: u32, height: u32) -> Self {
        Self {
            primary_size: Some(GuiFrameHostSize { width, height }),
            opening_frame_pending: true,
        }
    }
}

impl DisplayHost for RecordingDisplayHost {
    fn realize_gui_frame(&mut self, _request: GuiFrameHostRequest) -> Result<(), String> {
        Ok(())
    }

    fn resize_gui_frame(&mut self, _request: GuiFrameHostRequest) -> Result<(), String> {
        Ok(())
    }

    fn opening_gui_frame_pending(&self) -> bool {
        self.opening_frame_pending
    }

    fn current_primary_window_size(&self) -> Option<GuiFrameHostSize> {
        self.primary_size
    }
}

struct CursorBlinkRecordingDisplayHost {
    calls: Rc<RefCell<Vec<(bool, u32)>>>,
}

impl DisplayHost for CursorBlinkRecordingDisplayHost {
    fn realize_gui_frame(&mut self, _request: GuiFrameHostRequest) -> Result<(), String> {
        Ok(())
    }

    fn resize_gui_frame(&mut self, _request: GuiFrameHostRequest) -> Result<(), String> {
        Ok(())
    }

    fn set_cursor_blink(&mut self, enabled: bool, interval_ms: u32) -> Result<(), String> {
        self.calls.borrow_mut().push((enabled, interval_ms));
        Ok(())
    }
}

#[test]
fn eval_with_explicit_lexenv_restores_outer_lexenv() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one("(let ((x 41)) (list (eval 'x '((x . 7))) x))"),
        "OK (7 41)"
    );
}

#[test]
fn neomacs_set_cursor_blink_forwards_to_display_host() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let calls = Rc::new(RefCell::new(Vec::new()));
    ev.set_display_host(Box::new(CursorBlinkRecordingDisplayHost {
        calls: Rc::clone(&calls),
    }));

    ev.eval_str("(neomacs-set-cursor-blink nil 0.25)")
        .expect("set cursor blink should evaluate");

    assert_eq!(*calls.borrow(), vec![(false, 250)]);
}

#[test]
fn eval_with_explicit_lexenv_shadows_special_reads_and_setq() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            "(progn
               (defvar ev-explicit-special 1)
               (list
                 (eval '(progn (setq ev-explicit-special 9) ev-explicit-special)
                       '((ev-explicit-special . 7)))
                 ev-explicit-special))"
        ),
        "OK (9 1)"
    );
}

#[test]
fn source_cons_macro_form_expands_via_value_expansion_path() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    // Verify that the eval loop's macro dispatch correctly handles a
    // macro defined as a `(macro . FN)` cons cell (mirrors GNU
    // eval.c:2730 — a "macro function" cell that wraps a lambda).
    // The expansion itself must return the body unchanged (the
    // lambda is identity), so evaluating the expansion yields 3.
    //
    // Note: the previous version of this test asserted on
    // `macro_cache_misses` / `macro_cache_hits`, but those counters
    // track the `runtime_macro_expansion_cache` (used only by the
    // `macroexpand` builtin during file loading), not the eval
    // loop's macro dispatch. Reinstating those assertions would
    // require exercising a different code path entirely.
    ev.eval_str(
        "(fset 'source-cache-macro
                  (cons 'macro
                        (lambda (x)
                          x)))",
    )
    .expect("install macro");

    let first = ev.eval_str("(source-cache-macro (+ 1 2))");
    let second = ev.eval_str("(source-cache-macro (+ 1 2))");

    assert_eq!(format_eval_result(&first), "OK 3");
    assert_eq!(format_eval_result(&second), "OK 3");
}

#[test]
fn recursive_edit_without_input_receiver_still_runs_noninteractive_top_level() {
    crate::test_utils::init_test_tracing();

    let mut ev = Context::new();
    ev.set_variable("noninteractive", Value::T);
    let top_level = crate::emacs_core::value_reader::read_all(
        "(progn (setq neomacs--batch-no-input-probe 42) nil)",
    )
    .expect("parse top-level form")
    .into_iter()
    .next()
    .expect("top-level form");
    ev.set_variable("top-level", top_level);

    let result = ev.recursive_edit();
    assert!(result.is_ok(), "batch recursive edit should exit cleanly");
    assert_eq!(
        ev.shutdown_request(),
        Some(crate::emacs_core::eval::ShutdownRequest {
            exit_code: 0,
            restart: false,
        })
    );
    assert_eq!(
        ev.obarray().symbol_value("neomacs--batch-no-input-probe"),
        Some(&Value::fixnum(42))
    );
}

#[test]
fn clear_top_level_eval_state_discards_stale_named_call_cache_entries() {
    crate::test_utils::init_test_tracing();

    let mut ev = Context::new();
    ev.eval_str(r#"(autoload 'neomacs--stale-call-target "dummy-file" nil t)"#)
        .expect("autoload registration should succeed");
    let sym = intern("neomacs--stale-call-target");
    let epoch = ev.obarray.function_epoch();

    ev.named_call_cache.insert(
        sym,
        NamedCallCacheEntry {
            function_epoch: epoch,
            target: NamedCallTarget::Void,
        },
    );
    assert!(matches!(
        ev.resolve_named_call_target_by_id(sym),
        NamedCallTarget::Void
    ));

    ev.clear_top_level_eval_state();

    match ev.resolve_named_call_target_by_id(sym) {
        NamedCallTarget::Obarray(function) => {
            assert!(
                crate::emacs_core::autoload::is_autoload_value(&function),
                "expected autoload function cell, got {function}"
            );
        }
        other => panic!("expected autoload-backed named call target, got {other:?}"),
    }
}

#[test]
fn runtime_macro_cache_hits_across_equivalent_explicit_environments() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    // The runtime macro expansion cache is only active when
    // `load-in-progress` is truthy (mirroring GNU, where elisp
    // macros are cached only while loading `.el`/`.elc` files).
    // Enable it explicitly for this unit test.
    ev.set_variable("load-in-progress", Value::T);
    // Seed the macro used by the cache probe. The test relies on a
    // Lisp-defined macro function whose body does a `setq` side
    // effect so we can observe whether it ran (miss) or not (hit).
    ev.eval_str("(defvar runtime-cache-count 0)")
        .expect("defvar runtime-cache-count");
    ev.eval_str(
        "(defalias 'runtime-cache-macro
           (cons 'macro
                 (lambda (form)
                   (setq runtime-cache-count (1+ runtime-cache-count))
                   form)))",
    )
    .expect("install runtime-cache-macro");
    let definition = ev
        .obarray()
        .symbol_function("runtime-cache-macro")
        .expect("runtime-cache-macro definition");
    let arg = Value::list(vec![Value::symbol("+"), Value::fixnum(1), Value::fixnum(2)]);
    let form = Value::list(vec![Value::symbol("runtime-cache-macro"), arg]);
    let env1 = Value::list(vec![Value::cons(
        Value::symbol("context"),
        Value::symbol("marker"),
    )]);
    let env2 = Value::list(vec![Value::cons(
        Value::symbol("context"),
        Value::symbol("marker"),
    )]);

    let hits0 = ev.macro_cache_hits;
    let misses0 = ev.macro_cache_misses;

    let first = ev
        .expand_macro_for_macroexpand(form, definition, vec![arg], Some(env1))
        .expect("first runtime macro expansion");
    let second = ev
        .expand_macro_for_macroexpand(form, definition, vec![arg], Some(env2))
        .expect("second runtime macro expansion");

    assert!(equal_value(&first, &arg, 0));
    assert!(equal_value(&second, &arg, 0));
    assert_eq!(ev.macro_cache_misses - misses0, 1);
    assert_eq!(ev.macro_cache_hits - hits0, 1);
}

#[test]
fn runtime_macro_cache_handles_raw_unibyte_strings_in_environment() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_variable("load-in-progress", Value::T);
    ev.eval_str("(defvar runtime-cache-count 0)")
        .expect("defvar runtime-cache-count");
    ev.eval_str(
        "(defalias 'runtime-cache-macro
           (cons 'macro
                 (lambda (form)
                   (setq runtime-cache-count (1+ runtime-cache-count))
                   form)))",
    )
    .expect("install runtime-cache-macro");
    let definition = ev
        .obarray()
        .symbol_function("runtime-cache-macro")
        .expect("runtime-cache-macro definition");
    let arg = Value::list(vec![Value::symbol("+"), Value::fixnum(1), Value::fixnum(2)]);
    let form = Value::list(vec![Value::symbol("runtime-cache-macro"), arg]);
    let raw_unibyte = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![0xFF]));
    let env1 = Value::list(vec![Value::cons(Value::symbol("context"), raw_unibyte)]);
    let env2 = Value::list(vec![Value::cons(Value::symbol("context"), raw_unibyte)]);

    let hits0 = ev.macro_cache_hits;
    let misses0 = ev.macro_cache_misses;

    let first = ev
        .expand_macro_for_macroexpand(form, definition, vec![arg], Some(env1))
        .expect("first runtime macro expansion");
    let second = ev
        .expand_macro_for_macroexpand(form, definition, vec![arg], Some(env2))
        .expect("second runtime macro expansion");

    assert!(equal_value(&first, &arg, 0));
    assert!(equal_value(&second, &arg, 0));
    assert_eq!(ev.macro_cache_misses - misses0, 1);
    assert_eq!(ev.macro_cache_hits - hits0, 1);
    assert_eq!(
        ev.obarray()
            .symbol_value("runtime-cache-count")
            .copied()
            .unwrap_or(Value::NIL),
        Value::fixnum(1)
    );
}

#[test]
fn runtime_macro_cache_handles_raw_unibyte_string_arguments() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_variable("load-in-progress", Value::T);
    ev.eval_str("(defvar runtime-cache-bytes-count 0)")
        .expect("defvar runtime-cache-bytes-count");
    ev.eval_str(
        "(defalias 'runtime-cache-bytes-macro
           (cons 'macro
                 (lambda (form)
                   (setq runtime-cache-bytes-count
                         (1+ runtime-cache-bytes-count))
                   form)))",
    )
    .expect("install runtime-cache-bytes-macro");

    let definition = ev
        .obarray()
        .symbol_function("runtime-cache-bytes-macro")
        .expect("runtime-cache-bytes-macro definition");
    let arg = Value::heap_string(LispString::from_unibyte(vec![0xFF]));
    let form = Value::list(vec![Value::symbol("runtime-cache-bytes-macro"), arg]);

    let hits0 = ev.macro_cache_hits;
    let misses0 = ev.macro_cache_misses;

    let first = ev
        .expand_macro_for_macroexpand(form, definition, vec![arg], None)
        .expect("first raw-byte runtime macro expansion");
    let second = ev
        .expand_macro_for_macroexpand(form, definition, vec![arg], None)
        .expect("second raw-byte runtime macro expansion");

    assert!(equal_value(&first, &arg, 0));
    assert!(equal_value(&second, &arg, 0));
    assert_eq!(ev.macro_cache_misses - misses0, 1);
    assert_eq!(ev.macro_cache_hits - hits0, 1);
    assert_eq!(
        ev.obarray()
            .symbol_value("runtime-cache-bytes-count")
            .copied()
            .unwrap_or(Value::NIL),
        Value::fixnum(1)
    );
}

#[test]
fn catch_leaves_shared_condition_stack_balanced() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let result = ev.eval_str("(catch 'tag (throw 'tag 42))");
    assert_eq!(format_eval_result(&result), "OK 42");
    assert_eq!(ev.condition_stack_depth_for_test(), 0);
    assert!(ev.top_level_eval_state_is_clean());
}

#[test]
fn condition_case_leaves_shared_condition_stack_balanced() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let result = ev.eval_str("(condition-case err (signal 'error 1) (error err))");
    assert_eq!(format_eval_result(&result), "OK (error . 1)");
    assert_eq!(ev.condition_stack_depth_for_test(), 0);
    assert!(ev.top_level_eval_state_is_clean());
}

#[test]
fn condition_case_value_path_catches_default_toplevel_value_signal() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let result = ev.eval_str(
        "(condition-case nil
            (default-toplevel-value 'vm-unbound-value-path)
          (error 'caught))",
    );
    assert_eq!(format_eval_result(&result), "OK caught");
    assert_eq!(ev.condition_stack_depth_for_test(), 0);
    assert!(ev.top_level_eval_state_is_clean());
}

#[test]
fn handler_bind_1_leaves_shared_condition_stack_balanced() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let result = ev.eval_str(
        r#"(condition-case err
           (handler-bind-1 (lambda () (signal 'error 1))
                           '(error)
                           (lambda (_data) 'handled))
         (error err))"#,
    );
    assert_eq!(format_eval_result(&result), "OK (error . 1)");
    assert_eq!(ev.condition_stack_depth_for_test(), 0);
    assert!(ev.top_level_eval_state_is_clean());
}

#[test]
fn handler_bind_1_runs_inside_signal_dynamic_extent() {
    crate::test_utils::init_test_tracing();
    // user-error is defined in subr.el, so this needs the bootstrap
    // runtime context.
    assert_eq!(
        bootstrap_eval_one(
            "(catch 'tag
               (handler-bind-1
                 (lambda ()
                   (list 'inner-catch
                         (catch 'tag
                           (user-error \"hello\"))))
                 '(error)
                 (lambda (_err) (throw 'tag 'err))))"
        ),
        "OK (inner-catch err)"
    );
}

#[test]
fn set_lexical_binding_syncs_top_level_lexenv_sentinel() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();

    assert!(ev.lexenv.is_nil());
    assert!(!ev.lexical_binding());

    ev.set_lexical_binding(true);
    assert!(ev.lexical_binding());
    assert!(ev.lexenv.is_cons());
    assert!(ev.lexenv.cons_car().is_t());
    assert!(ev.lexenv.cons_cdr().is_nil());

    ev.set_lexical_binding(false);
    assert!(!ev.lexical_binding());
    assert!(ev.lexenv.is_nil());
}

#[test]
fn set_lexical_binding_updates_visible_dynamic_binding() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let sym = intern("lexical-binding");

    let specpdl_count = ev.specpdl.len();
    ev.specbind(sym, Value::NIL);
    assert!(ev.visible_variable_value_or_nil("lexical-binding").is_nil());

    ev.set_lexical_binding(true);
    assert!(ev.visible_variable_value_or_nil("lexical-binding").is_t());
    assert!(ev.lexical_binding());

    ev.unbind_to(specpdl_count);
    assert!(ev.visible_variable_value_or_nil("lexical-binding").is_nil());
}

#[test]
fn clear_top_level_eval_state_restores_top_level_lexenv_mode() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    ev.lexenv = Value::list(vec![Value::symbol("vm-temp"), Value::T]);

    ev.clear_top_level_eval_state();

    assert!(ev.lexical_binding());
    assert!(ev.lexenv.is_cons());
    assert!(ev.lexenv.cons_car().is_t());
    assert!(ev.lexenv.cons_cdr().is_nil());
    assert!(ev.top_level_eval_state_is_clean());
}

#[test]
fn handler_bind_1_mutes_lower_condition_handlers() {
    crate::test_utils::init_test_tracing();
    // user-error is defined in subr.el → bootstrap context required.
    assert_eq!(
        bootstrap_eval_one(
            "(condition-case nil
               (handler-bind-1
                 (lambda ()
                   (list 'result
                         (condition-case nil
                             (user-error \"hello\")
                           (wrong-type-argument 'inner-handler))))
                 '(error)
                 (lambda (_err) (signal 'wrong-type-argument nil)))
             (wrong-type-argument 'wrong-type-argument))"
        ),
        "OK wrong-type-argument"
    );
}

#[test]
fn handler_bind_1_handlers_do_not_apply_within_handlers() {
    crate::test_utils::init_test_tracing();
    // user-error is defined in subr.el → bootstrap context required.
    assert_eq!(
        bootstrap_eval_one(
            "(condition-case nil
               (handler-bind-1
                 (lambda () (user-error \"hello\"))
                 '(error)
                 (lambda (_err) (signal 'wrong-type-argument nil))
                 '(wrong-type-argument)
                 (lambda (_err) (user-error \"wrong-type-argument\")))
             (wrong-type-argument 'wrong-type-argument)
             (error 'plain-error))"
        ),
        "OK wrong-type-argument"
    );
}

#[test]
fn signal_hook_function_sees_raw_signal_payload_before_condition_case() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();

    assert_eq!(
        format_eval_result(&eval.eval_str(
            r#"(let (seen)
           (let ((signal-hook-function
                  (lambda (sym data)
                    (setq seen (cons sym data)))))
             (condition-case nil
                 (signal 'error 1)
               (error seen))))"#
        )),
        "OK (error . 1)"
    );
}

#[test]
fn signal_hook_function_runs_before_invalid_error_symbol_canonicalization() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();

    assert_eq!(
        format_eval_result(&eval.eval_str(
            r#"(catch 'tag
           (let ((signal-hook-function
                  (lambda (sym data)
                    (throw 'tag (list sym data)))))
             (signal 'neomacs-invalid-signal 1)))"#
        )),
        "OK (neomacs-invalid-signal 1)"
    );
}

#[test]
fn signal_nil_symbol_with_non_list_payload_becomes_plain_error() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one("(condition-case err (signal nil 1) (error err))"),
        "OK (error . 1)"
    );
}

#[test]
fn signal_nil_symbol_with_nil_payload_becomes_plain_error() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one("(condition-case err (signal nil nil) (error err))"),
        "OK (error)"
    );
}

#[test]
fn signal_nil_error_object_uses_embedded_symbol_and_skips_signal_hook() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();

    assert_eq!(
        format_eval_result(&eval.eval_str(
            r#"(let (seen)
           (let ((signal-hook-function
                  (lambda (&rest xs)
                    (setq seen xs))))
             (condition-case err
                 (signal nil '(error 1))
               (error (list err seen)))))"#
        )),
        "OK ((error 1) nil)"
    );
}

#[test]
fn signal_nil_error_object_with_invalid_symbol_reports_generic_invalid_error() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one("(condition-case err (signal nil '(bogus 1)) (error err))"),
        "OK (error \"Invalid error symbol\")"
    );
}

#[test]
fn evaluator_drop_leaves_symids_resolvable() {
    crate::test_utils::init_test_tracing();
    let sym = {
        let _ev = Context::new_minimal_vm_harness();
        crate::emacs_core::intern::intern("drop-stable-symbol")
    };
    assert_eq!(
        crate::emacs_core::intern::resolve_sym(sym),
        "drop-stable-symbol"
    );
}

#[test]
fn evaluator_reuses_hidden_internal_interpreter_environment_symbol() {
    crate::test_utils::init_test_tracing();
    let first = Context::new_minimal_vm_harness().internal_interpreter_environment_symbol;
    let second = Context::new_minimal_vm_harness().internal_interpreter_environment_symbol;
    assert_eq!(first, second);
}

#[test]
fn read_char_applies_resize_event_before_returning_next_keypress() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let fid = ev
        .frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    assert_eq!(ev.frames.selected_frame().map(|frame| frame.id), Some(fid));

    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);

    tx.send(crate::keyboard::InputEvent::Resize {
        width: 700,
        height: 800,
        emacs_frame_id: 0,
    })
    .unwrap();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('a'),
    ))
    .unwrap();

    let event = ev.read_char().expect("read_char should return a keypress");
    assert_eq!(event, Value::fixnum('a' as i64));

    let frame = ev.frames.get(fid).expect("frame should still be live");
    assert_eq!(frame.width, 700);
    assert_eq!(frame.height, 800);
}

#[test]
fn read_char_switches_active_kboard_to_keypress_source_frame_terminal() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let buf = ev.buffers.create_buffer("*scratch*");
    ev.buffers.set_current(buf);
    let primary = ev.frames.create_frame("F1", 960, 640, buf);
    ev.command_loop
        .keyboard
        .set_input_decode_map(Value::symbol("primary-map"));

    crate::emacs_core::terminal::pure::ensure_terminal_runtime_owner(
        7,
        "tty-7",
        crate::emacs_core::terminal::pure::TerminalRuntimeConfig::interactive(
            Some("xterm-256color".to_string()),
            256,
        ),
    );
    let secondary = ev.frames.create_frame_on_terminal("F2", 7, 960, 640, buf);
    assert!(ev.frames.select_frame(primary));
    ev.sync_keyboard_terminal_owner();
    assert_eq!(ev.command_loop.keyboard.active_terminal_id(), 0);

    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);

    tx.send(crate::keyboard::InputEvent::key_press_in_frame(
        crate::keyboard::KeyEvent::char('z'),
        secondary.0,
    ))
    .unwrap();

    let event = ev.read_char().expect("read_char should return a keypress");
    assert_eq!(event, Value::fixnum('z' as i64));
    assert_eq!(ev.command_loop.keyboard.active_terminal_id(), 7);
    assert_eq!(
        ev.command_loop.keyboard.input_decode_map(),
        Value::NIL,
        "raw key ingress should switch to the source frame terminal before key decoding state is used"
    );
}

#[test]
fn read_char_returns_unread_emacs_event_value_without_reencoding() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let meta_x = crate::keyboard::KeyEvent::char_with_mods('x', crate::keyboard::Modifiers::meta())
        .to_emacs_event_value();

    ev.command_loop
        .keyboard
        .kboard
        .unread_events
        .push_back(meta_x);

    let event = ev
        .read_char()
        .expect("read_char should return unread event");
    assert_eq!(event, meta_x);
}

#[test]
fn read_char_returns_macro_playback_event_value_without_reencoding() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let return_event =
        crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Return).to_emacs_event_value();

    ev.command_loop.keyboard.kboard.executing_kbd_macro = Some(vec![return_event]);
    ev.command_loop.keyboard.kboard.kbd_macro_index = 0;

    let event = ev
        .read_char()
        .expect("read_char should return executing macro event");
    assert_eq!(event, return_event);
    assert_eq!(ev.command_loop.keyboard.kboard.kbd_macro_index, 1);
}

#[test]
fn read_char_prefers_ready_keypress_over_due_timer_callback() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.eval_str(
        r#"(progn
             (fset 'read-char-priority-timer
                   (lambda () (setq read-char-priority-timer-fired t)))
             (setq read-char-priority-timer-fired nil))"#,
    )
    .expect("parse timer priority setup");
    ev.eval_str(
        r#"(progn
             (fset 'read-char-priority-timer
                   (lambda () (setq read-char-priority-timer-fired t)))
             (setq read-char-priority-timer-fired nil))"#,
    )
    .expect("install timer priority setup");
    ev.timers.add_timer(
        0.0,
        0.0,
        Value::symbol("read-char-priority-timer"),
        vec![],
        false,
    );

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('a'),
    ))
    .expect("queue ready keypress");
    ev.input_rx = Some(rx);

    let event = ev.read_char().expect("read_char should return keypress");
    assert_eq!(event, Value::fixnum('a' as i64));
    assert_eq!(
        ev.eval_symbol("read-char-priority-timer-fired")
            .expect("timer callback flag"),
        Value::NIL
    );

    ev.fire_pending_timers();
    assert_eq!(
        ev.eval_symbol("read-char-priority-timer-fired")
            .expect("timer callback flag after explicit service"),
        Value::T
    );
}

#[test]
fn read_char_prefers_ready_keypress_over_process_filter_callback() {
    crate::test_utils::init_test_tracing();
    let echo = find_bin("echo");
    let mut ev = Context::new();
    ev.eval_str(
        r#"(progn
             (fset 'read-char-priority-filter
                   (lambda (_proc string)
                     (setq read-char-priority-filter-data string)))
             (setq read-char-priority-filter-data nil))"#,
    )
    .expect("install process priority setup");

    let pid = ev.processes.create_process(
        "read-char-priority".into(),
        Value::NIL,
        echo,
        vec!["out".into()],
    );
    ev.processes
        .spawn_child(pid, false)
        .expect("spawn process priority child");
    crate::emacs_core::process::builtin_set_process_filter(
        &mut ev,
        vec![
            Value::fixnum(pid as i64),
            Value::symbol("read-char-priority-filter"),
        ],
    )
    .expect("install process priority filter");

    std::thread::sleep(Duration::from_millis(20));

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('a'),
    ))
    .expect("queue ready keypress");
    ev.input_rx = Some(rx);

    let event = ev.read_char().expect("read_char should return keypress");
    assert_eq!(event, Value::fixnum('a' as i64));
    assert_eq!(
        ev.eval_symbol("read-char-priority-filter-data")
            .expect("process filter flag"),
        Value::NIL
    );

    crate::emacs_core::process::builtin_accept_process_output(
        &mut ev,
        vec![Value::fixnum(pid as i64), Value::make_float(0.1)],
    )
    .expect("accept-process-output should service process callback afterwards");
    assert_eq!(
        ev.eval_symbol("read-char-priority-filter-data")
            .expect("process filter flag after explicit service"),
        Value::string("out\n")
    );
}

#[test]
fn read_char_triggers_redisplay_after_resize_event() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let fid = ev
        .frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    assert_eq!(ev.frames.selected_frame().map(|frame| frame.id), Some(fid));

    let redisplay_calls = Rc::new(RefCell::new(Vec::new()));
    let redisplay_calls_in_cb = redisplay_calls.clone();
    ev.redisplay_fn = Some(Box::new(move |ev: &mut Context| {
        let frame = ev
            .frames
            .selected_frame()
            .expect("selected frame during redisplay");
        redisplay_calls_in_cb
            .borrow_mut()
            .push((frame.width, frame.height));
    }));

    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);

    tx.send(crate::keyboard::InputEvent::Resize {
        width: 700,
        height: 800,
        emacs_frame_id: 0,
    })
    .unwrap();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('a'),
    ))
    .unwrap();

    let event = ev.read_char().expect("read_char should return a keypress");
    assert_eq!(event, Value::fixnum('a' as i64));
    assert_eq!(*redisplay_calls.borrow(), vec![(700, 800)]);
}

#[test]
fn read_char_redisplays_when_resize_arrives_after_pre_block_redisplay() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let fid = ev
        .frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    assert_eq!(ev.frames.selected_frame().map(|frame| frame.id), Some(fid));

    let redisplay_calls = Rc::new(RefCell::new(Vec::new()));
    let redisplay_calls_in_cb = redisplay_calls.clone();

    let (tx, rx) = crossbeam_channel::unbounded();
    let tx_in_cb = tx.clone();
    let injected = Rc::new(RefCell::new(false));
    let injected_in_cb = injected.clone();

    ev.redisplay_fn = Some(Box::new(move |ev: &mut Context| {
        let frame = ev
            .frames
            .selected_frame()
            .expect("selected frame during redisplay");
        redisplay_calls_in_cb
            .borrow_mut()
            .push((frame.width, frame.height));

        if !*injected_in_cb.borrow() {
            *injected_in_cb.borrow_mut() = true;
            tx_in_cb
                .send(crate::keyboard::InputEvent::Resize {
                    width: 700,
                    height: 800,
                    emacs_frame_id: 0,
                })
                .expect("enqueue resize after first redisplay");
            tx_in_cb
                .send(crate::keyboard::InputEvent::key_press(
                    crate::keyboard::KeyEvent::char('a'),
                ))
                .expect("enqueue keypress after resize");
        }
    }));

    ev.input_rx = Some(rx);

    let event = ev.read_char().expect("read_char should return a keypress");
    assert_eq!(event, Value::fixnum('a' as i64));
    assert_eq!(*redisplay_calls.borrow(), vec![(960, 640), (700, 800)]);
}

#[test]
fn read_char_respects_inhibit_redisplay_during_input_wait() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.obarray.set_symbol_value("inhibit-redisplay", Value::T);

    let redisplay_count = Rc::new(RefCell::new(0usize));
    let redisplay_count_in_cb = Rc::clone(&redisplay_count);
    ev.redisplay_fn = Some(Box::new(move |_ev: &mut Context| {
        *redisplay_count_in_cb.borrow_mut() += 1;
    }));

    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);
    thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        tx.send(crate::keyboard::InputEvent::key_press(
            crate::keyboard::KeyEvent::char('a'),
        ))
        .expect("send keypress");
    });

    let event = ev.read_char().expect("read_char should return keypress");
    assert_eq!(event, Value::fixnum('a' as i64));
    assert_eq!(*redisplay_count.borrow(), 0);
}

#[test]
fn redisplay_skips_callback_when_visible_state_is_unchanged() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();

    let redisplay_count = Rc::new(RefCell::new(0usize));
    let redisplay_count_in_cb = Rc::clone(&redisplay_count);
    ev.redisplay_fn = Some(Box::new(move |_ev: &mut Context| {
        *redisplay_count_in_cb.borrow_mut() += 1;
    }));

    ev.redisplay();
    ev.redisplay();
    assert_eq!(*redisplay_count.borrow(), 1);

    ev.set_current_message(Some(LispString::from_utf8("hello")));
    ev.redisplay();
    assert_eq!(*redisplay_count.borrow(), 2);

    ev.apply(Value::symbol("force-mode-line-update"), vec![])
        .expect("force-mode-line-update should be callable");
    ev.redisplay();
    assert_eq!(*redisplay_count.borrow(), 3);

    ev.redisplay_with_force(true);
    assert_eq!(*redisplay_count.borrow(), 4);
}

#[test]
fn redisplay_skips_callback_after_unwatched_symbol_value_change() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.obarray
        .set_symbol_value("blink-cursor-blinks-done", Value::fixnum(1));

    let redisplay_count = Rc::new(RefCell::new(0usize));
    let redisplay_count_in_cb = Rc::clone(&redisplay_count);
    ev.redisplay_fn = Some(Box::new(move |_ev: &mut Context| {
        *redisplay_count_in_cb.borrow_mut() += 1;
    }));

    ev.redisplay();
    assert_eq!(*redisplay_count.borrow(), 1);

    ev.eval_str("(setq blink-cursor-blinks-done (1+ blink-cursor-blinks-done))")
        .expect("blink counter setq should evaluate");
    ev.redisplay();
    assert_eq!(*redisplay_count.borrow(), 1);
}

#[test]
fn set_buffer_redisplay_watcher_invalidates_redisplay() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();

    let redisplay_count = Rc::new(RefCell::new(0usize));
    let redisplay_count_in_cb = Rc::clone(&redisplay_count);
    ev.redisplay_fn = Some(Box::new(move |_ev: &mut Context| {
        *redisplay_count_in_cb.borrow_mut() += 1;
    }));

    ev.redisplay();
    assert_eq!(*redisplay_count.borrow(), 1);

    ev.eval_str(
        r#"(progn
             (add-variable-watcher 'line-spacing
                                   (symbol-function 'set-buffer-redisplay))
             (setq line-spacing 2))"#,
    )
    .expect("line-spacing watcher should evaluate");
    ev.redisplay();
    assert_eq!(*redisplay_count.borrow(), 2);

    ev.redisplay();
    assert_eq!(*redisplay_count.borrow(), 2);
}

#[test]
fn read_char_does_not_redisplay_again_when_monitor_change_arrives_after_pre_block_redisplay() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();

    let redisplay_count = Rc::new(RefCell::new(0usize));
    let redisplay_count_in_cb = Rc::clone(&redisplay_count);

    let (tx, rx) = crossbeam_channel::unbounded();
    let tx_in_cb = tx.clone();
    let injected = Rc::new(RefCell::new(false));
    let injected_in_cb = Rc::clone(&injected);

    ev.redisplay_fn = Some(Box::new(move |_ev: &mut Context| {
        *redisplay_count_in_cb.borrow_mut() += 1;

        if !*injected_in_cb.borrow() {
            *injected_in_cb.borrow_mut() = true;
            tx_in_cb
                .send(crate::keyboard::InputEvent::MonitorsChanged {
                    monitors: vec![crate::emacs_core::builtins::NeomacsMonitorInfo {
                        x: 0,
                        y: 0,
                        width: 2560,
                        height: 1440,
                        scale: 1.25,
                        width_mm: 600,
                        height_mm: 340,
                        name: Some("DP-1".to_string()),
                    }],
                })
                .expect("enqueue monitor change after first redisplay");
            tx_in_cb
                .send(crate::keyboard::InputEvent::key_press(
                    crate::keyboard::KeyEvent::char('a'),
                ))
                .expect("enqueue keypress after monitor change");
        }
    }));

    ev.input_rx = Some(rx);

    let event = ev.read_char().expect("read_char should return a keypress");
    assert_eq!(event, Value::fixnum('a' as i64));
    assert_eq!(*redisplay_count.borrow(), 1);
}

#[test]
fn redisplay_applies_pending_resize_before_callback() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let fid = ev
        .frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    assert_eq!(ev.frames.selected_frame().map(|frame| frame.id), Some(fid));

    let redisplay_calls = Rc::new(RefCell::new(Vec::new()));
    let redisplay_calls_in_cb = redisplay_calls.clone();
    ev.redisplay_fn = Some(Box::new(move |ev: &mut Context| {
        let frame = ev
            .frames
            .selected_frame()
            .expect("selected frame during redisplay");
        redisplay_calls_in_cb
            .borrow_mut()
            .push((frame.width, frame.height));
    }));

    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);
    tx.send(crate::keyboard::InputEvent::Resize {
        width: 700,
        height: 800,
        emacs_frame_id: 0,
    })
    .unwrap();

    ev.redisplay();

    assert_eq!(*redisplay_calls.borrow(), vec![(700, 800)]);
}

#[test]
fn redisplay_syncs_opening_gui_frame_size_from_display_host() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let fid = ev
        .frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    ev.frames
        .get_mut(fid)
        .expect("frame should exist")
        .set_window_system(Some(Value::symbol("x")));

    let redisplay_calls = Rc::new(RefCell::new(Vec::new()));
    let redisplay_calls_in_cb = redisplay_calls.clone();
    ev.redisplay_fn = Some(Box::new(move |ev: &mut Context| {
        let frame = ev
            .frames
            .selected_frame()
            .expect("selected frame during redisplay");
        redisplay_calls_in_cb
            .borrow_mut()
            .push((frame.width, frame.height));
    }));

    ev.set_display_host(Box::new(RecordingDisplayHost::opening_with_primary_size(
        1500, 1900,
    )));

    ev.redisplay();

    assert_eq!(*redisplay_calls.borrow(), vec![(1500, 1900)]);
}

#[test]
fn recursive_edit_runs_top_level_before_outer_command_loop_reads_input() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let _ = ev.eval_str_each("(setq top-level '(setq neo-top-level-hit t))");

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::WindowClose { emacs_frame_id: 0 })
        .expect("queue close request");
    drop(tx);

    ev.input_rx = Some(rx);
    ev.command_loop.running = true;

    let result = ev
        .recursive_edit_inner()
        .expect("outer command loop should exit cleanly");
    assert_eq!(result, Value::NIL);
    assert!(
        ev.eval_symbol("neo-top-level-hit")
            .expect("top-level probe should be bound")
            .is_truthy(),
        "expected recursive_edit to evaluate `top-level' before waiting for input"
    );
}

#[test]
fn command_loop_runs_initial_post_command_hook_before_first_command() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    fn stop_command_loop_for_test(ctx: &mut Context, args: Vec<Value>) -> EvalResult {
        assert!(args.is_empty(), "stop helper should not receive arguments");
        ctx.command_loop.running = false;
        Ok(Value::NIL)
    }
    ev.defsubr(
        "neo-stop-command-loop-for-test",
        stop_command_loop_for_test,
        0,
        Some(0),
    );

    let scratch = ev.buffers.create_buffer("*command-loop-prologue*");
    ev.buffers.set_current(scratch);
    let frame = ev.frames.create_frame("F1", 80, 24, scratch);
    assert!(
        ev.frames.select_frame(frame),
        "command loop test should have a selected frame"
    );

    let global_map = crate::emacs_core::keymap::make_sparse_list_keymap();
    ev.assign("global-map", global_map);
    ev.eval_str(
        r#"(progn
             (setq neo-initial-post-command-count 0)
             (setq inhibit-redisplay t)
             (fset 'neo-initial-post-command-hook
                   (lambda ()
                     (setq neo-initial-post-command-count
                           (1+ neo-initial-post-command-count))
                     (setq inhibit-redisplay nil)
                     (setq post-command-hook nil)))
             (setq post-command-hook '(neo-initial-post-command-hook))
             (fset 'neo-exit-command
                   (lambda ()
                     (interactive)
                     (neo-stop-command-loop-for-test)))
             (fset 'command-execute
                   (lambda (cmd &optional _record _keys _special)
                     (funcall cmd))))"#,
    )
    .expect("setup command-loop prologue test");

    crate::emacs_core::keymap::list_keymap_define_seq(
        global_map,
        &[Value::fixnum('q' as i64)],
        Value::symbol("neo-exit-command"),
    )
    .expect("define exit command");
    ev.command_loop
        .keyboard
        .kboard
        .unread_events
        .push_back(Value::fixnum('q' as i64));
    ev.command_loop.running = true;

    let result = ev
        .recursive_edit_inner()
        .expect("recursive edit should exit through command");
    assert_eq!(result, Value::NIL);
    assert_eq!(
        ev.eval_symbol("neo-initial-post-command-count")
            .expect("post-command count"),
        Value::fixnum(1)
    );
    assert_eq!(
        ev.eval_symbol("inhibit-redisplay")
            .expect("inhibit-redisplay should be bound"),
        Value::NIL
    );
}

#[test]
fn read_char_requeues_keypress_and_throws_on_input() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('a'),
    ))
    .expect("queue keypress");
    ev.input_rx = Some(rx);
    ev.obarray
        .set_symbol_value("throw-on-input", Value::symbol("tag"));

    let flow = ev
        .read_char()
        .expect_err("throw-on-input should interrupt read_char");
    assert!(matches!(
        flow,
        Flow::Throw { tag, value } if tag == Value::symbol("tag") && value == Value::T
    ));

    ev.obarray.set_symbol_value("throw-on-input", Value::NIL);
    let event = ev.read_char().expect("keypress should remain queued");
    assert_eq!(event, Value::fixnum('a' as i64));
}

#[test]
fn read_char_window_close_honors_throw_on_input_before_quit() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::WindowClose { emacs_frame_id: 0 })
        .expect("queue close request");
    ev.input_rx = Some(rx);
    ev.obarray
        .set_symbol_value("throw-on-input", Value::symbol("tag"));

    let flow = ev
        .read_char()
        .expect_err("throw-on-input should interrupt read_char");
    assert!(matches!(
        flow,
        Flow::Throw { tag, value } if tag == Value::symbol("tag") && value == Value::T
    ));

    ev.obarray.set_symbol_value("throw-on-input", Value::NIL);
    let flow = ev
        .read_char()
        .expect_err("window close should still quit afterwards");
    assert!(matches!(flow, Flow::Signal(ref sig) if sig.symbol_name() == "quit"));
}

#[test]
fn read_char_window_close_uses_special_event_map_handler_when_loaded() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let scratch = ev.buffer_manager_mut().create_buffer("*scratch*");
    ev.buffer_manager_mut().set_current(scratch);
    let frame = ev.frames.create_frame("F1", 80, 24, scratch);
    install_minimal_special_event_command_runtime(&mut ev);

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::WindowClose {
        emacs_frame_id: frame.0,
    })
    .expect("queue window close");
    ev.input_rx = Some(rx);
    ev.command_loop.running = true;

    let event = match ev.read_char_with_timeout(Some(Duration::from_millis(0))) {
        Ok(event) => event,
        Err(flow) => panic!(
            "window close should be consumed without error, got flow={flow:?} logged={:?}",
            ev.eval_symbol("neo-last-delete-frame-event")
        ),
    };
    assert_eq!(event, None);
    drop(tx);
    let logged = ev
        .eval_symbol("neo-last-delete-frame-event")
        .expect("delete-frame event should be logged");
    assert_eq!(
        logged,
        Value::list(vec![
            Value::symbol("delete-frame"),
            Value::list(vec![Value::make_frame(frame.0)]),
        ]),
    );
}

#[test]
fn read_char_disconnected_input_uses_noelisp_terminal_teardown() {
    crate::test_utils::init_test_tracing();
    crate::emacs_core::terminal::pure::reset_terminal_thread_locals();
    let mut ev = Context::new();
    let scratch = ev.buffer_manager_mut().create_buffer("*scratch*");
    ev.buffer_manager_mut().set_current(scratch);
    let _frame = ev.frame_manager_mut().create_frame_on_terminal(
        "F1",
        crate::emacs_core::terminal::pure::TERMINAL_ID,
        80,
        25,
        scratch,
    );
    let (tx, rx) = crossbeam_channel::unbounded::<crate::keyboard::InputEvent>();
    ev.input_rx = Some(rx);
    drop(tx);

    ev.eval_str(
        r#"
(setq hook-log nil)
(setq delete-terminal-functions
      (list (lambda (term)
              (setq hook-log
                    (cons (list 'terminal (terminal-live-p term)) hook-log)))))
(setq delete-frame-functions
      (list (lambda (frame)
              (setq hook-log
                    (cons (list 'before (frame-live-p frame)) hook-log)))))
(setq after-delete-frame-functions
      (list (lambda (frame)
              (setq hook-log
                    (cons (list 'after (frame-live-p frame)) hook-log)))))
"#,
    )
    .expect("install disconnected input hook setup");

    let flow = ev
        .read_char()
        .expect_err("disconnected input should unwind read_char");
    assert!(matches!(flow, Flow::Signal(ref sig) if sig.symbol_name() == "quit"));
    assert_eq!(
        ev.shutdown_request().map(|request| request.exit_code),
        Some(0)
    );
    assert!(ev.frame_manager().frame_list().is_empty());
    assert!(
        crate::emacs_core::terminal::pure::builtin_terminal_live_p(
            &mut ev,
            vec![crate::emacs_core::terminal::pure::terminal_handle_value()]
        )
        .unwrap()
        .is_nil(),
        "disconnected input should tear down the display terminal via noelisp delete"
    );
    assert_eq!(
        ev.eval_str("hook-log").expect("hook-log before flush"),
        Value::NIL
    );

    ev.flush_pending_safe_funcalls();

    let post_flush = ev
        .eval_str("(nreverse hook-log)")
        .expect("hook-log after flush");
    assert_eq!(
        format!("{}", post_flush),
        "((after nil) (before nil) (terminal nil))"
    );
}

#[test]
fn eval_list_form_throws_on_pending_host_input() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('a'),
    ))
    .expect("queue keypress");
    ev.input_rx = Some(rx);
    ev.obarray
        .set_symbol_value("throw-on-input", Value::symbol("tag"));

    let result = ev.eval_str("(list 1 2)");
    assert!(matches!(
        result,
        Err(EvalError::UncaughtThrow { tag, value })
            if tag == Value::symbol("tag") && value == Value::T
    ));

    ev.obarray.set_symbol_value("throw-on-input", Value::NIL);
    let event = ev.read_char().expect("keypress should remain queued");
    assert_eq!(event, Value::fixnum('a' as i64));
}

#[test]
fn frame_native_width_syncs_pending_resize_without_read_char() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let fid = ev
        .frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    ev.frames
        .get_mut(fid)
        .expect("frame should exist")
        .set_parameter(Value::symbol("window-system"), Value::symbol("x"));

    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);
    tx.send(crate::keyboard::InputEvent::Resize {
        width: 700,
        height: 800,
        emacs_frame_id: 0,
    })
    .unwrap();

    let width = crate::emacs_core::window_cmds::builtin_frame_native_width(&mut ev, vec![])
        .expect("frame-native-width should succeed");
    let height = crate::emacs_core::window_cmds::builtin_frame_native_height(&mut ev, vec![])
        .expect("frame-native-height should succeed");

    assert_eq!(width, Value::fixnum(700));
    assert_eq!(height, Value::fixnum(800));
}

#[test]
fn frame_native_width_syncs_pending_resize_behind_focus_event() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let fid = ev
        .frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    ev.frames
        .get_mut(fid)
        .expect("frame should exist")
        .set_parameter(Value::symbol("window-system"), Value::symbol("x"));

    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);
    tx.send(crate::keyboard::InputEvent::Focus {
        focused: true,
        emacs_frame_id: 0,
    })
    .unwrap();
    tx.send(crate::keyboard::InputEvent::Resize {
        width: 700,
        height: 800,
        emacs_frame_id: 0,
    })
    .unwrap();

    let width = crate::emacs_core::window_cmds::builtin_frame_native_width(&mut ev, vec![])
        .expect("frame-native-width should succeed");
    let height = crate::emacs_core::window_cmds::builtin_frame_native_height(&mut ev, vec![])
        .expect("frame-native-height should succeed");

    assert_eq!(width, Value::fixnum(700));
    assert_eq!(height, Value::fixnum(800));
}

#[test]
fn redisplay_applies_resize_already_queued_behind_focus_event() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let fid = ev
        .frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    assert_eq!(ev.frames.selected_frame().map(|frame| frame.id), Some(fid));

    let redisplay_calls = Rc::new(RefCell::new(Vec::new()));
    let redisplay_calls_in_cb = redisplay_calls.clone();
    ev.redisplay_fn = Some(Box::new(move |ev: &mut Context| {
        let frame = ev
            .frames
            .selected_frame()
            .expect("selected frame during redisplay");
        redisplay_calls_in_cb
            .borrow_mut()
            .push((frame.width, frame.height));
    }));

    ev.command_loop
        .keyboard
        .pending_input_events
        .push_back(crate::keyboard::InputEvent::Focus {
            focused: true,
            emacs_frame_id: 0,
        });
    ev.command_loop
        .keyboard
        .pending_input_events
        .push_back(crate::keyboard::InputEvent::Resize {
            width: 700,
            height: 800,
            emacs_frame_id: 0,
        });

    ev.redisplay();

    assert_eq!(*redisplay_calls.borrow(), vec![(700, 800)]);
    assert!(matches!(
        ev.command_loop.keyboard.pending_input_events.front(),
        Some(crate::keyboard::InputEvent::Focus {
            focused: true,
            emacs_frame_id: 0
        })
    ));
    assert_eq!(ev.command_loop.keyboard.pending_input_events.len(), 1);
}

#[test]
fn read_char_preserves_keypress_after_queued_focus_and_resize() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let fid = ev
        .frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    assert_eq!(ev.frames.selected_frame().map(|frame| frame.id), Some(fid));

    ev.command_loop
        .keyboard
        .pending_input_events
        .push_back(crate::keyboard::InputEvent::Focus {
            focused: true,
            emacs_frame_id: 0,
        });
    ev.command_loop
        .keyboard
        .pending_input_events
        .push_back(crate::keyboard::InputEvent::Resize {
            width: 700,
            height: 800,
            emacs_frame_id: 0,
        });
    ev.command_loop.keyboard.pending_input_events.push_back(
        crate::keyboard::InputEvent::key_press(crate::keyboard::KeyEvent::char('a')),
    );

    let event = ev.read_char().expect("read_char should return a keypress");
    assert_eq!(event, Value::fixnum('a' as i64));

    let frame = ev.frames.get(fid).expect("frame should still be live");
    assert_eq!(frame.width, 700);
    assert_eq!(frame.height, 800);
}

#[test]
fn keyboard_runtime_starts_with_terminal_translation_maps_from_context_bootstrap() {
    crate::test_utils::init_test_tracing();
    let ev = Context::new();

    assert_eq!(
        ev.command_loop.keyboard.input_decode_map(),
        ev.eval_symbol("input-decode-map")
            .expect("input-decode-map should be bound")
    );
    assert_eq!(
        ev.command_loop.keyboard.local_function_key_map(),
        ev.eval_symbol("local-function-key-map")
            .expect("local-function-key-map should be bound")
    );
}

#[test]
fn assigning_terminal_translation_maps_updates_keyboard_runtime_owner() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let input_decode_map = crate::emacs_core::keymap::make_sparse_list_keymap();
    let local_function_key_map = crate::emacs_core::keymap::make_sparse_list_keymap();

    ev.assign("input-decode-map", input_decode_map);
    ev.assign("local-function-key-map", local_function_key_map);

    assert_eq!(
        ev.command_loop.keyboard.input_decode_map(),
        input_decode_map
    );
    assert_eq!(
        ev.command_loop.keyboard.local_function_key_map(),
        local_function_key_map
    );
}

#[test]
fn read_key_sequence_function_translation_receives_prompt() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let global_map = crate::emacs_core::keymap::make_sparse_list_keymap();
    ev.assign("global-map", global_map);
    ev.eval_str(
        r#"(progn
             (setq neomacs-test-read-key-sequence-prompt nil)
             (fset 'neomacs-test-read-key-sequence-command
                   (lambda () (interactive) 'ok))
             (fset 'neomacs-test-key-translation
                   (lambda (prompt)
                     (setq neomacs-test-read-key-sequence-prompt prompt)
                     [f1])))"#,
    )
    .expect("setup");

    crate::emacs_core::keymap::list_keymap_define_seq(
        global_map,
        &[Value::symbol("f1")],
        Value::symbol("neomacs-test-read-key-sequence-command"),
    )
    .expect("define translated command");

    let key_translation_map = ev
        .eval_symbol("key-translation-map")
        .expect("key-translation-map");
    crate::emacs_core::keymap::list_keymap_define_seq(
        key_translation_map,
        &[Value::fixnum('a' as i64)],
        Value::symbol("neomacs-test-key-translation"),
    )
    .expect("define translation");

    ev.command_loop
        .keyboard
        .kboard
        .unread_events
        .push_back(Value::fixnum('a' as i64));

    let (keys, binding) = ev
        .read_key_sequence_with_options(crate::keyboard::ReadKeySequenceOptions::new(
            Value::string("Prompt> "),
            false,
            false,
        ))
        .expect("read translated key sequence");

    assert_eq!(keys, vec![Value::symbol("f1")]);
    assert_eq!(
        binding,
        Value::symbol("neomacs-test-read-key-sequence-command")
    );

    let prompt = ev
        .eval_str("neomacs-test-read-key-sequence-prompt")
        .expect("prompt should evaluate");
    assert_eq!(prompt, Value::string("Prompt> "));
}

#[test]
fn read_key_sequence_continues_through_pending_suffix_translation_prefix() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let global_map = crate::emacs_core::keymap::make_sparse_list_keymap();
    ev.assign("global-map", global_map);
    ev.eval_str(
        r#"(fset 'neomacs-test-suffix-translation-command
                  (lambda () (interactive) 'ok))"#,
    )
    .expect("setup");

    crate::emacs_core::keymap::list_keymap_define_seq(
        global_map,
        &[Value::fixnum('a' as i64), Value::symbol("f1")],
        Value::symbol("neomacs-test-suffix-translation-command"),
    )
    .expect("define suffix command");

    let input_decode_map = ev
        .eval_symbol("input-decode-map")
        .expect("input-decode-map");
    crate::emacs_core::keymap::list_keymap_define_seq(
        input_decode_map,
        &[Value::fixnum('b' as i64), Value::fixnum('c' as i64)],
        Value::vector(vec![Value::symbol("f1")]),
    )
    .expect("define input-decode suffix translation");

    for event in [
        Value::fixnum('a' as i64),
        Value::fixnum('b' as i64),
        Value::fixnum('c' as i64),
    ] {
        ev.command_loop
            .keyboard
            .kboard
            .unread_events
            .push_back(event);
    }

    let (keys, binding) = ev
        .read_key_sequence()
        .expect("read suffix-translated sequence");
    assert_eq!(keys, vec![Value::fixnum('a' as i64), Value::symbol("f1")]);
    assert_eq!(
        binding,
        Value::symbol("neomacs-test-suffix-translation-command")
    );
}

#[test]
fn read_key_sequence_prefix_echo_does_not_log_to_messages_buffer() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let global_map = crate::emacs_core::keymap::make_sparse_list_keymap();
    ev.assign("global-map", global_map);
    ev.eval_str(
        r#"(fset 'neomacs-test-prefix-target-command
                  (lambda () (interactive) 'ok))"#,
    )
    .expect("setup prefix target command");

    let sequence =
        crate::keyboard::KeySequence::from_description("C-x C-f").expect("C-x C-f key sequence");
    let events = sequence
        .events
        .iter()
        .map(crate::keyboard::KeyEvent::to_emacs_event_value)
        .collect::<Vec<_>>();
    crate::emacs_core::keymap::list_keymap_define_seq(
        global_map,
        &events,
        Value::symbol("neomacs-test-prefix-target-command"),
    )
    .expect("define prefix command");
    for event in events {
        ev.command_loop
            .keyboard
            .kboard
            .unread_events
            .push_back(event);
    }

    let (_keys, binding) = ev.read_key_sequence().expect("read prefixed key sequence");
    assert_eq!(binding, Value::symbol("neomacs-test-prefix-target-command"));
    assert!(
        ev.current_message_text()
            .is_some_and(|message| message.contains("C-x")),
        "prefix echo should still update the echo area"
    );
    if let Some(messages_id) = ev.buffers.find_buffer_by_name("*Messages*") {
        let messages = ev.buffers.get(messages_id).expect("*Messages* live");
        assert!(
            !messages.buffer_string().contains("C-x"),
            "GNU prefix-key echo uses message3_nolog and must not log to *Messages*"
        );
    }
}

#[test]
fn read_key_sequence_shift_translates_uppercase_binding() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let global_map = crate::emacs_core::keymap::make_sparse_list_keymap();
    ev.assign("global-map", global_map);
    ev.eval_str(
        r#"(fset 'neomacs-test-shift-translation-command
                  (lambda () (interactive) 'ok))"#,
    )
    .expect("setup");

    crate::emacs_core::keymap::list_keymap_define_seq(
        global_map,
        &[Value::fixnum('a' as i64)],
        Value::symbol("neomacs-test-shift-translation-command"),
    )
    .expect("define lowercase command");

    ev.command_loop
        .keyboard
        .kboard
        .unread_events
        .push_back(Value::fixnum('A' as i64));

    let (keys, binding) = ev.read_key_sequence().expect("read shifted key");

    assert_eq!(keys, vec![Value::fixnum('a' as i64)]);
    assert_eq!(
        binding,
        Value::symbol("neomacs-test-shift-translation-command")
    );
    assert_eq!(
        ev.eval_symbol("this-command-keys-shift-translated")
            .expect("shift translation flag"),
        Value::T
    );
}

#[test]
fn read_key_sequence_dont_downcase_last_restores_original_event() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let global_map = crate::emacs_core::keymap::make_sparse_list_keymap();
    ev.assign("global-map", global_map);
    ev.eval_str(
        r#"(fset 'neomacs-test-shift-translation-command
                  (lambda () (interactive) 'ok))"#,
    )
    .expect("setup");

    crate::emacs_core::keymap::list_keymap_define_seq(
        global_map,
        &[Value::fixnum('a' as i64)],
        Value::symbol("neomacs-test-shift-translation-command"),
    )
    .expect("define lowercase command");

    ev.command_loop
        .keyboard
        .kboard
        .unread_events
        .push_back(Value::fixnum('A' as i64));

    let (keys, binding) = ev
        .read_key_sequence_with_options(crate::keyboard::ReadKeySequenceOptions::new(
            Value::NIL,
            true,
            false,
        ))
        .expect("read shifted key without downcasing");

    assert_eq!(keys, vec![Value::fixnum('A' as i64)]);
    assert_eq!(
        binding,
        Value::symbol("neomacs-test-shift-translation-command")
    );
    assert_eq!(
        ev.eval_symbol("this-command-keys-shift-translated")
            .expect("shift translation flag"),
        Value::NIL
    );
}

#[test]
fn read_key_sequence_undefined_shift_translation_restores_original_event() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.assign(
        "global-map",
        crate::emacs_core::keymap::make_sparse_list_keymap(),
    );

    ev.command_loop
        .keyboard
        .kboard
        .unread_events
        .push_back(Value::fixnum('A' as i64));

    let (keys, binding) = ev.read_key_sequence().expect("read undefined shifted key");

    assert_eq!(keys, vec![Value::fixnum('A' as i64)]);
    assert_eq!(binding, Value::symbol("self-insert-command"));
    assert_eq!(
        ev.eval_symbol("this-command-keys-shift-translated")
            .expect("shift translation flag"),
        Value::NIL
    );
}

#[test]
fn read_key_sequence_shift_translates_shifted_function_key() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let global_map = crate::emacs_core::keymap::make_sparse_list_keymap();
    ev.assign("global-map", global_map);
    ev.eval_str(
        r#"(fset 'neomacs-test-shifted-function-command
                  (lambda () (interactive) 'ok))"#,
    )
    .expect("setup");

    crate::emacs_core::keymap::list_keymap_define_seq(
        global_map,
        &[Value::symbol("f1")],
        Value::symbol("neomacs-test-shifted-function-command"),
    )
    .expect("define function-key command");

    ev.command_loop
        .keyboard
        .kboard
        .unread_events
        .push_back(Value::symbol("S-f1"));

    let (keys, binding) = ev
        .read_key_sequence()
        .expect("read shifted function-key sequence");

    assert_eq!(keys, vec![Value::symbol("f1")]);
    assert_eq!(
        binding,
        Value::symbol("neomacs-test-shifted-function-command")
    );
    assert_eq!(
        ev.eval_symbol("this-command-keys-shift-translated")
            .expect("shift translation flag"),
        Value::T
    );
}

#[test]
fn read_char_returns_lispy_switch_frame_for_focus_event() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    install_minimal_special_event_command_runtime(&mut ev);
    ev.frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    let target_buffer = ev.buffers.create_buffer("focus-target");
    let target_frame = ev.frames.create_frame("F2", 960, 640, target_buffer).0;

    ev.command_loop
        .keyboard
        .pending_input_events
        .push_back(crate::keyboard::InputEvent::Focus {
            focused: true,
            emacs_frame_id: target_frame,
        });

    let event = ev
        .read_char()
        .expect("read_char should surface switch-frame");
    assert_eq!(
        event,
        Value::list(vec![
            Value::symbol("switch-frame"),
            Value::make_frame(target_frame),
        ])
    );
}

#[test]
fn read_key_sequence_defers_switch_frame_until_after_current_key_sequence() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    install_minimal_special_event_command_runtime(&mut ev);
    ev.frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    let target_buffer = ev.buffers.create_buffer("focus-target");
    let target_frame = ev.frames.create_frame("F2", 960, 640, target_buffer).0;
    let global_map = crate::emacs_core::keymap::make_sparse_list_keymap();
    ev.assign("global-map", global_map);
    ev.eval_str(
        r#"(fset 'neomacs-test-switch-frame-deferred-command
                  (lambda () (interactive) 'ok))"#,
    )
    .expect("setup");
    crate::emacs_core::keymap::list_keymap_define_seq(
        global_map,
        &[Value::fixnum('a' as i64), Value::fixnum('b' as i64)],
        Value::symbol("neomacs-test-switch-frame-deferred-command"),
    )
    .expect("define command");

    ev.command_loop.keyboard.pending_input_events.push_back(
        crate::keyboard::InputEvent::key_press(crate::keyboard::KeyEvent::char('a')),
    );
    ev.command_loop
        .keyboard
        .pending_input_events
        .push_back(crate::keyboard::InputEvent::Focus {
            focused: true,
            emacs_frame_id: target_frame,
        });
    ev.command_loop.keyboard.pending_input_events.push_back(
        crate::keyboard::InputEvent::key_press(crate::keyboard::KeyEvent::char('b')),
    );

    let (keys, binding) = ev.read_key_sequence().expect("read key sequence");
    assert_eq!(
        keys,
        vec![Value::fixnum('a' as i64), Value::fixnum('b' as i64)]
    );
    assert_eq!(
        binding,
        Value::symbol("neomacs-test-switch-frame-deferred-command")
    );

    let deferred = ev
        .read_char()
        .expect("deferred switch-frame should be unread first");
    assert_eq!(
        deferred,
        Value::list(vec![
            Value::symbol("switch-frame"),
            Value::make_frame(target_frame),
        ])
    );
}

#[test]
fn read_key_sequence_can_return_switch_frame_at_sequence_start() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    install_minimal_special_event_command_runtime(&mut ev);
    ev.frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    let target_buffer = ev.buffers.create_buffer("focus-target");
    let target_frame = ev.frames.create_frame("F2", 960, 640, target_buffer).0;
    let global_map = crate::emacs_core::keymap::make_sparse_list_keymap();
    ev.assign("global-map", global_map);
    crate::emacs_core::keymap::list_keymap_define_seq(
        global_map,
        &[Value::symbol("switch-frame")],
        Value::symbol("handle-switch-frame"),
    )
    .expect("define switch-frame binding");

    ev.command_loop
        .keyboard
        .pending_input_events
        .push_back(crate::keyboard::InputEvent::Focus {
            focused: true,
            emacs_frame_id: target_frame,
        });

    let (keys, binding) = ev
        .read_key_sequence_with_options(crate::keyboard::ReadKeySequenceOptions::new(
            Value::NIL,
            false,
            true,
        ))
        .expect("read switch-frame sequence");

    assert_eq!(
        keys,
        vec![Value::list(vec![
            Value::symbol("switch-frame"),
            Value::make_frame(target_frame),
        ])]
    );
    assert_eq!(binding, Value::symbol("handle-switch-frame"));
}

#[test]
fn special_event_map_bootstraps_delete_frame_and_focus_handlers() {
    crate::test_utils::init_test_tracing();
    let ev = Context::new();
    let special_event_map = ev
        .eval_symbol("special-event-map")
        .expect("special-event-map should be bound");

    let delete_frame = crate::emacs_core::keymap::lookup_key_in_keymaps_in_obarray(
        ev.obarray(),
        &[special_event_map],
        &[Value::symbol("delete-frame")],
        true,
    );
    let focus_in = crate::emacs_core::keymap::lookup_key_in_keymaps_in_obarray(
        ev.obarray(),
        &[special_event_map],
        &[Value::symbol("focus-in")],
        true,
    );
    let focus_out = crate::emacs_core::keymap::lookup_key_in_keymaps_in_obarray(
        ev.obarray(),
        &[special_event_map],
        &[Value::symbol("focus-out")],
        true,
    );

    assert_eq!(delete_frame, Value::symbol("handle-delete-frame"));
    assert_eq!(focus_in, Value::symbol("handle-focus-in"));
    assert_eq!(focus_out, Value::symbol("handle-focus-out"));
}

#[test]
fn read_char_updates_monitor_snapshot_and_runs_display_monitor_hooks() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.eval_str(
        r#"(progn
             (setq monitor-hook-terminal nil)
             (setq display-monitors-changed-functions
                   (list (lambda (terminal)
                           (setq monitor-hook-terminal terminal)))))"#,
    )
    .expect("install display monitor hook");
    ev.command_loop.keyboard.pending_input_events.push_back(
        crate::keyboard::InputEvent::MonitorsChanged {
            monitors: vec![crate::emacs_core::builtins::NeomacsMonitorInfo {
                x: 10,
                y: 20,
                width: 2560,
                height: 1440,
                scale: 1.25,
                width_mm: 600,
                height_mm: 340,
                name: Some("DP-1".to_string()),
            }],
        },
    );
    ev.command_loop.keyboard.pending_input_events.push_back(
        crate::keyboard::InputEvent::key_press(crate::keyboard::KeyEvent::char('x')),
    );

    let event = ev
        .read_char()
        .expect("read_char should continue past monitor change event");
    assert_eq!(event, Value::fixnum('x' as i64));

    let snapshot = crate::emacs_core::builtins::neomacs_monitor_info_snapshot();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].name.as_deref(), Some("DP-1"));
    assert_eq!(snapshot[0].width, 2560);
    assert_eq!(snapshot[0].height, 1440);

    assert_eq!(
        ev.eval_str("monitor-hook-terminal")
            .expect("display monitor hook terminal"),
        crate::emacs_core::terminal::pure::terminal_handle_value()
    );
}

#[test]
fn read_char_returns_lispy_select_window_for_transport_event() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    let fid = ev.frames.selected_frame().expect("selected frame").id;
    let w1 = ev.frames.get(fid).expect("frame").window_list()[0];
    let other_buffer = ev.buffers.create_buffer("select-window-target");
    let w2 = ev
        .frames
        .split_window(
            fid,
            w1,
            crate::window::SplitDirection::Horizontal,
            other_buffer,
            None,
        )
        .expect("split window");

    ev.command_loop
        .keyboard
        .pending_input_events
        .push_back(crate::keyboard::InputEvent::SelectWindow { window_id: w2 });

    let event = ev
        .read_char()
        .expect("read_char should surface select-window");
    assert_eq!(
        event,
        Value::list(vec![
            Value::symbol("select-window"),
            Value::list(vec![Value::make_window(w2.0)]),
        ])
    );
}

#[test]
fn read_key_sequence_defers_select_window_until_after_current_key_sequence() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    let fid = ev.frames.selected_frame().expect("selected frame").id;
    let w1 = ev.frames.get(fid).expect("frame").window_list()[0];
    let other_buffer = ev.buffers.create_buffer("select-window-target");
    let w2 = ev
        .frames
        .split_window(
            fid,
            w1,
            crate::window::SplitDirection::Horizontal,
            other_buffer,
            None,
        )
        .expect("split window");

    let global_map = crate::emacs_core::keymap::make_sparse_list_keymap();
    ev.assign("global-map", global_map);
    ev.eval_str(
        r#"(fset 'neomacs-test-select-window-deferred-command
                  (lambda () (interactive) 'ok))"#,
    )
    .expect("parse");
    ev.eval_str(
        r#"(fset 'neomacs-test-select-window-deferred-command
                  (lambda () (interactive) 'ok))"#,
    )
    .expect("setup");
    crate::emacs_core::keymap::list_keymap_define_seq(
        global_map,
        &[Value::fixnum('a' as i64), Value::fixnum('b' as i64)],
        Value::symbol("neomacs-test-select-window-deferred-command"),
    )
    .expect("define command");

    ev.command_loop.keyboard.pending_input_events.push_back(
        crate::keyboard::InputEvent::key_press(crate::keyboard::KeyEvent::char('a')),
    );
    ev.command_loop
        .keyboard
        .pending_input_events
        .push_back(crate::keyboard::InputEvent::SelectWindow { window_id: w2 });
    ev.command_loop.keyboard.pending_input_events.push_back(
        crate::keyboard::InputEvent::key_press(crate::keyboard::KeyEvent::char('b')),
    );

    let (keys, binding) = ev.read_key_sequence().expect("read key sequence");
    assert_eq!(
        keys,
        vec![Value::fixnum('a' as i64), Value::fixnum('b' as i64)]
    );
    assert_eq!(
        binding,
        Value::symbol("neomacs-test-select-window-deferred-command")
    );

    let deferred = ev
        .read_char()
        .expect("deferred select-window should be unread first");
    assert_eq!(
        deferred,
        Value::list(vec![
            Value::symbol("select-window"),
            Value::list(vec![Value::make_window(w2.0)]),
        ])
    );
}

#[test]
fn read_key_sequence_can_return_select_window_at_sequence_start() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    let fid = ev.frames.selected_frame().expect("selected frame").id;
    let w1 = ev.frames.get(fid).expect("frame").window_list()[0];
    let other_buffer = ev.buffers.create_buffer("select-window-target");
    let w2 = ev
        .frames
        .split_window(
            fid,
            w1,
            crate::window::SplitDirection::Horizontal,
            other_buffer,
            None,
        )
        .expect("split window");

    let global_map = crate::emacs_core::keymap::make_sparse_list_keymap();
    ev.assign("global-map", global_map);
    ev.eval_str(
        r#"(fset 'neomacs-test-handle-select-window
                  (lambda () (interactive) 'ok))"#,
    )
    .expect("setup");
    crate::emacs_core::keymap::list_keymap_define_seq(
        global_map,
        &[Value::symbol("select-window")],
        Value::symbol("neomacs-test-handle-select-window"),
    )
    .expect("define select-window binding");

    ev.command_loop
        .keyboard
        .pending_input_events
        .push_back(crate::keyboard::InputEvent::SelectWindow { window_id: w2 });

    let (keys, binding) = ev
        .read_key_sequence_with_options(crate::keyboard::ReadKeySequenceOptions::new(
            Value::NIL,
            false,
            true,
        ))
        .expect("read select-window sequence");

    assert_eq!(
        keys,
        vec![Value::list(vec![
            Value::symbol("select-window"),
            Value::list(vec![Value::make_window(w2.0)]),
        ])]
    );
    assert_eq!(binding, Value::symbol("neomacs-test-handle-select-window"));
}

#[test]
fn read_char_mouse_press_uses_clicked_window_geometry() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    let fid = ev.frames.selected_frame().expect("selected frame").id;
    let w1 = ev.frames.get(fid).expect("frame").window_list()[0];
    let other_buffer = ev.buffers.create_buffer("mouse-click-target");
    let w2 = ev
        .frames
        .split_window(
            fid,
            w1,
            crate::window::SplitDirection::Horizontal,
            other_buffer,
            None,
        )
        .expect("split window");
    let _ = ev
        .buffers
        .replace_buffer_contents(other_buffer, &"x".repeat(96));

    let (click_x, click_y) = {
        let frame = ev.frames.get(fid).expect("frame after split");
        let bounds = *frame.find_window(w2).expect("clicked window").bounds();
        (bounds.x + 25.0, bounds.y + 10.0)
    };

    ev.frames
        .get_mut(fid)
        .expect("mutable frame")
        .replace_display_snapshots(vec![crate::window::WindowDisplaySnapshot {
            window_id: w2,
            text_area_left_offset: 5,
            mode_line_height: 0,
            header_line_height: 0,
            tab_line_height: 0,
            logical_cursor: None,
            phys_cursor: None,
            points: vec![crate::window::DisplayPointSnapshot {
                buffer_pos: 77,
                x: 20,
                y: 0,
                width: 8,
                height: 16,
                row: 0,
                col: 2,
            }],
            rows: vec![crate::window::DisplayRowSnapshot {
                row: 0,
                y: 0,
                height: 16,
                start_x: 0,
                start_col: 0,
                end_x: 0,
                end_col: 0,
                start_buffer_pos: Some(77),
                end_buffer_pos: Some(77),
            }],
        }]);

    ev.command_loop.keyboard.pending_input_events.push_back(
        crate::keyboard::InputEvent::MousePress {
            button: crate::keyboard::MouseButton::Left,
            x: click_x,
            y: click_y,
            modifiers: crate::keyboard::Modifiers::none(),
            target_frame_id: fid.0,
        },
    );

    let event = ev.read_char().expect("read mouse press");
    let event_slots = crate::emacs_core::value::list_to_vec(&event).expect("event list");
    let position = event_slots[1];
    let position_slots = crate::emacs_core::value::list_to_vec(&position).expect("mouse posn list");

    assert_eq!(event_slots[0], Value::symbol("down-mouse-1"));
    assert_eq!(position_slots[0], Value::make_window(w2.0));
    assert_eq!(position_slots[1], Value::fixnum(77));
    assert_eq!(
        position_slots[2],
        Value::cons(Value::fixnum(20), Value::fixnum(10))
    );
    assert_eq!(position_slots[5], Value::fixnum(77));
    assert_eq!(
        position_slots[6],
        Value::cons(Value::fixnum(2), Value::fixnum(0))
    );
    assert_eq!(
        position_slots[9],
        Value::cons(Value::fixnum(8), Value::fixnum(16))
    );
}

#[test]
fn read_key_sequence_uses_clicked_window_local_map_for_mouse_event() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    let fid = ev.frames.selected_frame().expect("selected frame").id;
    let w1 = ev.frames.get(fid).expect("frame").window_list()[0];
    let other_buffer = ev.buffers.create_buffer("mouse-click-binding");
    let w2 = ev
        .frames
        .split_window(
            fid,
            w1,
            crate::window::SplitDirection::Horizontal,
            other_buffer,
            None,
        )
        .expect("split window");
    let _ = ev
        .buffers
        .replace_buffer_contents(other_buffer, &"x".repeat(96));

    ev.eval_str(
        r#"(fset 'neomacs-mouse-click-target-command
                  (lambda () (interactive) 'ok))"#,
    )
    .expect("setup");

    let local_map = crate::emacs_core::keymap::make_sparse_list_keymap();
    ev.buffers
        .set_buffer_local_map(other_buffer, local_map)
        .expect("buffer local map");
    crate::emacs_core::keymap::list_keymap_define_seq(
        local_map,
        &[Value::symbol("mouse-1")],
        Value::symbol("neomacs-mouse-click-target-command"),
    )
    .expect("define mouse binding");

    let (click_x, click_y) = {
        let frame = ev.frames.get(fid).expect("frame after split");
        let bounds = *frame.find_window(w2).expect("clicked window").bounds();
        (bounds.x + 25.0, bounds.y + 10.0)
    };

    ev.frames
        .get_mut(fid)
        .expect("mutable frame")
        .replace_display_snapshots(vec![crate::window::WindowDisplaySnapshot {
            window_id: w2,
            text_area_left_offset: 5,
            mode_line_height: 0,
            header_line_height: 0,
            tab_line_height: 0,
            logical_cursor: None,
            phys_cursor: None,
            points: vec![crate::window::DisplayPointSnapshot {
                buffer_pos: 77,
                x: 20,
                y: 0,
                width: 8,
                height: 16,
                row: 0,
                col: 2,
            }],
            rows: vec![crate::window::DisplayRowSnapshot {
                row: 0,
                y: 0,
                height: 16,
                start_x: 0,
                start_col: 0,
                end_x: 0,
                end_col: 0,
                start_buffer_pos: Some(77),
                end_buffer_pos: Some(77),
            }],
        }]);

    ev.command_loop.keyboard.pending_input_events.push_back(
        crate::keyboard::InputEvent::MouseRelease {
            button: crate::keyboard::MouseButton::Left,
            x: click_x,
            y: click_y,
            target_frame_id: fid.0,
        },
    );

    let (keys, binding) = ev.read_key_sequence().expect("read mouse sequence");
    let position = crate::emacs_core::value::list_to_vec(&keys[0]).expect("event list")[1];
    let position_slots = crate::emacs_core::value::list_to_vec(&position).expect("mouse posn list");

    assert_eq!(binding, Value::symbol("neomacs-mouse-click-target-command"));
    assert_eq!(position_slots[0], Value::make_window(w2.0));
    assert_eq!(position_slots[5], Value::fixnum(77));
}

#[test]
fn read_key_sequence_drops_unbound_down_mouse_before_bound_click() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    let fid = ev.frames.selected_frame().expect("selected frame").id;
    let w1 = ev.frames.get(fid).expect("frame").window_list()[0];
    let other_buffer = ev.buffers.create_buffer("mouse-click-binding");
    let w2 = ev
        .frames
        .split_window(
            fid,
            w1,
            crate::window::SplitDirection::Horizontal,
            other_buffer,
            None,
        )
        .expect("split window");
    let _ = ev
        .buffers
        .replace_buffer_contents(other_buffer, &"x".repeat(96));

    ev.eval_str(
        r#"(fset 'neomacs-mouse-click-target-command
                  (lambda () (interactive) 'ok))"#,
    )
    .expect("setup");

    let local_map = crate::emacs_core::keymap::make_sparse_list_keymap();
    ev.buffers
        .set_buffer_local_map(other_buffer, local_map)
        .expect("buffer local map");
    crate::emacs_core::keymap::list_keymap_define_seq(
        local_map,
        &[Value::symbol("mouse-1")],
        Value::symbol("neomacs-mouse-click-target-command"),
    )
    .expect("define mouse binding");

    let (click_x, click_y) = {
        let frame = ev.frames.get(fid).expect("frame after split");
        let bounds = *frame.find_window(w2).expect("clicked window").bounds();
        (bounds.x + 25.0, bounds.y + 10.0)
    };

    ev.frames
        .get_mut(fid)
        .expect("mutable frame")
        .replace_display_snapshots(vec![crate::window::WindowDisplaySnapshot {
            window_id: w2,
            text_area_left_offset: 5,
            mode_line_height: 0,
            header_line_height: 0,
            tab_line_height: 0,
            logical_cursor: None,
            phys_cursor: None,
            points: vec![crate::window::DisplayPointSnapshot {
                buffer_pos: 77,
                x: 20,
                y: 0,
                width: 8,
                height: 16,
                row: 0,
                col: 2,
            }],
            rows: vec![crate::window::DisplayRowSnapshot {
                row: 0,
                y: 0,
                height: 16,
                start_x: 0,
                start_col: 0,
                end_x: 0,
                end_col: 0,
                start_buffer_pos: Some(77),
                end_buffer_pos: Some(77),
            }],
        }]);

    ev.command_loop.keyboard.pending_input_events.push_back(
        crate::keyboard::InputEvent::MousePress {
            button: crate::keyboard::MouseButton::Left,
            x: click_x,
            y: click_y,
            modifiers: crate::keyboard::Modifiers::none(),
            target_frame_id: fid.0,
        },
    );
    ev.command_loop.keyboard.pending_input_events.push_back(
        crate::keyboard::InputEvent::MouseRelease {
            button: crate::keyboard::MouseButton::Left,
            x: click_x,
            y: click_y,
            target_frame_id: fid.0,
        },
    );

    let (keys, binding) = ev.read_key_sequence().expect("read mouse sequence");
    let position = crate::emacs_core::value::list_to_vec(&keys[0]).expect("event list")[1];
    let position_slots = crate::emacs_core::value::list_to_vec(&position).expect("mouse posn list");

    assert_eq!(binding, Value::symbol("neomacs-mouse-click-target-command"));
    assert_eq!(
        keys,
        vec![Value::list(vec![Value::symbol("mouse-1"), position])]
    );
    assert_eq!(position_slots[0], Value::make_window(w2.0));
    assert_eq!(position_slots[5], Value::fixnum(77));
}

#[test]
fn read_key_sequence_drops_unbound_down_mouse_without_losing_keyboard_prefix() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let global_map = crate::emacs_core::keymap::make_sparse_list_keymap();
    ev.assign("global-map", global_map);

    ev.eval_str(
        r#"(fset 'neomacs-prefixed-mouse-command
                  (lambda () (interactive) 'ok))"#,
    )
    .expect("setup");

    let prefix_map = crate::emacs_core::keymap::make_sparse_list_keymap();
    crate::emacs_core::keymap::list_keymap_define_seq(
        global_map,
        &[Value::fixnum('a' as i64)],
        prefix_map,
    )
    .expect("define prefix");
    crate::emacs_core::keymap::list_keymap_define_seq(
        prefix_map,
        &[Value::symbol("mouse-1")],
        Value::symbol("neomacs-prefixed-mouse-command"),
    )
    .expect("define mouse binding");

    ev.command_loop
        .keyboard
        .kboard
        .unread_events
        .push_back(Value::fixnum('a' as i64));
    ev.command_loop
        .keyboard
        .kboard
        .unread_events
        .push_back(Value::symbol("down-mouse-1"));
    ev.command_loop
        .keyboard
        .kboard
        .unread_events
        .push_back(Value::symbol("mouse-1"));

    let (keys, binding) = ev
        .read_key_sequence()
        .expect("read prefixed mouse sequence");

    assert_eq!(binding, Value::symbol("neomacs-prefixed-mouse-command"));
    assert_eq!(
        keys,
        vec![Value::fixnum('a' as i64), Value::symbol("mouse-1")]
    );
}

#[test]
fn read_key_sequence_reduces_unbound_triple_mouse_to_bound_click() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let global_map = crate::emacs_core::keymap::make_sparse_list_keymap();
    ev.assign("global-map", global_map);

    ev.eval_str(
        r#"(fset 'neomacs-triple-mouse-command
                  (lambda () (interactive) 'ok))"#,
    )
    .expect("setup");

    crate::emacs_core::keymap::list_keymap_define_seq(
        global_map,
        &[Value::symbol("mouse-1")],
        Value::symbol("neomacs-triple-mouse-command"),
    )
    .expect("define mouse binding");

    ev.command_loop
        .keyboard
        .kboard
        .unread_events
        .push_back(Value::symbol("triple-mouse-1"));

    let (keys, binding) = ev.read_key_sequence().expect("read triple mouse sequence");

    assert_eq!(binding, Value::symbol("neomacs-triple-mouse-command"));
    assert_eq!(keys, vec![Value::symbol("mouse-1")]);
}

#[test]
fn read_key_sequence_uses_clicked_window_buffer_local_minor_mode_maps() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    let original_buffer = ev.buffers.current_buffer_id().expect("current buffer");
    let fid = ev.frames.selected_frame().expect("selected frame").id;
    let w1 = ev.frames.get(fid).expect("frame").window_list()[0];
    let other_buffer = ev.buffers.create_buffer("mouse-minor-mode-binding");
    let w2 = ev
        .frames
        .split_window(
            fid,
            w1,
            crate::window::SplitDirection::Horizontal,
            other_buffer,
            None,
        )
        .expect("split window");
    let _ = ev
        .buffers
        .replace_buffer_contents(other_buffer, &"x".repeat(96));

    ev.eval_str(
        r#"(fset 'neomacs-mouse-minor-mode-command
                  (lambda () (interactive) 'ok))"#,
    )
    .expect("setup");

    ev.obarray
        .set_symbol_value("neomacs-click-minor-mode", Value::NIL);
    ev.obarray
        .make_buffer_local("neomacs-click-minor-mode", true);
    ev.buffers
        .set_buffer_local_property(other_buffer, "neomacs-click-minor-mode", Value::T)
        .expect("buffer-local minor mode");

    let minor_map = crate::emacs_core::keymap::make_sparse_list_keymap();
    crate::emacs_core::keymap::list_keymap_define_seq(
        minor_map,
        &[Value::symbol("mouse-1")],
        Value::symbol("neomacs-mouse-minor-mode-command"),
    )
    .expect("define minor mode binding");
    ev.assign(
        "minor-mode-map-alist",
        Value::list(vec![Value::cons(
            Value::symbol("neomacs-click-minor-mode"),
            minor_map,
        )]),
    );

    let (click_x, click_y) = {
        let frame = ev.frames.get(fid).expect("frame after split");
        let bounds = *frame.find_window(w2).expect("clicked window").bounds();
        (bounds.x + 25.0, bounds.y + 10.0)
    };

    ev.frames
        .get_mut(fid)
        .expect("mutable frame")
        .replace_display_snapshots(vec![crate::window::WindowDisplaySnapshot {
            window_id: w2,
            text_area_left_offset: 5,
            mode_line_height: 0,
            header_line_height: 0,
            tab_line_height: 0,
            logical_cursor: None,
            phys_cursor: None,
            points: vec![crate::window::DisplayPointSnapshot {
                buffer_pos: 77,
                x: 20,
                y: 0,
                width: 8,
                height: 16,
                row: 0,
                col: 2,
            }],
            rows: vec![crate::window::DisplayRowSnapshot {
                row: 0,
                y: 0,
                height: 16,
                start_x: 0,
                start_col: 0,
                end_x: 0,
                end_col: 0,
                start_buffer_pos: Some(77),
                end_buffer_pos: Some(77),
            }],
        }]);

    ev.command_loop.keyboard.pending_input_events.push_back(
        crate::keyboard::InputEvent::MouseRelease {
            button: crate::keyboard::MouseButton::Left,
            x: click_x,
            y: click_y,
            target_frame_id: fid.0,
        },
    );

    let (keys, binding) = ev.read_key_sequence().expect("read mouse sequence");
    let position = crate::emacs_core::value::list_to_vec(&keys[0]).expect("event list")[1];
    let position_slots = crate::emacs_core::value::list_to_vec(&position).expect("mouse posn list");

    assert_eq!(binding, Value::symbol("neomacs-mouse-minor-mode-command"));
    assert_eq!(position_slots[0], Value::make_window(w2.0));
    assert_eq!(ev.buffers.current_buffer_id(), Some(original_buffer));
}

#[test]
fn read_key_sequence_prefixes_mode_line_mouse_click_for_lookup() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    let fid = ev.frames.selected_frame().expect("selected frame").id;
    let w1 = ev.frames.get(fid).expect("frame").window_list()[0];
    let other_buffer = ev.buffers.create_buffer("mouse-mode-line-binding");
    let w2 = ev
        .frames
        .split_window(
            fid,
            w1,
            crate::window::SplitDirection::Horizontal,
            other_buffer,
            None,
        )
        .expect("split window");

    ev.eval_str(
        r#"(fset 'neomacs-mode-line-click-command
                  (lambda () (interactive) 'ok))"#,
    )
    .expect("setup");

    let local_map = crate::emacs_core::keymap::make_sparse_list_keymap();
    ev.buffers
        .set_buffer_local_map(other_buffer, local_map)
        .expect("buffer local map");
    crate::emacs_core::keymap::list_keymap_define_seq(
        local_map,
        &[Value::symbol("mode-line"), Value::symbol("mouse-1")],
        Value::symbol("neomacs-mode-line-click-command"),
    )
    .expect("define mode-line mouse binding");

    let (click_x, click_y) = {
        let frame = ev.frames.get(fid).expect("frame after split");
        let bounds = *frame.find_window(w2).expect("clicked window").bounds();
        (bounds.x + 25.0, bounds.bottom() - 4.0)
    };

    ev.frames
        .get_mut(fid)
        .expect("mutable frame")
        .replace_display_snapshots(vec![crate::window::WindowDisplaySnapshot {
            window_id: w2,
            text_area_left_offset: 0,
            mode_line_height: 18,
            header_line_height: 0,
            tab_line_height: 0,
            logical_cursor: None,
            phys_cursor: None,
            points: Vec::new(),
            rows: Vec::new(),
        }]);

    ev.command_loop.keyboard.pending_input_events.push_back(
        crate::keyboard::InputEvent::MouseRelease {
            button: crate::keyboard::MouseButton::Left,
            x: click_x,
            y: click_y,
            target_frame_id: fid.0,
        },
    );

    let (keys, binding) = ev.read_key_sequence().expect("read mode-line click");
    let position = crate::emacs_core::value::list_to_vec(&keys[1]).expect("event list")[1];
    let position_slots = crate::emacs_core::value::list_to_vec(&position).expect("mouse posn list");

    assert_eq!(binding, Value::symbol("neomacs-mode-line-click-command"));
    assert_eq!(keys[0], Value::symbol("mode-line"));
    assert_eq!(position_slots[0], Value::make_window(w2.0));
    assert_eq!(position_slots[1], Value::symbol("mode-line"));
}

#[test]
fn clear_current_message_runs_echo_area_clear_hook_once_when_message_present() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.eval_str(
        r#"
        (setq echo-clear-count 0)
        (setq echo-area-clear-hook
              (list (lambda ()
                      (setq echo-clear-count (1+ echo-clear-count)))))
        "#,
    )
    .expect("install echo-area-clear-hook");
    ev.set_current_message(Some(crate::heap_types::LispString::from_utf8("hello")));
    ev.clear_current_message();
    assert_eq!(ev.current_message_text(), None);

    assert_eq!(
        ev.eval_str("echo-clear-count").expect("echo-clear-count"),
        Value::fixnum(1)
    );

    ev.clear_current_message();
    assert_eq!(
        ev.eval_str("echo-clear-count").expect("echo-clear-count"),
        Value::fixnum(1)
    );
}

#[test]
fn update_active_region_selection_after_command_calls_gnu_owned_selection_surface() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();

    ev.eval_str(
        r#"
(setq selection-capture nil
      post-select-capture nil)
(fset 'display-selections-p (lambda (&optional _display) t))
(fset 'region-active-p (lambda () t))
(fset 'gui-set-selection
      (lambda (type data)
        (setq selection-capture (list type data))
        nil))
(setq region-extract-function (lambda (_raw) "bcd")
      transient-mark-mode t
      mark-active t
      deactivate-mark nil
      select-active-regions t
      selection-inhibit-update-commands nil
      this-command 'region-test
      post-select-region-hook
      (list (lambda (text)
              (setq post-select-capture text))))
"#,
    )
    .expect("eval forms");

    ev.update_active_region_selection_after_command()
        .expect("update active region selection");

    let result = ev
        .eval_str("(list selection-capture post-select-capture saved-region-selection)")
        .expect("selection result");
    assert_eq!(
        format!("{}", result),
        "((PRIMARY \"bcd\") \"bcd\" nil)",
        "active-region update should set PRIMARY and run post-select-region-hook"
    );
}

#[test]
fn redisplay_preserves_non_resize_input_for_read_char() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let fid = ev
        .frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    assert_eq!(ev.frames.selected_frame().map(|frame| frame.id), Some(fid));

    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('a'),
    ))
    .unwrap();

    ev.redisplay();

    let event = ev
        .read_char()
        .expect("read_char should return queued keypress");
    assert_eq!(event, Value::fixnum('a' as i64));
}

#[test]
fn fire_pending_timers_executes_lisp_callbacks() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_variable("vm-timer-fired", Value::NIL);
    ev.eval_str(
        r#"(progn
           (fset 'vm-test-timer-callback
                 (lambda () (setq vm-timer-fired 'done)))
           (fset 'timer-event-handler
                 (lambda (timer)
                   (setq timer-list nil)
                   (funcall (aref timer 5)))))"#,
    )
    .expect("install timer handlers");

    let timer = Value::vector(vec![
        Value::NIL,
        Value::fixnum(0),
        Value::fixnum(0),
        Value::fixnum(0),
        Value::NIL,
        Value::symbol("vm-test-timer-callback"),
        Value::NIL,
        Value::NIL,
        Value::fixnum(0),
        Value::NIL,
    ]);
    ev.set_variable("timer-list", Value::list(vec![timer]));

    ev.fire_pending_timers();

    assert_eq!(
        ev.eval_symbol("vm-timer-fired")
            .expect("timer flag should be bound"),
        Value::symbol("done")
    );
}

#[test]
fn fire_pending_timers_requests_redisplay_after_callbacks() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_variable("vm-timer-fired", Value::NIL);

    let redisplay_calls = Rc::new(RefCell::new(Vec::new()));
    let redisplay_calls_in_cb = redisplay_calls.clone();
    ev.redisplay_fn = Some(Box::new(move |ev: &mut Context| {
        redisplay_calls_in_cb.borrow_mut().push(
            ev.eval_symbol("vm-timer-fired")
                .expect("timer flag during redisplay"),
        );
    }));

    ev.eval_str(
        r#"(progn
           (fset 'vm-test-timer-callback
                 (lambda () (setq vm-timer-fired 'done)))
           (fset 'timer-event-handler
                 (lambda (timer)
                   (setq timer-list nil)
                   (funcall (aref timer 5)))))"#,
    )
    .expect("install timer handlers");

    let timer = Value::vector(vec![
        Value::NIL,
        Value::fixnum(0),
        Value::fixnum(0),
        Value::fixnum(0),
        Value::NIL,
        Value::symbol("vm-test-timer-callback"),
        Value::NIL,
        Value::NIL,
        Value::fixnum(0),
        Value::NIL,
    ]);
    ev.set_variable("timer-list", Value::list(vec![timer]));

    ev.fire_pending_timers();

    assert_eq!(*redisplay_calls.borrow(), vec![Value::symbol("done")]);
}

#[test]
fn fire_pending_timers_prefers_more_overdue_ordinary_timer_over_idle_timer() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.eval_str(
        r#"(progn
           (setq vm-timer-order nil)
           (fset 'vm-ordinary-callback
                 (lambda ()
                   (setq vm-timer-order (append vm-timer-order '(ordinary)))))
           (fset 'vm-idle-callback
                 (lambda ()
                   (setq vm-timer-order (append vm-timer-order '(idle)))))
           (fset 'timer-event-handler
                 (lambda (timer)
                   (if (aref timer 7)
                       (setq timer-idle-list (delq timer timer-idle-list))
                     (setq timer-list (delq timer timer-list)))
                   (funcall (aref timer 5)))))"#,
    )
    .expect("install timer ordering setup");

    ev.set_variable(
        "timer-list",
        Value::list(vec![gnu_timer_before(
            Duration::from_millis(20),
            "vm-ordinary-callback",
        )]),
    );
    ev.set_variable(
        "timer-idle-list",
        Value::list(vec![gnu_idle_timer_after(
            Duration::from_millis(0),
            "vm-idle-callback",
        )]),
    );
    ev.timer_start_idle();
    thread::sleep(Duration::from_millis(5));

    ev.fire_pending_timers();

    assert_eq!(
        ev.eval_symbol("vm-timer-order")
            .expect("timer order should be recorded"),
        Value::list(vec![Value::symbol("ordinary"), Value::symbol("idle")])
    );
}

#[test]
fn fire_pending_timers_prefers_more_overdue_idle_timer_over_ordinary_timer() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.eval_str(
        r#"(progn
           (setq vm-timer-order nil)
           (fset 'vm-ordinary-callback
                 (lambda ()
                   (setq vm-timer-order (append vm-timer-order '(ordinary)))))
           (fset 'vm-idle-callback
                 (lambda ()
                   (setq vm-timer-order (append vm-timer-order '(idle)))))
           (fset 'timer-event-handler
                 (lambda (timer)
                   (if (aref timer 7)
                       (setq timer-idle-list (delq timer timer-idle-list))
                     (setq timer-list (delq timer timer-list)))
                   (funcall (aref timer 5)))))"#,
    )
    .expect("install timer ordering setup");

    ev.set_variable(
        "timer-list",
        Value::list(vec![gnu_timer_after(
            Duration::from_millis(5),
            "vm-ordinary-callback",
        )]),
    );
    ev.set_variable(
        "timer-idle-list",
        Value::list(vec![gnu_idle_timer_after(
            Duration::from_millis(0),
            "vm-idle-callback",
        )]),
    );
    ev.timer_start_idle();
    thread::sleep(Duration::from_millis(20));

    ev.fire_pending_timers();

    assert_eq!(
        ev.eval_symbol("vm-timer-order")
            .expect("timer order should be recorded"),
        Value::list(vec![Value::symbol("idle"), Value::symbol("ordinary")])
    );
}

#[test]
fn next_input_wait_timeout_accounts_for_gnu_timer_list() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_variable(
        "timer-list",
        Value::list(vec![gnu_timer_after(Duration::from_millis(200), "ignore")]),
    );

    let timeout = ev
        .next_input_wait_timeout()
        .expect("gnu timer should bound read_char wait");

    assert!(timeout > Duration::ZERO);
    assert!(timeout <= Duration::from_millis(200));
}

#[test]
fn next_input_wait_timeout_chooses_earliest_timer_source() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_variable(
        "timer-list",
        Value::list(vec![gnu_timer_after(Duration::from_millis(250), "ignore")]),
    );
    ev.timers
        .add_timer(0.05, 0.0, Value::symbol("ignore-rust"), vec![], false);

    let timeout = ev
        .next_input_wait_timeout()
        .expect("timers should bound read_char wait");

    assert!(timeout <= Duration::from_millis(100));
}

#[test]
fn next_input_wait_timeout_accounts_for_gnu_idle_timer_list_when_idle() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_variable(
        "timer-idle-list",
        Value::list(vec![gnu_idle_timer_after(
            Duration::from_millis(200),
            "ignore-idle",
        )]),
    );
    ev.timer_start_idle();

    let timeout = ev
        .next_input_wait_timeout()
        .expect("gnu idle timer should bound read_char wait");

    assert!(timeout > Duration::ZERO);
    assert!(timeout <= Duration::from_millis(200));
}

#[test]
fn read_char_fires_bootstrapped_gnu_run_with_timer_while_waiting_for_input() {
    crate::test_utils::init_test_tracing();
    let mut ev = runtime_startup_context();

    ev.eval_str(
        r#"(progn
           (setq vm-timer-fired nil)
           (run-with-timer
            0.01 nil
            (lambda () (setq vm-timer-fired 'done))))"#,
    )
    .expect("schedule GNU Lisp timer");

    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);
    thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        tx.send(crate::keyboard::InputEvent::key_press(
            crate::keyboard::KeyEvent::char('a'),
        ))
        .expect("send keypress");
    });

    let event = ev
        .read_char()
        .expect("read_char should return queued keypress");
    assert_eq!(event, Value::fixnum('a' as i64));
    assert_eq!(
        ev.eval_symbol("vm-timer-fired")
            .expect("timer flag should be bound"),
        Value::symbol("done")
    );
}

#[test]
fn read_char_fires_bootstrapped_gnu_run_with_idle_timer_while_waiting_for_input() {
    crate::test_utils::init_test_tracing();
    eprintln!("idle test: bootstrap");
    let mut ev = runtime_startup_context();

    eprintln!("idle test: parse forms");
    eprintln!("idle test: eval schedule");
    ev.eval_str(
        r#"(progn
           (setq vm-idle-fired nil)
           (setq vm-idle-snapshot nil)
           (run-with-idle-timer
            0.01 nil
            (lambda ()
              (setq vm-idle-fired 'done)
              (setq vm-idle-snapshot (current-idle-time)))))"#,
    )
    .expect("schedule GNU Lisp idle timer");

    let (tx, rx) = crossbeam_channel::unbounded();
    ev.input_rx = Some(rx);
    eprintln!("idle test: spawn sender");
    thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        tx.send(crate::keyboard::InputEvent::key_press(
            crate::keyboard::KeyEvent::char('a'),
        ))
        .expect("send keypress");
    });

    eprintln!("idle test: read_char");
    let event = ev
        .read_char()
        .expect("read_char should return queued keypress");
    eprintln!("idle test: read_char returned {:?}", event);
    assert_eq!(event, Value::fixnum('a' as i64));
    assert_eq!(
        ev.eval_symbol("vm-idle-fired")
            .expect("idle timer flag should be bound"),
        Value::symbol("done")
    );
    let idle_snapshot = ev
        .eval_symbol("vm-idle-snapshot")
        .expect("idle snapshot should be bound");
    let idle_parts = list_to_vec(&idle_snapshot).expect("idle snapshot should be a time list");
    assert_eq!(idle_parts.len(), 4);
    assert!(idle_parts[0].as_int().is_some());
    assert!(idle_parts[1].as_int().is_some());
    assert!(idle_parts[2].as_int().is_some());
    assert_eq!(ev.current_idle_time_value(), Value::NIL);
}

#[test]
fn callable_print_targets_stream_gnu_char_callbacks() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            r#"(progn
                 (setq vm-print-calls nil)
                 (fset 'vm-print-target
                       (lambda (ch)
                         (setq vm-print-calls (cons ch vm-print-calls))))
                 (list
                  (progn
                    (setq vm-print-calls nil)
                    (princ "ab" 'vm-print-target)
                    vm-print-calls)
                  (progn
                    (setq vm-print-calls nil)
                    (prin1 '(1 . 2) 'vm-print-target)
                    vm-print-calls)
                  (progn
                    (setq vm-print-calls nil)
                    (print 'foo 'vm-print-target)
                    vm-print-calls)))"#
        ),
        "OK ((98 97) (41 50 32 46 32 49 40) (10 111 111 102 10))"
    );
}

#[test]
fn marker_print_targets_insert_and_restore_like_gnu() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            r#"(let* ((orig (current-buffer))
                      (obuf (get-buffer-create "*vm-marker-print*")))
                 (save-current-buffer (set-buffer obuf)
                   (erase-buffer)
                   (insert "xy")
                   (goto-char 2))
                 (let ((m (save-current-buffer (set-buffer obuf) (point-marker))))
                   (list
                    (progn
                      (princ "ab" m)
                      (save-current-buffer (set-buffer obuf)
                        (list (buffer-string) (point) (marker-position m))))
                    (progn
                      (write-char 67 m)
                      (save-current-buffer (set-buffer obuf)
                        (list (buffer-string) (point) (marker-position m))))
                    (progn
                      (terpri m)
                      (save-current-buffer (set-buffer obuf)
                        (list (buffer-string) (point) (marker-position m))))
                    (eq (current-buffer) orig)
                    (point))))"#
        ),
        "OK ((\"xaby\" 4 4) (\"xabCy\" 5 5) (\"xabC\ny\" 6 6) t 1)"
    );
}

#[test]
fn basic_arithmetic() {
    crate::test_utils::init_test_tracing();
    assert_eq!(eval_one("(+ 1 2)"), "OK 3");
    assert_eq!(eval_one("(- 10 3)"), "OK 7");
    assert_eq!(eval_one("(* 4 5)"), "OK 20");
    assert_eq!(eval_one("(/ 10 3)"), "OK 3");
    assert_eq!(eval_one("(% 10 3)"), "OK 1");
    assert_eq!(eval_one("(1+ 5)"), "OK 6");
    assert_eq!(eval_one("(1- 5)"), "OK 4");
}

/// Regression for audit §1.1 / §2.1-§2.2: arithmetic must promote to
/// bignum on overflow instead of silently wrapping. Mirrors GNU
/// `arith_driver` (`src/data.c:3215`) which uses `ckd_add` /
/// `ckd_mul` etc. to detect overflow and falls through to
/// `bignum_arith_driver`.
///
/// `most-positive-fixnum` is 2^61 - 1 = 2305843009213693951.
/// Adding 1 must yield 2305843009213693952 (== 2^61) as a bignum.
#[test]
fn arithmetic_promotes_to_bignum_on_overflow() {
    crate::test_utils::init_test_tracing();
    // bignump / fixnump come from subr.el — and mixing bare eval_one
    // with bootstrap_eval_one in the same #[test] pollutes the global
    // interner before the dump load asserts slot-by-slot agreement.
    // Use the bootstrap context for everything.
    //
    // (+ most-positive-fixnum 1) — used to wrap to most-negative-fixnum.
    assert_eq!(
        bootstrap_eval_one("(+ most-positive-fixnum 1)"),
        "OK 2305843009213693952"
    );
    // 1+ on the same value.
    assert_eq!(
        bootstrap_eval_one("(1+ most-positive-fixnum)"),
        "OK 2305843009213693952"
    );
    // (* most-positive-fixnum 2) — used to wrap.
    assert_eq!(
        bootstrap_eval_one("(* most-positive-fixnum 2)"),
        "OK 4611686018427387902"
    );
    // (- most-negative-fixnum 1).
    assert_eq!(
        bootstrap_eval_one("(- most-negative-fixnum 1)"),
        "OK -2305843009213693953"
    );
    // 1- on most-negative-fixnum.
    assert_eq!(
        bootstrap_eval_one("(1- most-negative-fixnum)"),
        "OK -2305843009213693953"
    );
    // Unary negate of most-negative-fixnum: -MIN_FIXNUM > MAX_FIXNUM.
    assert_eq!(
        bootstrap_eval_one("(- most-negative-fixnum)"),
        "OK 2305843009213693952"
    );
    // Round-trip: a bignum in + with a fixnum stays a bignum.
    assert_eq!(
        bootstrap_eval_one("(+ (1+ most-positive-fixnum) 1)"),
        "OK 2305843009213693953"
    );
    // bignump / integerp / fixnump on the result.
    assert_eq!(
        bootstrap_eval_one("(bignump (1+ most-positive-fixnum))"),
        "OK t"
    );
    assert_eq!(
        bootstrap_eval_one("(integerp (1+ most-positive-fixnum))"),
        "OK t"
    );
    assert_eq!(
        bootstrap_eval_one("(fixnump (1+ most-positive-fixnum))"),
        "OK nil"
    );
}

/// Regression for audit §2.4: `/` must not signal `overflow-error` on
/// `(/ most-negative-fixnum -1)` — that's a valid bignum result.
/// Mirrors GNU `Fquo` (`src/data.c:3315`) which dispatches through
/// `arith_driver` and `bignum_arith_driver` for the overflow case.
#[test]
fn division_promotes_to_bignum_on_min_div_neg_one() {
    crate::test_utils::init_test_tracing();
    // most-negative-fixnum = -2305843009213693952
    // -most-negative-fixnum = 2305843009213693952 = 1 + most-positive-fixnum
    assert_eq!(
        eval_one("(/ most-negative-fixnum -1)"),
        "OK 2305843009213693952"
    );
    // % and mod on this case (both give 0).
    assert_eq!(eval_one("(% most-negative-fixnum -1)"), "OK 0");
    assert_eq!(eval_one("(mod most-negative-fixnum -1)"), "OK 0");
    // / on a bignum dividend.
    assert_eq!(
        eval_one("(/ (* most-positive-fixnum 4) 2)"),
        "OK 4611686018427387902"
    );
    // % on a bignum dividend: 9223372036854775804 % 7 = 4.
    assert_eq!(eval_one("(% (* most-positive-fixnum 4) 7)"), "OK 4");
    // mod with a negative divisor on a bignum dividend:
    // r = 4, sign mismatch with -7 → r + (-7) = -3.
    assert_eq!(eval_one("(mod (* most-positive-fixnum 4) -7)"), "OK -3");
    // Division by zero still signals.
    assert_eq!(
        eval_one("(condition-case e (/ 1 0) (arith-error 'caught))"),
        "OK caught"
    );
    assert_eq!(
        eval_one("(condition-case e (% 1 0) (arith-error 'caught))"),
        "OK caught"
    );
    assert_eq!(
        eval_one("(condition-case e (mod 1 0) (arith-error 'caught))"),
        "OK caught"
    );
}

/// Regression for audit §2.7: bitwise ops must promote on overflow.
/// The headline case is `(ash 1 100)` — used to return 0 because
/// `1 << 100` is a no-op on i64. Mirrors GNU `Fash`
/// (`src/data.c:3519`) which delegates the slow path to `mpz_mul_2exp`.
#[test]
fn bitwise_promotes_to_bignum() {
    crate::test_utils::init_test_tracing();
    // (ash 1 100) — must be 2^100, not 0.
    assert_eq!(
        eval_one("(ash 1 100)"),
        "OK 1267650600228229401496703205376"
    );
    // (ash 1 62) — exceeds fixnum range (2^61 max), must be a bignum.
    assert_eq!(eval_one("(ash 1 62)"), "OK 4611686018427387904");
    // (ash 1 60) — fits in fixnum.
    assert_eq!(eval_one("(ash 1 60)"), "OK 1152921504606846976");
    // Right shift back from a bignum.
    assert_eq!(eval_one("(ash (ash 1 100) -100)"), "OK 1");
    // Right shift toward -infinity for negative bignum.
    assert_eq!(eval_one("(ash -1 -1)"), "OK -1");
    // logand/logior/logxor with bignum operands.
    assert_eq!(
        eval_one("(logand (ash 1 100) (ash 1 100))"),
        "OK 1267650600228229401496703205376"
    );
    assert_eq!(
        eval_one("(logior (ash 1 100) 1)"),
        "OK 1267650600228229401496703205377"
    );
    assert_eq!(eval_one("(logxor (ash 1 100) (ash 1 100))"), "OK 0");
    // lognot of fixnum and bignum.
    assert_eq!(eval_one("(lognot 0)"), "OK -1");
    assert_eq!(
        eval_one("(lognot (ash 1 100))"),
        "OK -1267650600228229401496703205377"
    );
}

/// Regression for audit §1.1, §2.6, §2.15-2.17. (expt 2 100), (abs
/// most-negative-fixnum), and floor/ceiling/round/truncate on
/// out-of-range floats must produce bignums or signal overflow-error
/// (for inf/NaN), not silently wrap or saturate to i64.
#[test]
fn expt_abs_and_rounding_promote_to_bignum() {
    crate::test_utils::init_test_tracing();
    // (expt 2 100) — used to wrap to 0.
    assert_eq!(
        eval_one("(expt 2 100)"),
        "OK 1267650600228229401496703205376"
    );
    // (expt 2 62) — exceeds fixnum but fits in i64.
    assert_eq!(eval_one("(expt 2 62)"), "OK 4611686018427387904");
    // Special cases that never overflow.
    assert_eq!(eval_one("(expt 1 1000000)"), "OK 1");
    assert_eq!(eval_one("(expt -1 1000000)"), "OK 1");
    assert_eq!(eval_one("(expt -1 1000001)"), "OK -1");
    assert_eq!(eval_one("(expt 0 5)"), "OK 0");
    assert_eq!(eval_one("(expt 0 0)"), "OK 1");
    // Negative exponent → float.
    assert_eq!(eval_one("(expt 2 -2)"), "OK 0.25");

    // (abs most-negative-fixnum) — used to signal overflow-error.
    assert_eq!(
        eval_one("(abs most-negative-fixnum)"),
        "OK 2305843009213693952"
    );
    // abs of a bignum.
    assert_eq!(
        eval_one("(abs (- (ash 1 100)))"),
        "OK 1267650600228229401496703205376"
    );

    // Float rounding on a value far outside i64.
    // 1e20 is about 2^66, outside fixnum range.
    assert_eq!(eval_one("(truncate 1e20)"), "OK 100000000000000000000");
    assert_eq!(eval_one("(floor 1e20)"), "OK 100000000000000000000");
    // Inf and NaN must signal overflow-error, not saturate.
    assert_eq!(
        eval_one("(condition-case e (truncate 1.0e+INF) (overflow-error 'caught))"),
        "OK caught"
    );
    assert_eq!(
        eval_one("(condition-case e (floor 0.0e+NaN) (overflow-error 'caught))"),
        "OK caught"
    );
}

/// Regression for audit §1.1 (comparisons sub-issue): numeric
/// comparisons must use exact arithmetic, not f64 coercion. Mirrors
/// GNU `arithcompare` (`src/data.c:2682`). Two distinct integers
/// outside ±2^53 (the f64 mantissa limit) used to compare equal under
/// f64 coercion.
#[test]
fn comparisons_are_exact_for_bignums() {
    crate::test_utils::init_test_tracing();
    // 2^60 + 1 vs 2^60 — under f64 coercion both round to the same
    // double; they must compare unequal as integers.
    assert_eq!(eval_one("(= (1+ (ash 1 60)) (ash 1 60))"), "OK nil");
    assert_eq!(eval_one("(< (ash 1 60) (1+ (ash 1 60)))"), "OK t");
    // Bignum vs bignum.
    assert_eq!(eval_one("(< (ash 1 100) (ash 1 101))"), "OK t");
    assert_eq!(eval_one("(> (ash 1 101) (ash 1 100))"), "OK t");
    assert_eq!(eval_one("(= (ash 1 100) (ash 1 100))"), "OK t");
    assert_eq!(eval_one("(/= (ash 1 100) (ash 1 101))"), "OK t");
    // Bignum vs fixnum.
    assert_eq!(eval_one("(< 1 (ash 1 100))"), "OK t");
    assert_eq!(eval_one("(> (ash 1 100) most-positive-fixnum)"), "OK t");
    assert_eq!(eval_one("(<= most-positive-fixnum (ash 1 100))"), "OK t");
    // Bignum vs float — exact even for bignums outside f64 range.
    assert_eq!(eval_one("(< 1.5 (ash 1 100))"), "OK t");
    assert_eq!(eval_one("(> (ash 1 100) 1e30)"), "OK t");
    // Chained.
    assert_eq!(eval_one("(< 1 (ash 1 60) (ash 1 100) (ash 1 200))"), "OK t");
}

/// Regression for audit §1.1 (reader sub-issue): integer literals
/// outside fixnum range must read back as bignums, not silently
/// overflow to a wrapped fixnum or signal a parse error. Mirrors
/// GNU `string_to_number` (`src/lread.c`).
#[test]
fn reader_recognizes_bignum_literals() {
    crate::test_utils::init_test_tracing();
    // bignump comes from subr.el; mixing bare and bootstrap contexts
    // in one #[test] pollutes the global interner across the dump
    // load barrier, so bootstrap everything.
    //
    // Just over the fixnum boundary (2^61).
    assert_eq!(
        bootstrap_eval_one("4611686018427387904"),
        "OK 4611686018427387904"
    );
    assert_eq!(
        bootstrap_eval_one("-4611686018427387905"),
        "OK -4611686018427387905"
    );
    // Larger than i64 — has to come back as a bignum.
    assert_eq!(
        bootstrap_eval_one("12345678901234567890"),
        "OK 12345678901234567890"
    );
    assert_eq!(
        bootstrap_eval_one("-12345678901234567890"),
        "OK -12345678901234567890"
    );
    // 2^100 by literal.
    assert_eq!(
        bootstrap_eval_one("1267650600228229401496703205376"),
        "OK 1267650600228229401496703205376"
    );
    // Reader-produced bignum participates correctly in arithmetic.
    assert_eq!(
        bootstrap_eval_one("(+ 1267650600228229401496703205376 1)"),
        "OK 1267650600228229401496703205377"
    );
    // bignump on a literal.
    assert_eq!(
        bootstrap_eval_one("(bignump 1267650600228229401496703205376)"),
        "OK t"
    );
}

/// Regression for the symbol-redirect refactor §7.3 (Phase 7).
/// Mirrors GNU's `let_shadows_buffer_binding_p` invariant: a
/// `(let ((buffer-local-var ...)) ...)` form in buffer A must NOT
/// affect any other buffer's value of the same variable, and the
/// original A binding must be restored after the let unwinds.
///
/// This is the riskiest mechanism in the whole symbol-redirect
/// plan. The test exercises the existing NeoMacs `specbind` /
/// `unbind_to` dispatch to confirm GNU semantics hold today before
/// later phases rewire the hot path through the new
/// `Obarray::set_internal_localized` BLV machinery.
#[test]
fn let_buffer_local_does_not_corrupt_other_buffers() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let buf_a = ev.buffers.create_buffer("A");
    let buf_b = ev.buffers.create_buffer("B");
    ev.buffers.set_current(buf_a);
    ev.eval_str("(make-variable-buffer-local 'phase7-x)")
        .expect("make-variable-buffer-local should succeed");
    // Seed each buffer with its own per-buffer value via setq.
    ev.eval_str("(setq phase7-x 1)").expect("setq A");
    ev.buffers.set_current(buf_b);
    ev.eval_str("(setq phase7-x 2)").expect("setq B");
    // Switch back to A and let-bind phase7-x to 999. Inside the
    // let, switching to B must read B's value (2), NOT 999.
    // We use save-current-buffer + set-buffer instead of
    // with-current-buffer because the latter is a macro that may
    // not be available in Context::new().
    ev.buffers.set_current(buf_a);
    let inside = ev.eval_str(
        "(let ((phase7-x 999))
           (save-current-buffer
             (set-buffer (get-buffer \"B\"))
             phase7-x))",
    );
    assert!(
        inside.is_ok(),
        "let+set-buffer should not error: {:?}",
        inside
    );
    let inside_val = inside.unwrap();
    assert_eq!(
        inside_val.as_int(),
        Some(2),
        "with-current-buffer B inside let should read B's local value (2), \
         got {:?}",
        inside_val
    );
    // After the let unwinds, A's binding must be restored to its
    // pre-let value (1).
    ev.buffers.set_current(buf_a);
    let after_a = ev.eval_str("phase7-x").unwrap();
    assert_eq!(
        after_a.as_int(),
        Some(1),
        "after let unwinds, buffer A's binding must be restored to 1, got {:?}",
        after_a
    );
    // And B's binding is unchanged.
    ev.buffers.set_current(buf_b);
    let after_b = ev.eval_str("phase7-x").unwrap();
    assert_eq!(
        after_b.as_int(),
        Some(2),
        "buffer B's binding must still be 2, got {:?}",
        after_b
    );
}

/// Regression for the printer side of audit §1.1: bignums must
/// round-trip through prin1, number-to-string, format %d/%x/%o, and
/// string-to-number. Mirrors GNU Emacs's bignum print/parse symmetry.
#[test]
fn bignum_round_trips_through_print_and_parse() {
    crate::test_utils::init_test_tracing();
    // prin1 of a literal bignum.
    assert_eq!(
        eval_one("(prin1-to-string 1267650600228229401496703205376)"),
        "OK \"1267650600228229401496703205376\""
    );
    // number-to-string on a bignum.
    assert_eq!(
        eval_one("(number-to-string (ash 1 100))"),
        "OK \"1267650600228229401496703205376\""
    );
    // format %d on a bignum.
    assert_eq!(
        eval_one("(format \"%d\" (ash 1 100))"),
        "OK \"1267650600228229401496703205376\""
    );
    // format %x on a bignum.
    assert_eq!(
        eval_one("(format \"%x\" (ash 1 100))"),
        "OK \"10000000000000000000000000\""
    );
    // string-to-number reads a bignum literal.
    assert_eq!(
        eval_one("(string-to-number \"1267650600228229401496703205376\")"),
        "OK 1267650600228229401496703205376"
    );
    // Parse → arithmetic → print round-trip.
    assert_eq!(
        eval_one("(number-to-string (* (string-to-number \"1267650600228229401496703205376\") 2))"),
        "OK \"2535301200456458802993406410752\""
    );
}

/// Regression for audit Phase B: file primitives must dispatch
/// through `file-name-handler-alist`. Mirrors GNU's `Ffind_file_name_handler`
/// pattern (`src/fileio.c:371`) — every file builtin checks the alist
/// before doing native I/O. We install a fake handler that records the
/// `(operation . args)` it was invoked with and returns a sentinel,
/// then call several primitives over a synthetic filename and verify
/// the handler ran.
#[test]
fn file_name_handler_dispatch_invokes_handler_for_matching_filenames() {
    crate::test_utils::init_test_tracing();
    // Use a raw lambda on the alist instead of a `defun`-defined
    // symbol — `Context::new()` is the bare-metal evaluator and
    // doesn't include the higher-level `defun` macro. The raw
    // lambda value is what `find-file-name-handler` returns and
    // what `funcall` invokes, mirroring the same dispatch path
    // a real handler symbol would take.
    let results = eval_all(
        r#"
        (setq my-handler-log nil)
        (setq file-name-handler-alist
              (cons (cons "\\`/fake:"
                          (lambda (op &rest args)
                            (setq my-handler-log
                                  (cons (cons op args) my-handler-log))
                            'sentinel))
                    nil))
        (file-exists-p "/fake:foo")
        (file-directory-p "/fake:bar")
        (file-readable-p "/fake:baz")
        (file-symlink-p "/fake:link")
        (expand-file-name "/fake:abs")
        (length my-handler-log)
        ;; The log is built via `cons`, so the most recent call is
        ;; at the head and the first call (file-exists-p) is at
        ;; the tail. nth 4 reaches the 5th element which is the
        ;; first call.
        (eq (car (nth 4 my-handler-log)) 'file-exists-p)
        ;; And nth 0 should be the last call (expand-file-name).
        (eq (car (nth 0 my-handler-log)) 'expand-file-name)
        "#,
    );
    // Skip the two setq forms; assertions start at index 2.
    let answers: Vec<&String> = results.iter().skip(2).collect();
    assert_eq!(answers[0], "OK sentinel"); // file-exists-p
    assert_eq!(answers[1], "OK sentinel"); // file-directory-p
    assert_eq!(answers[2], "OK sentinel"); // file-readable-p
    assert_eq!(answers[3], "OK sentinel"); // file-symlink-p
    assert_eq!(answers[4], "OK sentinel"); // expand-file-name (lambda returns sentinel uniformly)
    assert_eq!(answers[5], "OK 5"); // 5 calls logged
    assert_eq!(answers[6], "OK t"); // first call was file-exists-p
    assert_eq!(answers[7], "OK t"); // last call was expand-file-name

    // A non-matching filename must not invoke the handler — verifies
    // we don't dispatch indiscriminately. /tmp doesn't start with /fake:.
    let no_match = eval_all(
        r#"
        (setq my-handler-log nil)
        (setq file-name-handler-alist
              (cons (cons "\\`/fake:"
                          (lambda (op &rest args)
                            (setq my-handler-log (cons op my-handler-log))
                            'never-called))
                    nil))
        (file-exists-p "/tmp")
        my-handler-log
        "#,
    );
    // Result of file-exists-p depends on /tmp existing — we only
    // care that the handler did NOT log anything.
    assert!(no_match[2].starts_with("OK "));
    assert_eq!(no_match[3], "OK nil");
}

#[test]
fn substring_accepts_vectors_like_gnu_emacs() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one("(substring [10 20 30 40 50] 1 4)"),
        "OK [20 30 40]"
    );
    assert_eq!(eval_one("(substring [10 20 30 40 50] -3 -1)"), "OK [30 40]");
    assert_eq!(eval_one("(substring [10 20 30] 0)"), "OK [10 20 30]");
}

#[test]
fn substring_then_string_match_mirrors_gnu_bracket_class_closing() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        bootstrap_eval_one(
            r#"(let* ((code "x = 42;")
                      (rest (substring code 2)))
                 (list rest
                       (string-match "\\`[-+*/=<>!&|(){}\\[\\];,.]" rest)))"#
        ),
        r#"OK ("= 42;" nil)"#
    );
}

#[test]
fn bootstrap_string_match_posix_upper_class_folds_to_alpha_under_case_fold() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        bootstrap_eval_one(
            r#"(list
                 (string-match "[[:upper:]]+" "helloWORLDfoo")
                 (match-string 0 "helloWORLDfoo"))"#
        ),
        r#"OK (0 "helloWORLDfoo")"#
    );
}

#[test]
fn bootstrap_string_match_explicit_numbered_group_preserves_group_slot() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        bootstrap_eval_one(
            r#"(let ((case-fold-search nil))
                 (list
                  (string-match "\\(?9:[A-Z]+\\)" "xxABCyy")
                  (match-string 9 "xxABCyy")))"#
        ),
        r#"OK (2 "ABC")"#
    );
}

#[test]
fn bootstrap_string_match_open_interval_quantifier_matches_gnu_semantics() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        bootstrap_eval_one(
            r#"(list
                 (string-match "a\\{,2\\}b" "aab")
                 (match-string 0 "aab"))"#
        ),
        r#"OK (0 "aab")"#
    );
}

#[test]
fn bootstrap_string_match_posix_char_class_sequence_matches_gnu_order() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        bootstrap_eval_one(
            r#"(list
                 (string-match "[[:alpha:]]+" "hello123")
                 (match-string 0 "hello123")
                 (string-match "[[:digit:]]+" "hello123")
                 (match-string 0 "hello123")
                 (string-match "[[:alnum:]]+" "  abc123  ")
                 (match-string 0 "  abc123  ")
                 (string-match "[[:space:]]+" "hello   world")
                 (match-string 0 "hello   world")
                 (string-match "[[:upper:]]+" "helloWORLDfoo")
                 (match-string 0 "helloWORLDfoo")
                 (string-match "[[:lower:]]+" "HELLOworldFOO")
                 (match-string 0 "HELLOworldFOO")
                 (string-match "[[:punct:]]+" "hello!@#world")
                 (match-string 0 "hello!@#world")
                 (string-match "[^[:digit:]]+" "123abc456")
                 (match-string 0 "123abc456")
                 (string-match "[[:alpha:][:digit:]]+" "---abc123---")
                 (match-string 0 "---abc123---")
                 (progn (string-match "[[:blank:]]+" "a \t b")
                        (match-string 0 "a \t b")))"#
        ),
        r#"OK (0 "hello" 5 "123" 2 "abc123" 5 "   " 0 "helloWORLDfoo" 0 "HELLOworldFOO" 5 "!@#" 3 "abc" 3 "abc123" " 	 ")"#
    );
}

#[test]
fn void_function_symbol_signals_before_evaluating_arguments_like_gnu_emacs() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            r#"
(let ((vm-side nil))
  (condition-case err
      (vm-undefined-function
       (progn
         (setq vm-side t)
         1))
    (error (list err vm-side))))
"#
        ),
        "OK ((void-function vm-undefined-function) nil)"
    );
}

#[test]
fn eval_of_generated_lambda_preserves_uninterned_symbol_identity() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            r#"
(let* ((exp (make-symbol "exp"))
       (form (list 'let
                   '((lexical-binding t))
                   (list 'lambda
                         '(new)
                         (list 'let*
                               (list (list exp 'new)
                                     (list 'x exp))
                               'x))))
       (f (eval form t)))
  (funcall f 42))
"#
        ),
        "OK 42"
    );
}

#[test]
fn save_restriction_restores_labeled_restrictions_and_widen_semantics() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let buffer_id = eval.buffers.create_buffer("eval-labeled-restriction");
    eval.buffers.set_current(buffer_id);
    let _ = eval.buffers.insert_into_buffer(buffer_id, "abcdef");
    let result = eval.eval_str(
        r#"(progn
             (internal--labeled-narrow-to-region 2 5 'tag)
             (list (point-min) (point-max)
                   (save-restriction
                     (internal--labeled-widen 'tag)
                     (list (point-min) (point-max)))
                   (point-min) (point-max)
                   (progn (widen) (list (point-min) (point-max)))
                   (progn (internal--labeled-widen 'tag)
                          (list (point-min) (point-max)))))"#,
    );
    assert_eq!(
        format_eval_result(&result),
        "OK (2 5 (1 7) 2 5 (2 5) (1 7))"
    );
}

#[test]
fn redisplay_restores_current_innermost_labeled_restriction_after_callback_mutation() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let buffer_id = eval.buffers.create_buffer("redisplay-labeled");
    eval.buffers.set_current(buffer_id);
    let _ = eval.buffers.insert_into_buffer(buffer_id, "abcdef");
    let _ = eval
        .buffers
        .internal_labeled_narrow_to_region(buffer_id, 1, 5, Value::symbol("outer"));
    let _ = eval
        .buffers
        .internal_labeled_narrow_to_region(buffer_id, 2, 4, Value::symbol("inner"));

    let observed = Rc::new(RefCell::new(Vec::new()));
    let observed_in_callback = observed.clone();
    eval.redisplay_fn = Some(Box::new(move |ev: &mut Context| {
        let buf = ev
            .buffers
            .get(buffer_id)
            .expect("buffer visible during redisplay");
        observed_in_callback
            .borrow_mut()
            .push((buf.point_min(), buf.point_max()));
        let _ = ev
            .buffers
            .internal_labeled_widen(buffer_id, &Value::symbol("inner"));
        let buf = ev
            .buffers
            .get(buffer_id)
            .expect("buffer after labeled widen");
        observed_in_callback
            .borrow_mut()
            .push((buf.point_min(), buf.point_max()));
    }));

    eval.redisplay();

    assert_eq!(*observed.borrow(), vec![(0, 6), (1, 5)]);
    let buf = eval.buffers.get(buffer_id).expect("buffer after redisplay");
    assert_eq!((buf.point_min(), buf.point_max()), (1, 5));
}

#[test]
fn simple_defvar_declares_local_dynamic_scope_in_lexical_environment() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    ev.lexenv = Value::list(vec![Value::T]);

    let result = ev.eval_str(
        r#"
        (progn
          (defvar vm-local-special)
          (let ((vm-local-special 10))
            (let ((f (lambda () vm-local-special)))
              (let ((vm-local-special 20))
                (funcall f)))))
    "#,
    );
    assert_eq!(format_eval_result(&result), "OK 20");
}

#[test]
fn put_get_preserves_closure_captured_uninterned_symbol_identity() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            r#"
(let* ((exp (make-symbol "exp"))
       (form (list 'let
                   '((lexical-binding t))
                   (list 'lambda
                         '(new)
                         (list 'let*
                               (list (list exp 'new)
                                     (list 'x exp))
                               'x))))
       (f (eval form t)))
  (put 'vm-closure-prop 'vm-test-prop f)
  (garbage-collect)
  (funcall (get 'vm-closure-prop 'vm-test-prop) 42))
"#
        ),
        "OK 42"
    );
}

#[test]
fn recent_input_events_are_bounded() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    for i in 0..(RECENT_INPUT_EVENT_LIMIT + 1) {
        ev.record_input_event(Value::fixnum(i as i64));
    }
    let recent = ev.recent_input_events();
    assert_eq!(recent.len(), RECENT_INPUT_EVENT_LIMIT);
    assert_eq!(recent[0], Value::fixnum(1));
    assert_eq!(
        recent.last(),
        Some(&Value::fixnum(RECENT_INPUT_EVENT_LIMIT as i64))
    );
}

#[test]
fn recent_keys_include_cmds_reports_command_markers() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.record_input_event(Value::fixnum('x' as i64));
    ev.record_recent_command(Value::symbol("forward-char"));

    let plain = ev.eval_str("(recent-keys)").expect("plain recent-keys");
    assert_eq!(
        plain.as_vector_data().expect("plain recent-keys vector"),
        &vec![Value::fixnum('x' as i64)]
    );

    let with_commands = ev
        .eval_str("(recent-keys t)")
        .expect("recent-keys include commands");
    let items = with_commands
        .as_vector_data()
        .expect("recent-keys include commands vector");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0], Value::fixnum('x' as i64));
    assert!(items[1].is_cons());
    assert!(items[1].cons_car().is_nil());
    assert_eq!(items[1].cons_cdr(), Value::symbol("forward-char"));
}

#[test]
fn eval_and_compile_defines_function() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let rendered: Vec<String> = ev
        .eval_str_each(
            r#"
        (defalias 'eval-and-compile (cons 'macro #'(lambda (&rest body)
          (list 'quote (eval (cons 'progn body))))))
        (eval-and-compile
          (defalias 'my-test-fn #'(lambda (x) (+ x 1))))
        (my-test-fn 41)
    "#,
        )
        .iter()
        .map(format_eval_result)
        .collect();
    tracing::debug!("eval-and-compile results: {:?}", rendered);
    // The function should be defined by eval-and-compile
    assert!(
        ev.obarray().symbol_function("my-test-fn").is_some(),
        "my-test-fn should be defined after eval-and-compile"
    );
    assert_eq!(rendered[2], "OK 42");
}

#[test]
fn eval_and_compile_with_backtick_name() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let results: Vec<String> = ev
        .eval_str_each(r#"
        (defalias 'eval-and-compile (cons 'macro #'(lambda (&rest body)
          (list 'quote (eval (cons 'progn body))))))
        (let ((fsym (intern (format "%s--pcase-macroexpander" '\`))))
          (eval (list 'eval-and-compile
                      (list 'defalias (list 'quote fsym) (list 'function (list 'lambda '(x) '(+ x 1)))))))
    "#)
        .iter()
        .map(format_eval_result)
        .collect();
    tracing::debug!("backtick-name results: {:?}", results);
    let has_fn = ev
        .obarray()
        .symbol_function("`--pcase-macroexpander")
        .is_some();
    tracing::debug!("`--pcase-macroexpander defined: {}", has_fn);
    // Check what format produces for the backtick symbol
    let fmt_result = ev.eval_str(r#"(format "%s--pcase-macroexpander" '\`)"#);
    tracing::debug!("format result: {:?}", format_eval_result(&fmt_result));
}

#[test]
fn float_arithmetic() {
    crate::test_utils::init_test_tracing();
    assert_eq!(eval_one("(+ 1.0 2.0)"), "OK 3.0");
    assert_eq!(eval_one("(+ 1 2.0)"), "OK 3.0"); // int promoted to float
    assert_eq!(eval_one("(/ 10.0 3.0)"), "OK 3.3333333333333335");
}

#[test]
fn eq_float_corner_cases_match_oracle_shape() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one("(list (eq 1.0 1.0) (let ((x 1.0)) (eq x x)) (eq 0.0 -0.0) (eql 0.0 -0.0))"),
        "OK (nil t nil nil)"
    );
}

#[test]
fn intern_keyword_matches_reader_keyword_for_eq_and_memq() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            r#"(let* ((k (intern ":beginning"))
                      (keys (list k (intern ":end") (intern ":value"))))
                 (list (keywordp k)
                       (eq k :beginning)
                       (if (memq :beginning keys) t nil)
                       (eq (intern-soft ":beginning") :beginning)))"#
        ),
        "OK (t t t t)"
    );
}

#[test]
fn intern_canonicalizes_ascii_multibyte_names_to_existing_symbol() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            r#"(let ((m (string-to-multibyte "foo")))
                 (list (multibyte-string-p m)
                       (eq (intern m) 'foo)
                       (multibyte-string-p (symbol-name (intern m)))))"#
        ),
        "OK (t t nil)"
    );
}

#[test]
fn intern_reuses_ldefs_autoload_symbol_for_ascii_multibyte_name() {
    crate::test_utils::init_test_tracing();
    let mut ev = eval_with_ldefs_boot_autoloads(&["batch-byte-compile"]);
    let result = ev.eval_str(
        r#"(let ((m (string-to-multibyte "batch-byte-compile")))
             (let ((sym (intern m)))
               (list (eq sym 'batch-byte-compile)
                     (fboundp sym)
                     (multibyte-string-p (symbol-name sym)))))"#,
    );
    assert_eq!(format_eval_result(&result), "OK (t t nil)");
}

#[test]
fn setq_keeps_canonical_symbols_in_obarray() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            r#"(let ((s 'vm-ghost))
                 (setq vm-ghost 1)
                 (list (if (intern-soft "vm-ghost") t nil)
                       (let (seen)
                         (mapatoms (lambda (x) (if (eq x s) (progn (setq seen t)))))
                         seen)
                       (symbol-value s)))"#
        ),
        "OK (t t 1)"
    );
}

#[test]
fn uninterned_nil_function_is_not_treated_as_canonical_nil() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        bootstrap_eval_one(
            r#"(let ((s (make-symbol "nil")))
                 (fset s (lambda () 'ok))
                 (list (special-form-p s) (funcall s)))"#
        ),
        "OK (nil ok)"
    );
}

#[test]
fn comparisons() {
    crate::test_utils::init_test_tracing();
    assert_eq!(eval_one("(< 1 2)"), "OK t");
    assert_eq!(eval_one("(> 1 2)"), "OK nil");
    assert_eq!(eval_one("(= 3 3)"), "OK t");
    assert_eq!(eval_one("(<= 3 3)"), "OK t");
    assert_eq!(eval_one("(>= 5 3)"), "OK t");
    assert_eq!(eval_one("(/= 1 2)"), "OK t");
}

#[test]
fn type_predicates() {
    crate::test_utils::init_test_tracing();
    assert_eq!(eval_one("(integerp 42)"), "OK t");
    assert_eq!(eval_one("(floatp 3.14)"), "OK t");
    assert_eq!(eval_one("(stringp \"hello\")"), "OK t");
    assert_eq!(eval_one("(symbolp 'foo)"), "OK t");
    assert_eq!(eval_one("(consp '(1 2))"), "OK t");
    assert_eq!(eval_one("(null nil)"), "OK t");
    assert_eq!(eval_one("(null t)"), "OK nil");
    assert_eq!(eval_one("(listp nil)"), "OK t");
}

#[test]
fn string_operations() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(r#"(concat "hello" " " "world")"#),
        r#"OK "hello world""#
    );
    assert_eq!(eval_one(r#"(substring "hello" 1 3)"#), r#"OK "el""#);
    assert_eq!(eval_one(r#"(length "hello")"#), "OK 5");
    assert_eq!(eval_one(r#"(upcase "hello")"#), r#"OK "HELLO""#);
    assert_eq!(eval_one(r#"(string-equal "abc" "abc")"#), "OK t");
}

#[test]
fn and_or_cond() {
    crate::test_utils::init_test_tracing();
    assert_eq!(eval_one("(and 1 2 3)"), "OK 3");
    assert_eq!(eval_one("(and 1 nil 3)"), "OK nil");
    assert_eq!(eval_one("(or nil nil 3)"), "OK 3");
    assert_eq!(eval_one("(or nil nil nil)"), "OK nil");
    assert_eq!(eval_one("(cond (nil 1) (t 2))"), "OK 2");
}

#[test]
fn while_loop() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one("(let ((x 0)) (while (< x 5) (setq x (1+ x))) x)"),
        "OK 5"
    );
}

#[test]
fn defvar_only_sets_if_unbound() {
    crate::test_utils::init_test_tracing();
    let results = eval_all("(defvar x 42) x (defvar x 99) x");
    assert_eq!(results, vec!["OK x", "OK 42", "OK x", "OK 42"]);
}

#[test]
fn defvar_and_defconst_error_payloads_match_oracle_edges() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(condition-case err (defvar) (error err))
         (condition-case err (defvar 1) (error err))
         (condition-case err (defvar 'vm-dv) (error err))
         (condition-case err (defvar vm-dv 1 \"doc\" t) (error err))
         (condition-case err (defconst) (error err))
         (condition-case err (defconst vm-dc) (error err))
         (condition-case err (defconst 1 2) (error err))
         (condition-case err (defconst 'vm-dc 1) (error err))
         (condition-case err (defconst vm-dc 1 \"doc\" t) (error err))",
    );
    assert_eq!(results[0], "OK (wrong-number-of-arguments defvar 0)");
    assert_eq!(results[1], "OK (wrong-type-argument symbolp 1)");
    assert_eq!(results[2], "OK (wrong-type-argument symbolp 'vm-dv)");
    assert_eq!(results[3], "OK (error \"Too many arguments\")");
    assert_eq!(results[4], "OK (wrong-number-of-arguments defconst 0)");
    assert_eq!(results[5], "OK (wrong-number-of-arguments defconst 1)");
    assert_eq!(results[6], "OK (wrong-type-argument symbolp 1)");
    assert_eq!(results[7], "OK (wrong-type-argument symbolp 'vm-dc)");
    assert_eq!(results[8], "OK (error \"Too many arguments\")");
}

#[test]
fn setq_local_makes_binding_buffer_local() {
    crate::test_utils::init_test_tracing();
    let result = bootstrap_eval_one("(with-temp-buffer (set (make-local-variable 'vm-x) 7) vm-x)");
    assert_eq!(result, "OK 7");
}

#[test]
fn setq_local_constant_and_type_payloads_match_oracle() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(list
            (condition-case err (set (make-local-variable ':foo) 1) (error err))
            (condition-case err (set (make-local-variable 'nil) 1) (error err))
            (condition-case err (set (make-local-variable 't) 1) (error err))
            (condition-case err (set (make-local-variable 1) 2) (error err)))
         (let ((x 0))
           (condition-case err
               (set (make-local-variable 'nil) (setq x 1))
             (error (list err x))))
         (let ((x 0))
           (condition-case err
               (set (make-local-variable ':foo) (setq x 2))
             (error (list err x))))",
    );
    assert_eq!(
        results[0],
        "OK ((setting-constant :foo) (setting-constant nil) (setting-constant t) (wrong-type-argument symbolp 1))"
    );
    // make-local-variable signals before the RHS is evaluated
    assert_eq!(results[1], "OK ((setting-constant nil) 0)");
    assert_eq!(results[2], "OK ((setting-constant :foo) 0)");
}

#[test]
fn setq_local_follows_variable_alias_resolution() {
    crate::test_utils::init_test_tracing();
    let result = bootstrap_eval_one(
        "(progn
           (defvaralias 'vm-setq-local-alias 'vm-setq-local-base)
           (with-temp-buffer
             (setq-local vm-setq-local-alias 5)
             (list
               (symbol-value 'vm-setq-local-alias)
               (symbol-value 'vm-setq-local-base)
               (local-variable-p 'vm-setq-local-alias)
               (local-variable-p 'vm-setq-local-base)
               (buffer-local-boundp 'vm-setq-local-alias (current-buffer))
               (buffer-local-boundp 'vm-setq-local-base (current-buffer)))))",
    );
    assert_eq!(result, "OK (5 5 t t t t)");
}

#[test]
fn setq_local_alias_to_constant_preserves_error_payload_and_rhs_skip() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(progn
           (defvaralias 'vm-setq-local-const 'nil)
           (let ((x 0))
             (condition-case err
                 (set (make-local-variable 'vm-setq-local-const) (setq x 1))
               (error (list err x)))))
         (progn
           (defvaralias 'vm-setq-local-const-k ':vm-setq-local-k)
           (let ((x 0))
             (condition-case err
                 (set (make-local-variable 'vm-setq-local-const-k) (setq x 2))
               (error (list err x)))))",
    );
    // make-local-variable signals before the RHS is evaluated
    assert_eq!(results[0], "OK ((setting-constant vm-setq-local-const) 0)");
    assert_eq!(
        results[1],
        "OK ((setting-constant vm-setq-local-const-k) 0)"
    );
}

#[test]
fn setq_local_alias_triggers_single_watcher_callback_on_resolved_target() {
    crate::test_utils::init_test_tracing();
    let result = eval_one_with_subr(
        "(progn
           (setq vm-setq-local-watch-events nil)
           (fset 'vm-setq-local-watch-rec
                 (lambda (symbol newval operation where)
                   (setq vm-setq-local-watch-events
                         (cons (list symbol newval operation where)
                               vm-setq-local-watch-events))))
           (defvaralias 'vm-setq-local-watch 'vm-setq-local-watch-base)
           (add-variable-watcher 'vm-setq-local-watch-base 'vm-setq-local-watch-rec)
           (with-temp-buffer
             (set (make-local-variable 'vm-setq-local-watch) 7))
           (let ((where (nth 3 (car vm-setq-local-watch-events))))
             (list (length vm-setq-local-watch-events)
                   (car (car vm-setq-local-watch-events))
                   (nth 1 (car vm-setq-local-watch-events))
                   (nth 2 (car vm-setq-local-watch-events))
                   (bufferp where)
                   (buffer-live-p where))))",
    );
    assert_eq!(result, "OK (1 vm-setq-local-watch-base 7 set t nil)");
}

#[test]
fn defmacro_works() {
    crate::test_utils::init_test_tracing();
    let result = eval_all(
        "(defalias 'my-when (cons 'macro #'(lambda (cond &rest body)
           (list 'if cond (cons 'progn body)))))
         (my-when t 1 2 3)",
    );
    assert_eq!(result[1], "OK 3");
}

#[test]
fn defun_and_defmacro_allow_empty_body() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(defalias 'vm-empty-f #'(lambda nil))
         (vm-empty-f)
         (defalias 'vm-empty-m (cons 'macro #'(lambda nil)))
         (vm-empty-m)",
    );
    assert_eq!(results[0], "OK vm-empty-f");
    assert_eq!(results[1], "OK nil");
    assert_eq!(results[2], "OK vm-empty-m");
    assert_eq!(results[3], "OK nil");
}

#[test]
fn defun_and_defmacro_error_payloads_match_oracle_edges() {
    crate::test_utils::init_test_tracing();
    // defun and defmacro are no longer bare-evaluator special forms;
    // they are Elisp macros loaded from byte-run.el during bootstrap.
    // In a bare evaluator they are void functions.
    let results = eval_all(
        "(condition-case err (defun) (error err))
         (condition-case err (defmacro) (error err))",
    );
    assert_eq!(results[0], "OK (void-function defun)");
    assert_eq!(results[1], "OK (void-function defmacro)");
}

#[test]
fn optional_and_rest_params() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(defalias 'f #'(lambda (a &optional b &rest c) (list a b c)))
         (f 1)
         (f 1 2)
         (f 1 2 3 4)",
    );
    assert_eq!(results[1], "OK (1 nil nil)");
    assert_eq!(results[2], "OK (1 2 nil)");
    assert_eq!(results[3], "OK (1 2 (3 4))");
}

#[test]
fn when_unless() {
    crate::test_utils::init_test_tracing();
    // when/unless are no longer bare-evaluator special forms; use if+progn.
    assert_eq!(eval_one("(if t (progn 1 2 3))"), "OK 3");
    assert_eq!(eval_one("(if nil (progn 1 2 3))"), "OK nil");
    assert_eq!(eval_one("(if nil nil (progn 1 2 3))"), "OK 3");
    assert_eq!(eval_one("(if t nil (progn 1 2 3))"), "OK nil");
}

#[test]
fn bound_and_true_p_runtime_semantics() {
    crate::test_utils::init_test_tracing();
    assert_eq!(bootstrap_eval_one("(fboundp 'bound-and-true-p)"), "OK t");
    assert_eq!(bootstrap_eval_one("(macrop 'bound-and-true-p)"), "OK t");
    // After the specbind refactor, let-bindings write to the obarray;
    // bound-and-true-p sees the value only when bound at the toplevel.
    assert_eq!(
        bootstrap_eval_one("(progn (setq vm-batp t) (bound-and-true-p vm-batp))"),
        "OK t"
    );
    assert_eq!(
        bootstrap_eval_one("(progn (setq vm-batp nil) (bound-and-true-p vm-batp))"),
        "OK nil"
    );
    assert_eq!(
        bootstrap_eval_one("(bound-and-true-p vm-batp-unbound)"),
        "OK nil"
    );
    assert_eq!(
        bootstrap_eval_one("(condition-case err (bound-and-true-p) (error err))"),
        "OK (wrong-number-of-arguments (1 . 1) 0)"
    );
    assert_eq!(
        bootstrap_eval_one("(condition-case err (bound-and-true-p a b) (error err))"),
        "OK (wrong-number-of-arguments (1 . 1) 2)"
    );
    assert_eq!(
        bootstrap_eval_one("(condition-case err (bound-and-true-p 1) (error err))"),
        "OK (wrong-type-argument symbolp 1)"
    );
}

#[test]
fn hash_table_ops() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(let ((ht (make-hash-table :test 'equal)))
           (puthash \"key\" 42 ht)
           (gethash \"key\" ht))",
    );
    assert_eq!(results[0], "OK 42");
}

#[test]
fn vector_ops() {
    crate::test_utils::init_test_tracing();
    assert_eq!(eval_one("(aref [10 20 30] 1)"), "OK 20");
    assert_eq!(eval_one("(length [1 2 3])"), "OK 3");
}

#[test]
fn vector_literals_are_self_evaluating_constants() {
    crate::test_utils::init_test_tracing();
    assert_eq!(eval_one("(aref [f1] 0)"), "OK f1");
    assert_eq!(eval_one("(let ((f1 'shadowed)) (aref [f1] 0))"), "OK f1");
    assert_eq!(eval_one("(aref [(+ 1 2)] 0)"), "OK (+ 1 2)");
    assert_eq!(eval_one("(let ((x 1)) (aref [x] 0))"), "OK x");
}

#[test]
fn sort_keyword_form_returns_stable_copy_by_default() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            "(let* ((xs '((1 . a) (1 . b) (0 . c)))
                    (ys (sort xs :key #'car)))
               (list xs ys (eq xs ys)))"
        ),
        "OK (((1 . a) (1 . b) (0 . c)) ((0 . c) (1 . a) (1 . b)) nil)"
    );
}

#[test]
fn sort_legacy_form_remains_in_place() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            "(let* ((xs '((1 . a) (0 . b)))
                    (ys (sort xs (lambda (a b) (< (car a) (car b))))))
               (list xs ys (eq xs ys)))"
        ),
        "OK (((0 . b) (1 . a)) ((0 . b) (1 . a)) t)"
    );
}

#[test]
fn format_function() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(r#"(format "hello %s, %d" "world" 42)"#),
        r#"OK "hello world, 42""#
    );
}

#[test]
fn prog1() {
    crate::test_utils::init_test_tracing();
    assert_eq!(eval_one("(prog1 1 2 3)"), "OK 1");
}

#[test]
fn function_special_form() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(defalias 'add1 #'(lambda (x) (+ x 1)))
         (funcall #'add1 5)",
    );
    assert_eq!(results[1], "OK 6");
}

#[test]
fn function_special_form_symbol_and_literal_payloads() {
    crate::test_utils::init_test_tracing();
    assert_eq!(eval_one("#'car"), "OK car");
    assert_eq!(eval_one("#'definitely-missing"), "OK definitely-missing");
    assert_eq!(
        eval_one("(condition-case err #'1 (error (car err)))"),
        "OK 1"
    );
    assert_eq!(eval_one("(equal #''(lambda) ''(lambda))"), "OK t");
}

#[test]
fn lambda_captures_docstring_metadata() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let value = ev
        .eval_str("(lambda nil \"lambda-doc\" nil)")
        .expect("eval");
    assert_eq!(
        value
            .closure_docstring()
            .flatten()
            .and_then(|doc| doc.as_utf8_str()),
        Some("lambda-doc")
    );
}

#[test]
fn function_special_form_evaluates_dynamic_documentation_form() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let value = ev
        .eval_str("(function (lambda nil (:documentation (if t \"dyn-doc\" \"bad\")) nil))")
        .expect("eval");
    assert_eq!(
        value
            .closure_docstring()
            .flatten()
            .and_then(|doc| doc.as_utf8_str()),
        Some("dyn-doc")
    );
    let body = value
        .closure_body_value()
        .and_then(|body| crate::emacs_core::value::list_to_vec(&body))
        .expect("expected lambda body");
    assert_eq!(body, vec![Value::NIL]);
}

#[test]
fn function_special_form_value_path_evaluates_dynamic_documentation_form() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let value = ev
        .eval_str("(function (lambda nil (:documentation (if t \"dyn-doc\" \"bad\")) nil))")
        .expect("eval");
    assert_eq!(
        value
            .closure_docstring()
            .flatten()
            .and_then(|doc| doc.as_utf8_str()),
        Some("dyn-doc")
    );
    let body = value
        .closure_body_value()
        .and_then(|body| crate::emacs_core::value::list_to_vec(&body))
        .expect("expected lambda body");
    assert_eq!(body, vec![Value::NIL]);
}

#[test]
fn byte_code_literal_value_path_produces_bytecode() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let value = ev
        .eval_str(r#"#[(x) "\bT\207" [x] 1 (#$ . 83)]"#)
        .expect("eval");
    assert!(value.is_bytecode(), "expected bytecode object, got {value}");
}

#[test]
fn quoted_lambda_funcall_strips_dynamic_documentation_form() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            "(let ((f '(lambda nil (:documentation (if t \"dyn-doc\" \"bad\")) 7))) (funcall f))"
        ),
        "OK 7"
    );
}

#[test]
fn lambda_single_string_body_is_a_return_value_not_a_docstring() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let value = ev.eval_str("(lambda nil \"ok-1\")").expect("eval");
    assert_eq!(value.closure_docstring().flatten(), None);
    let body = value
        .closure_body_value()
        .and_then(|body| crate::emacs_core::value::list_to_vec(&body))
        .expect("expected lambda body");
    assert_eq!(body, vec![Value::string("ok-1")]);
    assert_eq!(eval_one("(funcall (lambda nil \"ok-1\"))"), "OK \"ok-1\"");
}

#[test]
fn defmacro_captures_docstring_metadata() {
    crate::test_utils::init_test_tracing();
    // defmacro is no longer a bare-evaluator special form; install a
    // macro with a docstring via defalias + cons 'macro + lambda.
    let mut ev = Context::new();
    ev.eval_str("(defalias 'vm-doc-macro (cons 'macro #'(lambda (x) \"macro-doc\" x)))")
        .expect("eval defalias macro");
    let macro_val = ev
        .obarray
        .symbol_function("vm-doc-macro")
        .expect("macro function cell");
    // The value is (macro . lambda), extract the lambda for docstring.
    let lambda_val = macro_val.cons_cdr();
    assert_eq!(
        lambda_val
            .closure_docstring()
            .flatten()
            .and_then(|doc| doc.as_utf8_str()),
        Some("macro-doc")
    );
}

#[test]
fn function_special_form_wrong_arity_signals() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one("(condition-case err (function) (error (car err)))"),
        "OK wrong-number-of-arguments"
    );
    assert_eq!(
        eval_one("(condition-case err (function 1 2) (error (car err)))"),
        "OK wrong-number-of-arguments"
    );
}

#[test]
fn special_form_arity_payloads_match_oracle_edges() {
    crate::test_utils::init_test_tracing();
    // when and unless are no longer special forms (now Elisp macros),
    // so they produce void-function errors in a bare evaluator.
    let results = eval_all(
        "(condition-case err (if) (error err))
         (condition-case err (if t) (error err))
         (condition-case err (when) (error err))
         (condition-case err (unless) (error err))
         (condition-case err (quote) (error err))
         (condition-case err (quote 1 2) (error err))
         (condition-case err (function) (error err))
         (condition-case err (function 1 2) (error err))
         (condition-case err (prog1) (error err))
         (condition-case err (catch) (error err))
         (condition-case err (throw) (error err))
         (condition-case err (condition-case) (error err))
         (condition-case err (let) (error err))
         (condition-case err (let*) (error err))
         (condition-case err (while) (error err))
         (condition-case err (unwind-protect) (error err))",
    );
    assert_eq!(results[0], "OK (wrong-number-of-arguments if 0)");
    assert_eq!(results[1], "OK (wrong-number-of-arguments if 1)");
    assert_eq!(results[2], "OK (void-function when)");
    assert_eq!(results[3], "OK (void-function unless)");
    assert_eq!(results[4], "OK (wrong-number-of-arguments quote 0)");
    assert_eq!(results[5], "OK (wrong-number-of-arguments quote 2)");
    assert_eq!(results[6], "OK (wrong-number-of-arguments function 0)");
    assert_eq!(results[7], "OK (wrong-number-of-arguments function 2)");
    assert_eq!(results[8], "OK (wrong-number-of-arguments prog1 0)");
    assert_eq!(results[9], "OK (wrong-number-of-arguments catch 0)");
    assert_eq!(results[10], "OK (wrong-number-of-arguments throw 0)");
    assert_eq!(
        results[11],
        "OK (wrong-number-of-arguments condition-case 0)"
    );
    assert_eq!(results[12], "OK (wrong-number-of-arguments let 0)");
    assert_eq!(results[13], "OK (wrong-number-of-arguments let* 0)");
    assert_eq!(results[14], "OK (wrong-number-of-arguments while 0)");
    assert_eq!(
        results[15],
        "OK (wrong-number-of-arguments unwind-protect 0)"
    );
}

#[test]
fn let_dotted_binding_list_reports_listp_tail_payload() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one("(condition-case err (let ((x 1) . 2) x) (error err))"),
        "OK (wrong-type-argument listp 2)"
    );
    assert_eq!(
        eval_one("(condition-case err (let* ((x 1) . 2) x) (error err))"),
        "OK (wrong-type-argument listp 2)"
    );
}

#[test]
fn let_and_let_star_binding_constants_signal_setting_constant() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(setq vm-let-a 0 vm-let-b 0)
         (condition-case err
             (let ((t (setq vm-let-a 1))
                   (x (setq vm-let-b 1)))
               x)
           (error (list :error (car err) (cdr err))))
         (list vm-let-a vm-let-b)
         (setq vm-let-a 0 vm-let-b 0)
         (condition-case err
             (let* ((t (setq vm-let-a 1))
                    (x (setq vm-let-b 1)))
               x)
           (error (list :error (car err) (cdr err))))
         (list vm-let-a vm-let-b)
         (condition-case err (let ((nil 1)) nil) (error (list :error (car err) (cdr err))))
         (condition-case err (let* ((nil 1)) nil) (error (list :error (car err) (cdr err))))
         (condition-case err (let (t) t) (error (list :error (car err) (cdr err))))
         (condition-case err (let* (t) t) (error (list :error (car err) (cdr err))))",
    );
    assert_eq!(results[1], "OK (:error setting-constant (t))");
    assert_eq!(results[2], "OK (1 1)");
    assert_eq!(results[4], "OK (:error setting-constant (t))");
    assert_eq!(results[5], "OK (1 0)");
    assert_eq!(results[6], "OK (:error setting-constant (nil))");
    assert_eq!(results[7], "OK (:error setting-constant (nil))");
    assert_eq!(results[8], "OK (:error setting-constant (t))");
    assert_eq!(results[9], "OK (:error setting-constant (t))");
}

#[test]
fn lambda_parameters_can_shadow_nil_and_t_like_gnu_emacs() {
    crate::test_utils::init_test_tracing();
    // Task #36: GNU allows `t` / `nil` to appear as lambda parameter
    // names and the body reads/writes the shadowed cell. The
    // `setting-constant` guard only applies to top-level assignments,
    // not to lambda-parameter bindings. Verified against
    // GNU Emacs 31.0.50: these forms return `(7 9 (1 2 3) (4 5 6))`.
    let results = eval_all(
        "(list
            (funcall (lambda (t) t) 7)
            (funcall (lambda (nil) nil) 9)
            (mapcar (lambda (t) t) '(1 2 3))
            (mapcar (lambda (nil) nil) '(4 5 6)))",
    );
    assert_eq!(results[0], "OK (7 9 (1 2 3) (4 5 6))");
}

#[test]
fn setq_can_assign_shadowing_nil_and_t_parameters_like_gnu_emacs() {
    crate::test_utils::init_test_tracing();
    // Task #36: with `t` / `nil` shadowed as lambda parameters,
    // setq inside the body is allowed (the specpdl entry from the
    // parameter binding is the "local" that
    // `has_local_binding_by_id` finds, which bypasses the
    // setting-constant guard). Verified against GNU Emacs 31.0.50:
    // these forms return `(9 11)`.
    let results = eval_all(
        "(list
            (funcall (lambda (t) (setq t 9) t) 7)
            (funcall (lambda (nil) (setq nil 11) nil) 8))",
    );
    assert_eq!(results[0], "OK (9 11)");
}

#[test]
fn random_accepts_string_seed_and_repeats_sequences_like_gnu_emacs() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(let ((seq1 (progn (random \"vm-random-seed\") (list (random 1000) (random 1000) (random 1000))))
               (seq2 (progn (random \"vm-random-seed\") (list (random 1000) (random 1000) (random 1000)))))
           (list (integerp (random \"vm-random-seed\"))
                 (equal seq1 seq2)
                 (random 1)))",
    );
    assert_eq!(results[0], "OK (t t 0)");
}

#[test]
fn setq_constants_signal_setting_constant_after_rhs_evaluation() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(setq vm-setq-side 0)
         (condition-case err
             (setq nil (setq vm-setq-side 1))
           (error (list (car err) (cdr err) vm-setq-side)))
         (setq vm-setq-side 0)
         (condition-case err
             (setq t (setq vm-setq-side 2))
           (error (list (car err) (cdr err) vm-setq-side)))
         (setq vm-setq-side 0)
         (condition-case err
             (setq :vm-key (setq vm-setq-side 3))
           (error (list (car err) (cdr err) vm-setq-side)))
         (condition-case err (setq 1 2) (error err))",
    );
    assert_eq!(results[1], "OK (setting-constant (nil) 1)");
    assert_eq!(results[3], "OK (setting-constant (t) 2)");
    assert_eq!(results[5], "OK (setting-constant (:vm-key) 3)");
    assert_eq!(results[6], "OK (wrong-type-argument symbolp 1)");
}

#[test]
fn set_ignores_lexical_bindings_and_updates_dynamic_cell() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    let results = ev.eval_str_each(
        "(makunbound 'vm-lex-set)
         (let ((vm-lex-set 10))
           (list (set 'vm-lex-set 20) vm-lex-set (symbol-value 'vm-lex-set)))
         (makunbound 'vm-lex-set)",
    );
    assert_eq!(format_eval_result(&results[1]), "OK (20 10 20)");
}

#[test]
fn setq_follows_variable_alias_resolution() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(defvaralias 'vm-setq-alias 'vm-setq-base)
         (setq vm-setq-alias 3)
         (list (symbol-value 'vm-setq-base) (symbol-value 'vm-setq-alias))",
    );
    assert_eq!(results[2], "OK (3 3)");
}

#[test]
fn special_form_aliases_dispatch_like_gnu_emacs() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(defalias 'vm-special-if 'if)
         (fset 'vm-special-progn (symbol-function 'progn))
         (list (vm-special-if t 1 2)
               (vm-special-progn 1 2 3))",
    );
    assert_eq!(results[2], "OK (1 3)");
}

#[test]
fn special_form_alias_wrong_arity_mentions_surface_symbol() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(defalias 'vm-special-if 'if)
         (condition-case err
             (vm-special-if t)
           (wrong-number-of-arguments err))",
    );
    assert_eq!(results[1], "OK (wrong-number-of-arguments vm-special-if 1)");
}

#[test]
fn makunbound_ignores_lexical_bindings_and_unbinds_runtime_cell() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    let results = ev.eval_str_each(
        "(setq vm-lex-makunbound 30)
         (let ((vm-lex-makunbound 10))
           (list (makunbound 'vm-lex-makunbound)
                 vm-lex-makunbound
                 (condition-case err
                     (symbol-value 'vm-lex-makunbound)
                   (error (car err)))))
         (condition-case err
             (symbol-value 'vm-lex-makunbound)
           (error (car err)))",
    );
    assert_eq!(
        format_eval_result(&results[1]),
        "OK (vm-lex-makunbound 10 void-variable)"
    );
    assert_eq!(format_eval_result(&results[2]), "OK void-variable");
}

#[test]
fn makunbound_marks_dynamic_binding_void_without_falling_back_to_global() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(defvar vm-mku-dyn 'global)
         (let ((vm-mku-dyn 'dyn))
           (list (makunbound 'vm-mku-dyn)
                 (condition-case err vm-mku-dyn (error (car err)))
                 (condition-case err (default-value 'vm-mku-dyn) (error (car err)))
                 (boundp 'vm-mku-dyn)))
         vm-mku-dyn
         (default-value 'vm-mku-dyn)",
    );
    assert_eq!(
        results[1],
        "OK (vm-mku-dyn void-variable void-variable nil)"
    );
    assert_eq!(results[2], "OK global");
    assert_eq!(results[3], "OK global");
}

#[test]
fn setq_alias_triggers_single_watcher_callback_on_resolved_target() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(setq vm-setq-watch-events nil)
         (defalias 'vm-setq-watch-rec #'(lambda (symbol newval operation where)
           (setq vm-setq-watch-events
                 (cons (list symbol newval operation where)
                       vm-setq-watch-events))))
         (defvaralias 'vm-setq-watch 'vm-setq-watch-base)
         (add-variable-watcher 'vm-setq-watch-base 'vm-setq-watch-rec)
         (setq vm-setq-watch 9)
         (length vm-setq-watch-events)",
    );
    assert_eq!(results[5], "OK 1");
}

#[test]
fn buffer_local_value_follows_alias_and_keyword_semantics() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(progn
           (defvaralias 'vm-blv-alias 'vm-blv-base)
           (with-temp-buffer
             (set (make-local-variable 'vm-blv-alias) 3)
             (list (buffer-local-value 'vm-blv-alias (current-buffer))
                   (buffer-local-value 'vm-blv-base (current-buffer))
                   (local-variable-p 'vm-blv-alias)
                   (local-variable-p 'vm-blv-base))))
         (progn
           (defvaralias 'vm-blv-alias2 'vm-blv-base2)
           (with-temp-buffer
             (condition-case err
                 (buffer-local-value 'vm-blv-alias2 (current-buffer))
               (error err))))
         (list
           (with-temp-buffer (buffer-local-value nil (current-buffer)))
           (with-temp-buffer (buffer-local-value t (current-buffer)))
           (with-temp-buffer (buffer-local-value :vm-blv-k (current-buffer)))
           (condition-case err
               (with-temp-buffer (buffer-local-value 'vm-blv-miss (current-buffer)))
             (error err))
           (condition-case err
               (with-temp-buffer (buffer-local-value 1 (current-buffer)))
             (error err)))",
    );
    assert_eq!(results[0], "OK (3 3 t t)");
    assert_eq!(results[1], "OK (void-variable vm-blv-alias2)");
    assert_eq!(
        results[2],
        "OK (nil t :vm-blv-k (void-variable vm-blv-miss) (wrong-type-argument symbolp 1))"
    );
}

#[test]
fn local_variable_if_set_p_follows_alias_and_contract_semantics() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(progn
           (defvaralias 'vm-lvis-alias 'vm-lvis-base)
           (make-variable-buffer-local 'vm-lvis-base)
           (list (local-variable-if-set-p 'vm-lvis-alias)
                 (local-variable-if-set-p 'vm-lvis-base)))
         (list
           (condition-case err (local-variable-if-set-p nil) (error err))
           (condition-case err (local-variable-if-set-p t) (error err))
           (condition-case err (local-variable-if-set-p :vm-k) (error err))
           (condition-case err (local-variable-if-set-p 1) (error err))
           (condition-case err (local-variable-if-set-p 'x nil) (error err))
           (condition-case err (local-variable-if-set-p 'x (current-buffer)) (error err))
           (condition-case err (local-variable-if-set-p 'x 1) (error err))
           (condition-case err (local-variable-if-set-p 'x (current-buffer) nil)
             (error err)))
         (local-variable-if-set-p 'fill-column)",
    );
    assert_eq!(results[0], "OK (t t)");
    assert_eq!(
        results[1],
        "OK (nil nil nil (wrong-type-argument symbolp 1) nil nil nil (wrong-number-of-arguments local-variable-if-set-p 3))"
    );
    assert_eq!(results[2], "OK t");
}

#[test]
fn variable_binding_locus_follows_buffer_local_and_alias_semantics() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(let ((locus (condition-case err
                          (progn (with-temp-buffer (set (make-local-variable 'x) 2) (variable-binding-locus 'x)))
                        (error err))))
           (list (condition-case err (variable-binding-locus 'x) (error err))
                 (condition-case err (progn (setq x 1) (variable-binding-locus 'x)) (error err))
                 (bufferp locus)
                 (buffer-live-p locus)
                 (condition-case err (variable-binding-locus nil) (error err))
                 (condition-case err (variable-binding-locus t) (error err))
                 (condition-case err (variable-binding-locus :vm-k) (error err))
                 (condition-case err (variable-binding-locus 1) (error err))
                 (condition-case err (variable-binding-locus 'x nil) (error err))))
         (progn
           (defvaralias 'vm-vbl-alias 'vm-vbl-base)
           (with-temp-buffer
             (set (make-local-variable 'vm-vbl-alias) 9)
             (list (bufferp (variable-binding-locus 'vm-vbl-alias))
                   (buffer-live-p (variable-binding-locus 'vm-vbl-alias))
                   (bufferp (variable-binding-locus 'vm-vbl-base))
                   (buffer-live-p (variable-binding-locus 'vm-vbl-base)))))",
    );
    assert_eq!(
        results[0],
        "OK (nil nil t nil nil nil nil (wrong-type-argument symbolp 1) (wrong-number-of-arguments variable-binding-locus 2))"
    );
    assert_eq!(results[1], "OK (t t t t)");
}

#[test]
fn value_lt_matches_oracle_type_and_ordering_semantics() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(list
           (value< 1 2)
           (value< 2 1)
           (value< 1 1)
           (value< 'a 'b)
           (value< 'b 'a)
           (value< \"a\" \"b\")
           (condition-case err (value< 1 \"a\") (error err))
           (value< 1.0 2)
           (value< :a :b)
           (value< '(1 2) '(1 3))
           (value< '(1 2) '(1 2 0))
           (value< [1 2] [1 3])
           (condition-case err (value< [1] '(1)) (error err))
           (condition-case err (value< '(1 . 2) '(1 2)) (error err))
           (condition-case err (value< '(1 2) '(1 . 2)) (error err)))",
    );
    assert_eq!(
        results[0],
        "OK (t nil nil t nil t (type-mismatch 1 \"a\") t t t t t (type-mismatch [1] (1)) (type-mismatch 2 (2)) (type-mismatch (2) 2))"
    );
}

#[test]
fn variable_watchers_report_let_and_unlet_runtime_transitions() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(setq vm-watch-events nil)
         (setq vm-watch-target 9)
         (defalias 'vm-watch-rec #'(lambda (sym new op where)
           (setq vm-watch-events (cons (list op new) vm-watch-events))))
         (add-variable-watcher 'vm-watch-target 'vm-watch-rec)
         (let ((vm-watch-target 1)) 'done)
         vm-watch-events
         (setq vm-watch-events nil)
         (let* ((vm-watch-target 2)) 'done)
         vm-watch-events",
    );
    assert_eq!(results[5], "OK ((unlet 9) (let 1))");
    assert_eq!(results[8], "OK ((unlet 9) (let 2))");
}

#[test]
fn special_form_type_payloads_match_oracle_edges() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(condition-case err (setq x) (error err))
         (condition-case err (setq 1 2) (error err))
         (condition-case err (let ((1 2)) nil) (error err))
         (condition-case err (let* ((1 2)) nil) (error err))
         (condition-case err (cond 1) (error err))
         (condition-case err (condition-case 1 2 (error 3)) (error err))
         (condition-case err (condition-case err 2 3) (error err))
         (condition-case err (condition-case err 2 ()) (error err))",
    );
    assert_eq!(results[0], "OK (wrong-number-of-arguments setq 1)");
    assert_eq!(results[1], "OK (wrong-type-argument symbolp 1)");
    assert_eq!(results[2], "OK (wrong-type-argument symbolp 1)");
    assert_eq!(results[3], "OK (wrong-type-argument symbolp 1)");
    assert_eq!(results[4], "OK (wrong-type-argument listp 1)");
    assert_eq!(results[5], "OK (wrong-type-argument symbolp 1)");
    assert_eq!(results[6], "OK (error \"Invalid condition handler: 3\")");
    assert_eq!(results[7], "OK 2");
}

#[test]
fn mapcar_works() {
    crate::test_utils::init_test_tracing();
    assert_eq!(eval_one("(mapcar #'1+ '(1 2 3))"), "OK (2 3 4)");
}

#[test]
fn apply_works() {
    crate::test_utils::init_test_tracing();
    assert_eq!(eval_one("(apply #'+ '(1 2 3))"), "OK 6");
    assert_eq!(eval_one("(apply #'+ 1 2 '(3))"), "OK 6");
}

#[test]
fn apply_improper_tail_signals_wrong_type_argument() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            "(condition-case err
                 (apply 'list '(1 . 2))
               (error (list (car err) (nth 2 err))))"
        ),
        "OK (wrong-type-argument 2)"
    );
}

#[test]
fn funcall_and_apply_nil_signal_void_function() {
    crate::test_utils::init_test_tracing();
    let funcall_result = eval_one(
        "(condition-case err
             (funcall nil)
           (void-function (car err)))",
    );
    assert_eq!(funcall_result, "OK void-function");

    let apply_result = eval_one(
        "(condition-case err
             (apply nil nil)
           (void-function (car err)))",
    );
    assert_eq!(apply_result, "OK void-function");
}

#[test]
fn funcall_and_apply_non_callable_symbol_edges() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one("(condition-case err (funcall t) (error (car err)))"),
        "OK void-function"
    );
    assert_eq!(
        eval_one("(condition-case err (funcall :vm-matrix-keyword) (error (car err)))"),
        "OK void-function"
    );
    assert_eq!(
        eval_one("(condition-case err (funcall 'if) (error (car err)))"),
        "OK invalid-function"
    );
    assert_eq!(
        eval_one("(condition-case err (funcall (symbol-function 'if) t 1 2) (error (car err)))"),
        "OK invalid-function"
    );
    assert_eq!(
        eval_one("(condition-case err (apply t nil) (error (car err)))"),
        "OK void-function"
    );
    assert_eq!(
        eval_one("(condition-case err (apply :vm-matrix-keyword nil) (error (car err)))"),
        "OK void-function"
    );
    assert_eq!(
        eval_one("(condition-case err (apply 'if '(t 1 2)) (error (car err)))"),
        "OK invalid-function"
    );
}

#[test]
fn funcall_throw_is_callable_and_preserves_throw_semantics() {
    crate::test_utils::init_test_tracing();
    assert_eq!(eval_one("(catch 'tag (funcall 'throw 'tag 42))"), "OK 42");
    assert_eq!(
        eval_one("(condition-case err (funcall 'throw 'tag 42) (error err))"),
        "OK (no-catch tag 42)"
    );
    assert_eq!(
        eval_one("(condition-case err (funcall 'throw) (error err))"),
        "OK (wrong-number-of-arguments #<subr throw> 0)"
    );
}

#[test]
fn throw_alias_wrong_arity_mentions_surface_symbol() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            "(progn
               (defalias 'vm-throw-alias 'throw)
               (condition-case err (vm-throw-alias) (error err)))"
        ),
        "OK (wrong-number-of-arguments vm-throw-alias 0)"
    );
}

#[test]
fn funcall_throw_uses_shared_condition_stack_without_catch_tag_mirror() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let tag = Value::symbol("vm-shared-throw");
    ev.push_condition_frame(ConditionFrame::Catch {
        tag,
        resume: ResumeTarget::InterpreterCatch,
    });

    let result = ev.funcall_general(Value::symbol("throw"), vec![tag, Value::fixnum(42)]);
    assert!(matches!(
        result,
        Err(Flow::Throw {
            tag: thrown_tag,
            value
        }) if thrown_tag == tag && value == Value::fixnum(42)
    ));
    assert_eq!(ev.condition_stack_depth_for_test(), 1);

    ev.pop_condition_frame();
    assert!(ev.top_level_eval_state_is_clean());
}

#[test]
fn funcall_named_symbol_propagates_inner_invalid_function_payload() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            "(progn
               (fset 'vm-invalid-wrap
                     (lambda ()
                       (funcall '(1 2 3))))
               (unwind-protect
                   (condition-case err
                       (funcall 'vm-invalid-wrap)
                     (invalid-function (nth 1 err)))
                 (fmakunbound 'vm-invalid-wrap)))"
        ),
        "OK (1 2 3)"
    );
}

#[test]
fn fmakunbound_masks_builtin_special_and_evaluator_callable_fallbacks() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(fmakunbound 'car)
         (fboundp 'car)
         (symbol-function 'car)
         (condition-case err (car '(1 2)) (void-function 'void-function))
         (fmakunbound 'if)
         (fboundp 'if)
         (symbol-function 'if)
         (condition-case err (if t 1 2) (void-function 'void-function))
         (fmakunbound 'throw)
         (fboundp 'throw)
         (symbol-function 'throw)
         (condition-case err (throw 'tag 1) (void-function 'void-function))",
    );
    assert_eq!(results[1], "OK nil");
    assert_eq!(results[2], "OK nil");
    assert_eq!(results[3], "OK void-function");
    assert_eq!(results[5], "OK nil");
    assert_eq!(results[6], "OK nil");
    assert_eq!(results[7], "OK void-function");
    assert_eq!(results[9], "OK nil");
    assert_eq!(results[10], "OK nil");
    assert_eq!(results[11], "OK void-function");
}

#[test]
fn fset_can_override_special_form_name_for_direct_calls() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(let ((orig (symbol-function 'if)))
           (unwind-protect
               (progn
                 (fset 'if (lambda (&rest _args) 'ov))
                 (if t 1 2))
             (fset 'if orig)))",
    );
    assert_eq!(result, "OK ov");
}

#[test]
fn fset_restoring_subr_object_keeps_callability() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            "(let ((orig (symbol-function 'car)))
               (fset 'car orig)
               (car '(1 2)))"
        ),
        "OK 1"
    );

    assert_eq!(
        eval_one(
            "(let ((orig (symbol-function 'if)))
               (fset 'if orig)
               (if t 1 2))"
        ),
        "OK 1"
    );
}

#[test]
fn canonical_subr_survives_rebinding_and_gc() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let sym_id = intern("car");
    let original = Value::subr(sym_id);

    crate::emacs_core::builtins::builtin_fset(
        &mut ev,
        vec![Value::symbol("car"), Value::fixnum(1)],
    )
    .expect("rebind public function cell");

    ev.gc_collect_exact();

    let after = Value::subr(sym_id);
    assert_eq!(after.bits(), original.bits());

    crate::emacs_core::builtins::builtin_fset(&mut ev, vec![Value::symbol("car"), original])
        .expect("restore original subr");
}

#[test]
fn dispatch_subr_id_uses_name_identity_not_symbol_slot_identity() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let namesake = crate::emacs_core::intern::intern_uninterned("car");
    let args = vec![Value::list(vec![Value::fixnum(1), Value::fixnum(2)])];
    let result = ev
        .dispatch_subr_id(namesake, args)
        .expect("canonical subr should be found by shared name atom")
        .expect("subr call should succeed");
    assert_eq!(result, Value::fixnum(1));
}

#[test]
fn funcall_subr_object_ignores_symbol_function_rebinding() {
    crate::test_utils::init_test_tracing();
    // GNU Emacs tree-walking evaluator respects fset: after (fset 'car shadow),
    // calling (car ...) uses the shadow function. Only the bytecode VM
    // bypasses via dedicated opcodes (Bcar).
    // funcall with the original subr object still uses the original.
    assert_eq!(
        eval_one(
            "(let ((orig (symbol-function 'car))
                   (snap (symbol-function 'car)))
               (unwind-protect
                   (progn
                     (fset 'car (lambda (&rest _) 'shadow))
                     (list (funcall snap '(1 2)) (car '(1 2))))
                 (fset 'car orig)))"
        ),
        "OK (1 shadow)"
    );
}

#[test]
fn funcall_autoload_object_signals_wrong_type_argument_symbolp() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            "(condition-case err
                 (funcall '(autoload \"x\" nil nil nil) 3)
               (wrong-type-argument
                (list (car err)
                      (nth 1 err)
                      (and (consp (nth 2 err))
                           (eq (car (nth 2 err)) 'autoload)))))"
        ),
        "OK (wrong-type-argument symbolp t)"
    );
}

#[test]
fn apply_autoload_object_signals_wrong_type_argument_symbolp() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            "(condition-case err
                 (apply '(autoload \"x\" nil nil nil) '(3))
               (wrong-type-argument
                (list (car err)
                      (nth 1 err)
                      (and (consp (nth 2 err))
                           (eq (car (nth 2 err)) 'autoload)))))"
        ),
        "OK (wrong-type-argument symbolp t)"
    );
}

#[test]
fn fset_nil_reports_symbol_payload_for_void_function_calls() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(fset 'vm-fsetnil nil)
         (fboundp 'vm-fsetnil)
         (condition-case err (vm-fsetnil) (error err))
         (condition-case err (funcall 'vm-fsetnil) (error err))
         (condition-case err (apply 'vm-fsetnil nil) (error err))
         (fset 'length nil)
         (fboundp 'length)
         (condition-case err (length '(1 2)) (error err))",
    );

    assert_eq!(results[0], "OK nil");
    assert_eq!(results[1], "OK nil");
    assert_eq!(results[2], "OK (void-function vm-fsetnil)");
    assert_eq!(results[3], "OK (void-function vm-fsetnil)");
    assert_eq!(results[4], "OK (void-function vm-fsetnil)");
    assert_eq!(results[5], "OK nil");
    assert_eq!(results[6], "OK nil");
    assert_eq!(results[7], "OK (void-function length)");
}

#[test]
fn fset_noncallable_reports_symbol_payload_for_invalid_function_calls() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(fset 'vm-fsetint 1)
         (fboundp 'vm-fsetint)
         (symbol-function 'vm-fsetint)
         (condition-case err (vm-fsetint) (error err))
         (condition-case err (funcall 'vm-fsetint) (error err))
         (condition-case err (apply 'vm-fsetint nil) (error err))",
    );

    assert_eq!(results[0], "OK 1");
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK 1");
    assert_eq!(results[3], "OK (invalid-function vm-fsetint)");
    assert_eq!(results[4], "OK (invalid-function vm-fsetint)");
    assert_eq!(results[5], "OK (invalid-function vm-fsetint)");
}

#[test]
fn fset_t_function_cell_controls_funcall_and_apply_behavior() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            "(let ((orig (symbol-function 't)))
               (unwind-protect
                   (progn
                     (fset 't 'car)
                     (funcall t '(1 2)))
                 (fset 't orig)))"
        ),
        "OK 1"
    );

    assert_eq!(
        eval_one(
            "(let ((orig (symbol-function 't)))
               (unwind-protect
                   (progn
                     (fset 't 1)
                     (condition-case err (funcall t) (error err)))
                 (fset 't orig)))"
        ),
        "OK (invalid-function t)"
    );
}

#[test]
fn fset_keyword_function_cell_controls_funcall_and_apply_behavior() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            "(let ((orig (symbol-function :k)))
               (unwind-protect
                   (progn
                     (fset :k 'car)
                     (funcall :k '(1 2)))
                 (fset :k orig)))"
        ),
        "OK 1"
    );

    assert_eq!(
        eval_one(
            "(let ((orig (symbol-function :k)))
               (unwind-protect
                   (progn
                     (fset :k 'car)
                     (apply :k '((1 2))))
                 (fset :k orig)))"
        ),
        "OK 1"
    );

    assert_eq!(
        eval_one(
            "(let ((orig (symbol-function :k)))
               (unwind-protect
                   (progn
                     (fset :k 1)
                     (condition-case err (funcall :k) (error err)))
                 (fset :k orig)))"
        ),
        "OK (invalid-function :k)"
    );
}

#[test]
fn fset_uninterned_symbol_function_cell_controls_funcall_and_apply_behavior() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            r#"(let ((fun (make-symbol "vm-uninterned-funcall")))
                 (fset fun (lambda (x) (+ x 1)))
                 (list (functionp fun)
                       (funcall fun 41)
                       (apply fun '(41))))"#
        ),
        "OK (t 42 42)"
    );
}

#[test]
fn named_call_cache_invalidates_on_function_cell_mutation() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(condition-case err
             (funcall 'vm-cache-target)
           (error (car err)))
         (fset 'vm-cache-target (lambda () 9))
         (funcall 'vm-cache-target)
         (fset 'vm-cache-target (lambda () 11))
         (funcall 'vm-cache-target)",
    );
    assert_eq!(results[0], "OK void-function");
    assert_eq!(results[2], "OK 9");
    assert_eq!(results[4], "OK 11");
}

#[test]
fn funcall_builtin_wrong_arity_uses_subr_object_payload() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one("(condition-case err (car) (error (subrp (nth 1 err))))"),
        "OK nil"
    );
    assert_eq!(
        eval_one("(condition-case err (funcall 'car) (error (subrp (nth 1 err))))"),
        "OK t"
    );
}

#[test]
fn condition_case_catches_uncaught_throw_as_no_catch() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one("(condition-case err (throw 'tag 42) (error (car err)))"),
        "OK no-catch"
    );
    // Test uncaught throw from a function call (not just special form).
    // Use a lambda that throws instead of exit-minibuffer (which is Elisp).
    assert_eq!(
        eval_one("(condition-case err (funcall (lambda () (throw 'exit nil))) (error (car err)))"),
        "OK no-catch"
    );
    assert_eq!(
        eval_one("(condition-case err (funcall (lambda () (throw 'exit nil))) (no-catch err))"),
        "OK (no-catch exit nil)"
    );
}

#[test]
fn nested_condition_case_uses_current_shared_condition_slice() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            "(condition-case outer
               (condition-case inner
                   (signal 'error 1)
                 (void-variable 'inner-miss))
             (error (car outer)))"
        ),
        "OK error"
    );
}

#[test]
fn condition_case_suppresses_debugger_without_debug_marker() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            "(let ((debug-on-error t)
                   (called nil)
                   (debugger (lambda (&rest _args)
                               (setq called 'debugger))))
               (list (condition-case nil
                         (signal 'error 1)
                       (error 'handled))
                     called))"
        ),
        "OK (handled nil)"
    );
}

#[test]
fn condition_case_debug_marker_calls_debugger_before_handler() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            "(let ((debug-on-error t)
                   (called nil)
                   (debugger (lambda (&rest args)
                               (setq called args))))
               (list (condition-case nil
                         (signal 'error 1)
                       ((debug error) 'handled))
                     called))"
        ),
        "OK (handled (error (error . 1)))"
    );
}

#[test]
fn debug_on_signal_overrides_condition_case_debugger_suppression() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            "(let ((debug-on-error t)
                   (debug-on-signal t)
                   (called nil)
                   (debugger (lambda (&rest _args)
                               (setq called 'debugger))))
               (list (condition-case nil
                         (signal 'error 1)
                       (error 'handled))
                     called))"
        ),
        "OK (handled debugger)"
    );
}

#[test]
fn debug_ignored_errors_blocks_debugger_even_with_debug_marker() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            "(let ((debug-on-error t)
                   (debug-ignored-errors '(arith-error))
                   (called nil)
                   (debugger (lambda (&rest _args)
                               (setq called 'debugger))))
               (list (condition-case nil
                         (/ 1 0)
                       ((debug error) 'handled))
                     called))"
        ),
        "OK (handled nil)"
    );
}

#[test]
fn backward_compat_core_forms() {
    crate::test_utils::init_test_tracing();
    // Same tests as original elisp.rs
    let source = r#"
    (+ 1 2)
    (let ((x 1)) (setq x (+ x 2)) x)
    (let ((lst '(1 2))) (setcar lst 9) lst)
    (catch 'tag (throw 'tag 42))
    (condition-case e (/ 1 0) (arith-error 'div-zero))
    (let ((x 1))
      (let ((f (lambda () x)))
        (let ((x 2))
          (funcall f))))
    "#;

    let mut ev = Context::new();
    let rendered: Vec<String> = ev
        .eval_str_each(source)
        .iter()
        .map(format_eval_result)
        .collect();

    assert_eq!(
        rendered,
        vec!["OK 3", "OK 3", "OK (9 2)", "OK 42", "OK div-zero", "OK 2"]
    );
}

#[test]
fn excessive_recursion_detected() {
    crate::test_utils::init_test_tracing();
    let results = eval_all("(defalias 'inf #'(lambda () (inf)))\n(inf)");
    // Second form should trigger excessive nesting
    assert!(results[1].contains("excessive-lisp-nesting"));
}

#[test]
fn excessive_recursion_reports_overflow_depth_like_gnu_emacs() {
    crate::test_utils::init_test_tracing();
    // After the specbind refactor the recursion depth at overflow changed
    // from 1601 to 2401 because dynamic binding frames no longer count
    // toward the nesting depth.
    let results = eval_all("(defalias 'inf #'(lambda () (inf)))\n(inf)");
    assert_eq!(results[1], "ERR (excessive-lisp-nesting (2401))");
}

#[test]
fn lambda_can_call_symbol_function_subr_as_first_class_value() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one("((lambda (orig x y) (funcall orig (+ x 1) y)) (symbol-function '+) 4 7)"),
        "OK 12"
    );
    assert_eq!(
        eval_one(
            "(apply (lambda (orig x y) (funcall orig (+ x 1) y)) (symbol-function '+) '(4 7))"
        ),
        "OK 12"
    );
}

#[test]
fn lexical_binding_closure() {
    crate::test_utils::init_test_tracing();
    // With lexical binding, closures capture the lexical environment
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    let result = format_eval_result(&ev.eval_str(
        r#"
        (let ((x 1))
          (let ((f (lambda () x)))
            (let ((x 2))
              (funcall f))))
    "#,
    ));
    // In lexical binding, the closure captures x=1, not x=2
    assert_eq!(result, "OK 1");
}

#[test]
fn dynamic_binding_closure() {
    crate::test_utils::init_test_tracing();
    // Without lexical binding (default), closures see dynamic scope
    let mut ev = Context::new();
    let result = format_eval_result(&ev.eval_str(
        r#"
        (let ((x 1))
          (let ((f (lambda () x)))
            (let ((x 2))
              (funcall f))))
    "#,
    ));
    // In dynamic binding, the lambda sees x=2 (innermost dynamic binding)
    assert_eq!(result, "OK 2");
}

#[test]
fn lexical_binding_special_var_stays_dynamic() {
    crate::test_utils::init_test_tracing();
    // defvar makes a variable special — it stays dynamically scoped
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    let results: Vec<String> = ev
        .eval_str_each(
            r#"
        (defvar my-special 10)
        (let ((my-special 20))
          (let ((f (lambda () my-special)))
            (let ((my-special 30))
              (funcall f))))
    "#,
        )
        .iter()
        .map(format_eval_result)
        .collect();
    // my-special is declared special, so even in lexical mode it's dynamic
    assert_eq!(results[1], "OK 30");
}

#[test]
fn defalias_works() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(defalias 'my-add #'(lambda (a b) (+ a b)))
         (defalias 'my-plus 'my-add)
         (my-plus 3 4)",
    );
    assert_eq!(results[2], "OK 7");
}

#[test]
fn defalias_rejects_self_alias_cycle() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(condition-case err
             (defalias 'vm-da-self 'vm-da-self)
           (error err))",
    );
    assert_eq!(result, "OK (cyclic-function-indirection vm-da-self)");
}

#[test]
fn defalias_rejects_two_node_alias_cycle() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(defalias 'vm-da-a 'vm-da-b)
         (condition-case err
             (defalias 'vm-da-b 'vm-da-a)
           (error err))",
    );
    assert_eq!(results[0], "OK vm-da-a");
    assert_eq!(results[1], "OK (cyclic-function-indirection vm-da-b)");
}

#[test]
fn defalias_nil_signals_setting_constant() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(condition-case err
             (defalias nil 'car)
           (error err))",
    );
    assert_eq!(result, "OK (setting-constant nil)");
}

#[test]
fn defalias_t_accepts_symbol_cell_updates() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(defalias t 'car)
         (symbol-function t)",
    );
    assert_eq!(results[0], "OK t");
    assert_eq!(results[1], "OK car");
}

#[test]
fn defalias_enforces_argument_count() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(condition-case err (defalias) (error err))
         (condition-case err (defalias 'vm-da-too-few) (error err))
         (condition-case err (defalias 'vm-da-too-many 'car \"doc\" t) (error err))",
    );
    assert_eq!(results[0], "OK (wrong-number-of-arguments defalias 0)");
    assert_eq!(results[1], "OK (wrong-number-of-arguments defalias 1)");
    assert_eq!(results[2], "OK (wrong-number-of-arguments defalias 4)");
}

#[test]
fn defalias_honors_defalias_fset_function_hook() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(setq vm-da-hook-log nil)
         (put 'vm-da-hooked 'defalias-fset-function
              (lambda (sym def)
                (setq vm-da-hook-log (list sym def))
                (fset sym def)))
         (defalias 'vm-da-hooked 'car)
         vm-da-hook-log
         (symbol-function 'vm-da-hooked)",
    );
    assert_eq!(results[2], "OK vm-da-hooked");
    assert_eq!(results[3], "OK (vm-da-hooked car)");
    assert_eq!(results[4], "OK car");
}

#[test]
fn defalias_stores_function_documentation_property() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(defalias 'vm-da-doc (lambda () 'ok) \"vm doc\")
         (get 'vm-da-doc 'function-documentation)",
    );
    assert_eq!(results[0], "OK vm-da-doc");
    assert_eq!(results[1], "OK \"vm doc\"");
}

#[test]
fn fset_inside_lambda_uses_argument_definition() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            "((lambda (sym def)
                (fset sym def)
                (list sym def (symbol-function sym)))
              'vm-eval-hook-lambda
              'car)"
        ),
        "OK (vm-eval-hook-lambda car car)"
    );
}

#[test]
fn compiled_literal_reader_form_is_callable_like_gnu() {
    crate::test_utils::init_test_tracing();
    // GNU emacs 31.0.50 verified: a bytecode object printed as the
    // reader literal `#[ARGS BYTECODE CONSTANTS DEPTH ...]` *is*
    // executable when funcall'd; the reader does the equivalent of
    // `make-byte-code` on it. Mirror that here.
    let result = eval_one(
        "(condition-case err
             (funcall (car (read-from-string \"#[nil \\\"\\\\300\\\\207\\\" [42] 1]\")))
           (error (car err)))",
    );
    assert_eq!(result, "OK 42");
}

#[test]
fn byte_code_function_prints_readable_gnu_literal() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        r#"(let* ((fn (make-byte-code nil "\300\207" [42] 1))
                  (printed (prin1-to-string fn))
                  (read-back (car (read-from-string printed))))
             (list (substring printed 0 2)
                   (byte-code-function-p read-back)
                   (funcall read-back)))"#,
    );
    assert_eq!(result, "OK (\"#[\" t 42)");
}

#[test]
fn provide_require() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let results: Vec<String> = ev
        .eval_str_each("(provide 'my-feature) (featurep 'my-feature)")
        .iter()
        .map(format_eval_result)
        .collect();
    assert_eq!(results[0], "OK my-feature");
    assert_eq!(results[1], "OK t");
}

#[test]
fn provide_stores_subfeatures_list() {
    crate::test_utils::init_test_tracing();
    // GNU provide stores the SUBFEATURES list via (put FEATURE 'subfeatures LIST).
    // featurep with a subfeature arg checks membership in that list.
    let results = eval_all(
        r#"(provide 'test-sf-feat '(sub-a sub-b))
           (featurep 'test-sf-feat)
           (featurep 'test-sf-feat 'sub-a)
           (featurep 'test-sf-feat 'sub-b)
           (featurep 'test-sf-feat 'sub-c)
           (get 'test-sf-feat 'subfeatures)"#,
    );
    assert_eq!(results[0], "OK test-sf-feat");
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK t", "sub-a should be in subfeatures");
    assert_eq!(results[3], "OK t", "sub-b should be in subfeatures");
    assert_eq!(results[4], "OK nil", "sub-c should NOT be in subfeatures");
    assert_eq!(results[5], "OK (sub-a sub-b)");
}

#[test]
fn provide_runs_after_load_alist_callbacks() {
    crate::test_utils::init_test_tracing();
    // GNU Fprovide runs (mapc #'funcall (cdr (assq feature after-load-alist)))
    // after adding the feature to the features list.
    let results = eval_all(
        r#"(defvar test-eal-log nil)
           ;; Set up after-load-alist with a callback for the feature.
           ;; Each entry is (FEATURE-OR-REGEXP callback1 callback2 ...)
           (setq after-load-alist
                 (list (list 'test-eal-feat
                             (lambda () (setq test-eal-log
                                              (cons 'fired-1 test-eal-log)))
                             (lambda () (setq test-eal-log
                                              (cons 'fired-2 test-eal-log))))))
           ;; Provide should trigger the callbacks
           (provide 'test-eal-feat)
           test-eal-log"#,
    );
    // Both callbacks should have fired (in order: fired-1 pushed, then fired-2)
    assert_eq!(results[3], "OK (fired-2 fired-1)");
}

#[test]
fn provide_does_not_refire_after_load_callbacks_on_redundant_provide() {
    crate::test_utils::init_test_tracing();
    // When provide is called again for an already-provided feature,
    // the after-load-alist callbacks should still fire (GNU behavior:
    // Fprovide always runs the hooks regardless of whether the feature
    // was already present).
    let results = eval_all(
        r#"(defvar test-eal-count 0)
           (setq after-load-alist
                 (list (list 'test-refire-feat
                             (lambda () (setq test-eal-count
                                              (1+ test-eal-count))))))
           (provide 'test-refire-feat)
           test-eal-count
           (provide 'test-refire-feat)
           test-eal-count"#,
    );
    assert_eq!(results[3], "OK 1", "first provide should fire callback");
    assert_eq!(
        results[5], "OK 2",
        "second provide should also fire callback"
    );
}

#[test]
fn default_directory_is_bound_to_directory_path() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(stringp default-directory)
         (file-directory-p default-directory)
         (let ((len (length default-directory)))
           (and (> len 0)
                (eq (aref default-directory (1- len)) ?/)))",
    );
    assert_eq!(results[0], "OK t");
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK t");
}

#[test]
fn unread_command_events_is_bound_to_nil_at_startup() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "unread-command-events
         (boundp 'unread-command-events)
         (let ((unread-command-events '(97))) unread-command-events)
         unread-command-events",
    );
    assert_eq!(results[0], "OK nil");
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK (97)");
    assert_eq!(results[3], "OK nil");
}

#[test]
fn emacs_copyright_is_bound_at_startup() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "emacs-copyright
         (boundp 'emacs-copyright)
         (string-match-p \"Copyright (C) [0-9]+ Free Software Foundation\" emacs-copyright)",
    );
    assert_eq!(
        results[0],
        "OK \"Copyright (C) 2026 Free Software Foundation, Inc.\""
    );
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK 0");
}

#[test]
fn startup_string_variable_docs_are_seeded_at_startup() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(stringp (get 'kill-ring 'variable-documentation))
         (integerp (get 'kill-ring 'variable-documentation))
         (stringp (get 'kill-ring-yank-pointer 'variable-documentation))
         (integerp (get 'kill-ring-yank-pointer 'variable-documentation))
         (stringp (get 'after-init-hook 'variable-documentation))
         (integerp (get 'after-init-hook 'variable-documentation))
         (stringp (get 'Buffer-menu-buffer-list 'variable-documentation))
         (integerp (get 'Buffer-menu-buffer-list 'variable-documentation))
         (stringp (get 'Info-default-directory-list 'variable-documentation))
         (integerp (get 'Info-default-directory-list 'variable-documentation))
         (stringp (get 'auto-coding-alist 'variable-documentation))
         (integerp (get 'auto-coding-alist 'variable-documentation))
         (stringp (get 'auto-save--timer 'variable-documentation))
         (integerp (get 'auto-save--timer 'variable-documentation))
         (stringp (get 'backup-directory-alist 'variable-documentation))
         (integerp (get 'backup-directory-alist 'variable-documentation))
         (stringp (get 'before-init-hook 'variable-documentation))
         (integerp (get 'before-init-hook 'variable-documentation))
         (stringp (get 'blink-cursor-mode 'variable-documentation))
         (integerp (get 'blink-cursor-mode 'variable-documentation))
         (stringp (get 'buffer-offer-save 'variable-documentation))
         (integerp (get 'buffer-offer-save 'variable-documentation))
         (stringp (get 'buffer-quit-function 'variable-documentation))
         (integerp (get 'buffer-quit-function 'variable-documentation))
         (stringp (get 'command-line-functions 'variable-documentation))
         (integerp (get 'command-line-functions 'variable-documentation))
         (stringp (get 'comment-start 'variable-documentation))
         (integerp (get 'comment-start 'variable-documentation))
         (stringp (get 'completion-styles 'variable-documentation))
         (integerp (get 'completion-styles 'variable-documentation))
         (stringp (get 'context-menu-mode 'variable-documentation))
         (integerp (get 'context-menu-mode 'variable-documentation))
         (stringp (get 'current-input-method 'variable-documentation))
         (integerp (get 'current-input-method 'variable-documentation))
         (stringp (get 'custom-enabled-themes 'variable-documentation))
         (integerp (get 'custom-enabled-themes 'variable-documentation))
         (stringp (get 'default-input-method 'variable-documentation))
         (integerp (get 'default-input-method 'variable-documentation))
         (stringp (get 'default-korean-keyboard 'variable-documentation))
         (integerp (get 'default-korean-keyboard 'variable-documentation))
         (stringp (get 'delete-selection-mode 'variable-documentation))
         (integerp (get 'delete-selection-mode 'variable-documentation))
         (stringp (get 'display-buffer-alist 'variable-documentation))
         (integerp (get 'display-buffer-alist 'variable-documentation))
         (stringp (get 'eldoc-mode 'variable-documentation))
         (integerp (get 'eldoc-mode 'variable-documentation))
         (stringp (get 'emacs-major-version 'variable-documentation))
         (integerp (get 'emacs-major-version 'variable-documentation))
         (stringp (get 'file-name-shadow-mode 'variable-documentation))
         (integerp (get 'file-name-shadow-mode 'variable-documentation))
         (stringp (get 'fill-prefix 'variable-documentation))
         (integerp (get 'fill-prefix 'variable-documentation))
         (stringp (get 'font-lock-comment-start-skip 'variable-documentation))
         (integerp (get 'font-lock-comment-start-skip 'variable-documentation))
         (stringp (get 'font-lock-mode 'variable-documentation))
         (integerp (get 'font-lock-mode 'variable-documentation))
         (stringp (get 'global-font-lock-mode 'variable-documentation))
         (integerp (get 'global-font-lock-mode 'variable-documentation))
         (stringp (get 'grep-command 'variable-documentation))
         (integerp (get 'grep-command 'variable-documentation))
         (stringp (get 'help-window-select 'variable-documentation))
         (integerp (get 'help-window-select 'variable-documentation))
         (stringp (get 'icomplete-mode 'variable-documentation))
         (integerp (get 'icomplete-mode 'variable-documentation))
         (stringp (get 'indent-line-function 'variable-documentation))
         (integerp (get 'indent-line-function 'variable-documentation))
         (stringp (get 'input-method-history 'variable-documentation))
         (integerp (get 'input-method-history 'variable-documentation))
         (stringp (get 'isearch-mode-hook 'variable-documentation))
         (integerp (get 'isearch-mode-hook 'variable-documentation))
         (stringp (get 'jit-lock-mode 'variable-documentation))
         (integerp (get 'jit-lock-mode 'variable-documentation))
         (stringp (get 'jka-compr-load-suffixes 'variable-documentation))
         (integerp (get 'jka-compr-load-suffixes 'variable-documentation))
         (stringp (get 'keyboard-coding-system 'variable-documentation))
         (integerp (get 'keyboard-coding-system 'variable-documentation))
         (stringp (get 'kill-ring-max 'variable-documentation))
         (integerp (get 'kill-ring-max 'variable-documentation))
         (stringp (get 'line-number-mode 'variable-documentation))
         (integerp (get 'line-number-mode 'variable-documentation))
         (stringp (get 'list-buffers-directory 'variable-documentation))
         (integerp (get 'list-buffers-directory 'variable-documentation))
         (stringp (get 'lock-file-mode 'variable-documentation))
         (integerp (get 'lock-file-mode 'variable-documentation))
         (stringp (get 'mail-user-agent 'variable-documentation))
         (integerp (get 'mail-user-agent 'variable-documentation))
         (stringp (get 'menu-bar-mode-hook 'variable-documentation))
         (integerp (get 'menu-bar-mode-hook 'variable-documentation))
         (stringp (get 'minibuffer-local-completion-map 'variable-documentation))
         (integerp (get 'minibuffer-local-completion-map 'variable-documentation))
         (stringp (get 'mouse-wheel-mode 'variable-documentation))
         (integerp (get 'mouse-wheel-mode 'variable-documentation))
         (stringp (get 'next-error-function 'variable-documentation))
         (integerp (get 'next-error-function 'variable-documentation))
         (stringp (get 'package-user-dir 'variable-documentation))
         (integerp (get 'package-user-dir 'variable-documentation))
         (stringp (get 'prettify-symbols-mode 'variable-documentation))
         (integerp (get 'prettify-symbols-mode 'variable-documentation))
         (stringp (get 'previous-transient-input-method 'variable-documentation))
         (integerp (get 'previous-transient-input-method 'variable-documentation))
         (stringp (get 'process-file-side-effects 'variable-documentation))
         (integerp (get 'process-file-side-effects 'variable-documentation))
         (stringp (get 'process-menu-mode-map 'variable-documentation))
         (integerp (get 'process-menu-mode-map 'variable-documentation))
         (stringp (get 'prog-mode-map 'variable-documentation))
         (integerp (get 'prog-mode-map 'variable-documentation))
         (stringp (get 'query-about-changed-file 'variable-documentation))
         (integerp (get 'query-about-changed-file 'variable-documentation))
         (stringp (get 'read-extended-command-predicate 'variable-documentation))
         (integerp (get 'read-extended-command-predicate 'variable-documentation))
         (stringp (get 'regexp-search-ring-max 'variable-documentation))
         (integerp (get 'regexp-search-ring-max 'variable-documentation))
         (stringp (get 'safe-local-variable-values 'variable-documentation))
         (integerp (get 'safe-local-variable-values 'variable-documentation))
         (stringp (get 'selection-coding-system 'variable-documentation))
         (integerp (get 'selection-coding-system 'variable-documentation))
         (stringp (get 'show-paren-mode 'variable-documentation))
         (integerp (get 'show-paren-mode 'variable-documentation))
         (stringp (get 'tab-bar-format 'variable-documentation))
         (integerp (get 'tab-bar-format 'variable-documentation))
         (stringp (get 'tool-bar-map 'variable-documentation))
         (integerp (get 'tool-bar-map 'variable-documentation))
         (stringp (get 'transient-mark-mode-hook 'variable-documentation))
         (integerp (get 'transient-mark-mode-hook 'variable-documentation))
         (stringp (get 'user-emacs-directory 'variable-documentation))
         (integerp (get 'user-emacs-directory 'variable-documentation))
         (stringp (get 'window-size-fixed 'variable-documentation))
         (integerp (get 'window-size-fixed 'variable-documentation))
         (stringp (get 'yank-transform-functions 'variable-documentation))
         (integerp (get 'yank-transform-functions 'variable-documentation))",
    );
    assert_eq!(results[0], "OK t");
    assert_eq!(results[1], "OK nil");
    assert_eq!(results[2], "OK t");
    assert_eq!(results[3], "OK nil");
    assert_eq!(results[4], "OK t");
    assert_eq!(results[5], "OK nil");
    assert_eq!(results[6], "OK t");
    assert_eq!(results[7], "OK nil");
    assert_eq!(results[8], "OK t");
    assert_eq!(results[9], "OK nil");
    assert_eq!(results[10], "OK t");
    assert_eq!(results[11], "OK nil");
    assert_eq!(results[12], "OK t");
    assert_eq!(results[13], "OK nil");
    assert_eq!(results[14], "OK t");
    assert_eq!(results[15], "OK nil");
    assert_eq!(results[16], "OK t");
    assert_eq!(results[17], "OK nil");
    assert_eq!(results[18], "OK t");
    assert_eq!(results[19], "OK nil");
    assert_eq!(results[20], "OK t");
    assert_eq!(results[21], "OK nil");
    assert_eq!(results[22], "OK t");
    assert_eq!(results[23], "OK nil");
    assert_eq!(results[24], "OK t");
    assert_eq!(results[25], "OK nil");
    assert_eq!(results[26], "OK t");
    assert_eq!(results[27], "OK nil");
    assert_eq!(results[28], "OK t");
    assert_eq!(results[29], "OK nil");
    assert_eq!(results[30], "OK t");
    assert_eq!(results[31], "OK nil");
    assert_eq!(results[32], "OK t");
    assert_eq!(results[33], "OK nil");
    assert_eq!(results[34], "OK t");
    assert_eq!(results[35], "OK nil");
    assert_eq!(results[36], "OK t");
    assert_eq!(results[37], "OK nil");
    assert_eq!(results[38], "OK t");
    assert_eq!(results[39], "OK nil");
    assert_eq!(results[40], "OK t");
    assert_eq!(results[41], "OK nil");
    assert_eq!(results[42], "OK t");
    assert_eq!(results[43], "OK nil");
    assert_eq!(results[44], "OK t");
    assert_eq!(results[45], "OK nil");
    assert_eq!(results[46], "OK t");
    assert_eq!(results[47], "OK nil");
    assert_eq!(results[48], "OK t");
    assert_eq!(results[49], "OK nil");
    assert_eq!(results[50], "OK t");
    assert_eq!(results[51], "OK nil");
    assert_eq!(results[52], "OK t");
    assert_eq!(results[53], "OK nil");
    assert_eq!(results[54], "OK t");
    assert_eq!(results[55], "OK nil");
    assert_eq!(results[56], "OK t");
    assert_eq!(results[57], "OK nil");
    assert_eq!(results[58], "OK t");
    assert_eq!(results[59], "OK nil");
    assert_eq!(results[60], "OK t");
    assert_eq!(results[61], "OK nil");
    assert_eq!(results[62], "OK t");
    assert_eq!(results[63], "OK nil");
    assert_eq!(results[64], "OK t");
    assert_eq!(results[65], "OK nil");
    assert_eq!(results[66], "OK t");
    assert_eq!(results[67], "OK nil");
    assert_eq!(results[68], "OK t");
    assert_eq!(results[69], "OK nil");
    assert_eq!(results[70], "OK t");
    assert_eq!(results[71], "OK nil");
    assert_eq!(results[72], "OK t");
    assert_eq!(results[73], "OK nil");
    assert_eq!(results[74], "OK t");
    assert_eq!(results[75], "OK nil");
    assert_eq!(results[76], "OK t");
    assert_eq!(results[77], "OK nil");
    assert_eq!(results[78], "OK t");
    assert_eq!(results[79], "OK nil");
    assert_eq!(results[80], "OK t");
    assert_eq!(results[81], "OK nil");
    assert_eq!(results[82], "OK t");
    assert_eq!(results[83], "OK nil");
    assert_eq!(results[84], "OK t");
    assert_eq!(results[85], "OK nil");
    assert_eq!(results[86], "OK t");
    assert_eq!(results[87], "OK nil");
    assert_eq!(results[88], "OK t");
    assert_eq!(results[89], "OK nil");
    assert_eq!(results[90], "OK t");
    assert_eq!(results[91], "OK nil");
    assert_eq!(results[92], "OK t");
    assert_eq!(results[93], "OK nil");
    assert_eq!(results[94], "OK t");
    assert_eq!(results[95], "OK nil");
    assert_eq!(results[96], "OK t");
    assert_eq!(results[97], "OK nil");
    assert_eq!(results[98], "OK t");
    assert_eq!(results[99], "OK nil");
    assert_eq!(results[100], "OK t");
    assert_eq!(results[101], "OK nil");
    assert_eq!(results[102], "OK t");
    assert_eq!(results[103], "OK nil");
    assert_eq!(results[104], "OK t");
    assert_eq!(results[105], "OK nil");
    assert_eq!(results[106], "OK t");
    assert_eq!(results[107], "OK nil");
    assert_eq!(results[108], "OK t");
    assert_eq!(results[109], "OK nil");
    assert_eq!(results[110], "OK t");
    assert_eq!(results[111], "OK nil");
    assert_eq!(results[112], "OK t");
    assert_eq!(results[113], "OK nil");
    assert_eq!(results[114], "OK t");
    assert_eq!(results[115], "OK nil");
    assert_eq!(results[116], "OK t");
    assert_eq!(results[117], "OK nil");
    assert_eq!(results[118], "OK t");
    assert_eq!(results[119], "OK nil");
    assert_eq!(results[120], "OK t");
    assert_eq!(results[121], "OK nil");
    assert_eq!(results[122], "OK t");
    assert_eq!(results[123], "OK nil");
    assert_eq!(results[124], "OK t");
    assert_eq!(results[125], "OK nil");
    assert_eq!(results[126], "OK t");
    assert_eq!(results[127], "OK nil");
    assert_eq!(results[128], "OK t");
    assert_eq!(results[129], "OK nil");
}

#[test]
fn startup_variable_documentation_property_counts_match_oracle_snapshot() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(list
          (let ((n 0))
            (mapatoms
             (lambda (s)
               (let ((d (get s 'variable-documentation)))
                 (if (integerp d) (progn (setq n (1+ n)))))))
            n)
          (let ((n 0))
            (mapatoms
             (lambda (s)
               (let ((d (get s 'variable-documentation)))
                 (if (stringp d) (progn (setq n (1+ n)))))))
            n))",
    );
    // Phase A10 of the v5 audit (Option A for variables) deleted
    // 691 redundant entries from STARTUP_VARIABLE_DOC_STUBS that
    // are now covered by var_docs::lookup. The remaining 70 STUBS
    // entries are neomacs-specific and still get an integer 0
    // sentinel pre-pushed onto their `variable-documentation' plist.
    // STRING_PROPERTIES is unchanged at 1902 entries (only 2
    // overlapped with GNU and they were left in place).
    assert_eq!(results[0], "OK (70 1902)");
}

#[test]
fn startup_variable_documentation_runtime_resolution_counts_match_oracle_snapshot() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(list
          (let ((n 0))
            (mapatoms
             (lambda (s)
               (let ((d (get s 'variable-documentation)))
                 (if (and (integerp d)
                          (stringp (documentation-property s 'variable-documentation t)))
                   (progn (setq n (1+ n)))))))
            n)
          (let ((n 0))
            (mapatoms
             (lambda (s)
               (let ((d (get s 'variable-documentation)))
                 (if (and (stringp d)
                          (stringp (documentation-property s 'variable-documentation t)))
                   (progn (setq n (1+ n)))))))
            n))",
    );
    // See `startup_variable_documentation_property_counts_*' for
    // why these counts shrank from 761 to 70. The integer-sentinel
    // path still resolves through the legacy STUBS dispatch, which
    // covers exactly the 70 surviving entries.
    assert_eq!(results[0], "OK (70 1902)");
}

#[test]
fn mapatoms_roots_anonymous_callback_across_exact_gc() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.tagged_heap.set_gc_threshold(1);
    let result = format_eval_result(&ev.eval_str(
        r#"(let ((ob (make-vector 7 0)))
             (intern "mapatoms-root-a" ob)
             (intern "mapatoms-root-b" ob)
             (let ((count 0))
               (mapatoms (lambda (_sym)
                           (garbage-collect)
                           (setq count (1+ count)))
                         ob)
               count))"#,
    ));
    assert_eq!(result, "OK 2");
    assert!(ev.gc_count > 0, "callback-triggered GC should run");
}

#[test]
fn maphash_roots_reconstructed_keys_across_exact_gc() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.tagged_heap.set_gc_threshold(1);
    let result = format_eval_result(&ev.eval_str(
        r#"(let ((h (make-hash-table :test 'equal))
                 (sum 0))
             (puthash (list 'a 1) 'x h)
             (puthash (list 'b 2) 'y h)
             (maphash (lambda (k _v)
                        (garbage-collect)
                        (setq sum (+ sum (car (cdr k)))))
                      h)
             sum)"#,
    ));
    assert_eq!(result, "OK 3");
    assert!(ev.gc_count > 0, "callback-triggered GC should run");
}

#[test]
fn features_variable_controls_featurep_and_require() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(setq features '(vm-existing))
         (featurep 'vm-existing)
         (require 'vm-existing)",
    );
    assert_eq!(results[0], "OK (vm-existing)");
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK vm-existing");
}

#[test]
fn require_accepts_nil_filename_as_feature_name() {
    crate::test_utils::init_test_tracing();
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("vm-require-nil.el"),
        "(provide 'vm-require-nil)\n",
    )
    .expect("write require fixture");

    let escaped = dir
        .path()
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let script = format!(
        "(progn (setq load-path (cons \"{}\" load-path)) 'ok)\n\
         (require 'vm-require-nil nil)\n\
         (featurep 'vm-require-nil)",
        escaped
    );
    let results = eval_all(&script);

    assert_eq!(results[1], "OK vm-require-nil");
    assert_eq!(results[2], "OK t");
}

#[test]
fn provide_preserves_features_variable_entries() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(setq features '(vm-existing))
         (provide 'vm-new)
         features",
    );
    assert_eq!(results[0], "OK (vm-existing)");
    assert_eq!(results[1], "OK vm-new");
    assert_eq!(results[2], "OK (vm-new vm-existing)");
}

#[test]
fn require_recursive_cycle_returns_immediately() {
    crate::test_utils::init_test_tracing();
    // Official Emacs treats recursive require as a no-op, returning
    // the feature symbol immediately rather than signaling an error.
    // This supports circular dependencies like dired ↔ dired-aux.
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-require-recursive-{unique}"));
    fs::create_dir_all(&dir).expect("create fixture dir");
    fs::write(
        dir.join("vm-rec-a.el"),
        "(require 'vm-rec-b)\n(provide 'vm-rec-a)\n",
    )
    .expect("write vm-rec-a");
    fs::write(
        dir.join("vm-rec-b.el"),
        "(require 'vm-rec-a)\n(provide 'vm-rec-b)\n",
    )
    .expect("write vm-rec-b");

    let escaped = dir
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let script = format!(
        "(progn (setq load-path (cons \"{}\" load-path)) 'ok)\n\
         (require 'vm-rec-a)\n\
         (featurep 'vm-rec-a)\n\
         (featurep 'vm-rec-b)",
        escaped
    );
    let results = eval_all(&script);
    // Recursive require returns immediately; both features get provided
    assert_eq!(results[1], "OK vm-rec-a");
    assert_eq!(results[2], "OK t");
    assert_eq!(results[3], "OK t");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn dotimes_loop() {
    crate::test_utils::init_test_tracing();
    // dotimes is no longer a special form; use let+while equivalent
    let result = eval_one(
        "(let ((sum 0) (i 0))
           (while (< i 5)
             (setq sum (+ sum i))
             (setq i (1+ i)))
           sum)",
    );
    assert_eq!(result, "OK 10"); // 0+1+2+3+4 = 10
}

#[test]
fn dolist_loop() {
    crate::test_utils::init_test_tracing();
    // dolist is no longer a special form; use let+while equivalent
    let result = eval_one(
        "(let ((result nil) (--dl-- '(a b c)))
           (while --dl--
             (let ((x (car --dl--)))
               (setq result (cons x result)))
             (setq --dl-- (cdr --dl--)))
           result)",
    );
    assert_eq!(result, "OK (c b a)");
}

#[test]
fn ignore_errors_catches_signal() {
    crate::test_utils::init_test_tracing();
    let result = bootstrap_eval_one("(ignore-errors (/ 1 0) 42)");
    assert_eq!(result, "OK nil"); // error caught, returns nil
}

#[test]
fn math_functions() {
    crate::test_utils::init_test_tracing();
    assert_eq!(eval_one("(expt 2 10)"), "OK 1024");
    assert_eq!(eval_one("(sqrt 4.0)"), "OK 2.0");
}

#[test]
fn hook_system() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(defvar my-hook nil)
         (defun hook-fn () 42)
         (add-hook 'my-hook 'hook-fn)
         (list (run-hooks 'my-hook)
               my-hook
               (subrp (symbol-function 'add-hook))
               (subrp (symbol-function 'remove-hook))
               (subrp (symbol-function 'run-mode-hooks)))",
    );
    assert_eq!(results[3], "OK (nil (hook-fn) nil nil nil)");
}

#[test]
fn hook_system_runtime_value_shapes() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(setq hook-count 0)
         (defalias 'hook-inc #'(lambda () (setq hook-count (1+ hook-count))))
         (setq hook-probe-hook 'hook-inc)
         (condition-case err (run-hooks 'hook-probe-hook) (error err))
         hook-count
         (setq hook-count 0)
         (setq hook-probe-hook (cons 'hook-inc 1))
         (condition-case err (run-hooks 'hook-probe-hook) (error err))
         hook-count
         (setq hook-probe-hook t)
         (condition-case err (run-hooks 'hook-probe-hook) (error err))
         (setq hook-probe-hook 42)
         (condition-case err (run-hooks 'hook-probe-hook) (error err))
         (setq hook-probe-hook '(t hook-inc))
         (setq hook-count 0)
         (condition-case err (run-hooks 'hook-probe-hook) (error err))
         hook-count",
    );
    assert_eq!(results[3], "OK nil");
    assert_eq!(results[4], "OK 1");
    assert_eq!(results[7], "OK nil");
    assert_eq!(results[8], "OK 1");
    assert_eq!(results[10], "OK (void-function t)");
    assert_eq!(results[12], "OK (invalid-function 42)");
    assert_eq!(results[15], "OK nil");
    assert_eq!(results[16], "OK 2");
}

#[test]
fn safe_run_hook_removes_failing_local_hook_and_continues_to_global_hook() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let buffer = ev.buffers.create_buffer("*safe-hook*");
    ev.buffers.set_current(buffer);

    ev.eval_str(
        r#"(progn
             (setq safe-hook-log nil)
             (defalias 'safe-hook-bad
               #'(lambda ()
                   (setq safe-hook-log (cons 'bad safe-hook-log))
                   (error "boom")))
             (defalias 'safe-hook-good
               #'(lambda ()
                   (setq safe-hook-log (cons 'good safe-hook-log))))
             (setq safe-local-hook '(safe-hook-good))
             (make-local-variable 'safe-local-hook)
             (setq safe-local-hook '(safe-hook-bad t)))"#,
    )
    .expect("safe hook test setup");

    crate::emacs_core::hook_runtime::safe_run_named_hook(
        &mut ev,
        crate::emacs_core::intern::intern("safe-local-hook"),
        &[],
    )
    .expect("safe hook should swallow ordinary hook errors");

    let result = ev
        .eval_str("(list safe-hook-log safe-local-hook (default-value 'safe-local-hook))")
        .expect("inspect safe hook result");
    assert_eq!(format!("{}", result), "((good bad) (t) (safe-hook-good))");
}

#[test]
fn run_hook_with_args_runtime_value_shapes() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(setq hook-log nil)
         (defalias 'hook-log-fn #'(lambda (&rest args) (setq hook-log (cons args hook-log))))
         (setq hook-probe-hook 'hook-log-fn)
         (condition-case err (run-hook-with-args 'hook-probe-hook 1 2) (error err))
         hook-log
         (setq hook-log nil)
         (setq hook-probe-hook (cons 'hook-log-fn 1))
         (condition-case err (run-hook-with-args 'hook-probe-hook 3) (error err))
         hook-log
         (setq hook-probe-hook t)
         (condition-case err (run-hook-with-args 'hook-probe-hook 4) (error err))
         (setq hook-probe-hook 42)
         (condition-case err (run-hook-with-args 'hook-probe-hook 5) (error err))
         (setq hook-log nil)
         (setq hook-probe-hook '(t hook-log-fn))
         (condition-case err (run-hook-with-args 'hook-probe-hook 6) (error err))
         hook-log",
    );
    assert_eq!(results[3], "OK nil");
    assert_eq!(results[4], "OK ((1 2))");
    assert_eq!(results[7], "OK nil");
    assert_eq!(results[8], "OK ((3))");
    assert_eq!(results[10], "OK (void-function t)");
    assert_eq!(results[12], "OK (invalid-function 42)");
    assert_eq!(results[15], "OK nil");
    assert_eq!(results[16], "OK ((6) (6))");
}

#[test]
fn run_hook_with_args_roots_callbacks_and_args_across_exact_gc() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.tagged_heap.set_gc_threshold(1);
    let result = format_eval_result(&ev.eval_str(
        r#"
(progn
  (setq hook-root-a nil)
  (setq hook-root-b nil)
  (setq hook-probe-hook
        (list
         (lambda (arg)
           (garbage-collect)
           (setq hook-root-a arg))
         (lambda (arg)
           (garbage-collect)
           (setq hook-root-b arg))))
  (let ((payload (cons 'x 'y)))
    (run-hook-with-args 'hook-probe-hook payload)
    (list hook-root-a hook-root-b payload)))
"#,
    ));
    assert_eq!(result, "OK ((x . y) (x . y) (x . y))");
    assert!(ev.gc_count > 0, "hook callback GC should run");
}

#[test]
fn run_hook_with_args_accepts_uninterned_symbol_after_same_eval_let_setup() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            r#"(progn
                 (setq test-hook nil)
                 (let ((fun (make-symbol "vm-hook-uninterned")))
                   (fset fun (lambda (x) (setq test-hook-result x)))
                   (setq test-hook (list fun)))
                 (run-hook-with-args 'test-hook 42)
                 test-hook-result)"#
        ),
        "OK 42"
    );
}

#[test]
fn run_hook_with_args_accepts_uninterned_symbol_after_same_eval_lexical_let_setup() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    let result = format_eval_result(&ev.eval_str(
        r#"(progn
             (setq test-hook nil)
             (let ((fun (make-symbol "vm-hook-uninterned-lex")))
               (fset fun (lambda (x) (setq test-hook-result x)))
               (setq test-hook (list fun)))
             (run-hook-with-args 'test-hook 42)
             test-hook-result)"#,
    ));
    assert_eq!(result, "OK 42");
}

#[test]
fn run_hook_wrapped_stops_on_first_non_nil_wrapper_result() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(let ((seen nil))
           (defalias 'hook-wrap-a #'(lambda () 'a))
           (defalias 'hook-wrap-b #'(lambda () 'b))
           (defalias 'hook-wrap-wrapper
             #'(lambda (fn)
                 (setq seen (cons fn seen))
                 (if (eq fn 'hook-wrap-a) 'stop nil)))
           (setq hook-wrap-probe '(hook-wrap-a hook-wrap-b))
           (list (run-hook-wrapped 'hook-wrap-probe 'hook-wrap-wrapper)
                 seen))",
    );
    assert_eq!(result, "OK (stop (hook-wrap-a))");
}

#[test]
fn get_buffer_create_runs_buffer_list_update_hook_when_enabled() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(progn
           (setq hook-log nil)
           (setq buffer-list-update-hook
                 (list (lambda ()
                         (setq hook-log (cons 'ran hook-log)))))
           (get-buffer-create \"gbc-hook\")
           hook-log)",
    );
    assert_eq!(result, "OK (ran)");
}

#[test]
fn get_buffer_create_inhibit_buffer_hooks_suppresses_buffer_and_kill_hooks() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(progn
           (setq hook-log nil)
           (setq buffer-list-update-hook
                 (list (lambda ()
                         (setq hook-log (cons 'buffer-list hook-log)))))
           (let ((buf (get-buffer-create \"gbc-inhibit\" t)))
             (save-current-buffer
               (set-buffer buf)
               (setq kill-buffer-query-functions
                     (list (lambda ()
                             (setq hook-log (cons 'query hook-log))
                             t)))
               (setq kill-buffer-hook
                     (list (lambda ()
                             (setq hook-log (cons 'kill hook-log))))))
             (kill-buffer buf)
             hook-log))",
    );
    assert_eq!(result, "OK nil");
}

#[test]
fn kill_buffer_runs_query_functions_and_hook_in_target_buffer_context() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(progn
           (setq hook-log nil)
           (let ((buf (get-buffer-create \"kill-hook\"))
                 (other (get-buffer-create \"kill-other\")))
             (set-buffer buf)
             (setq kill-buffer-query-functions
                   (list (lambda ()
                           (setq hook-log
                                 (cons (list 'query (buffer-name)) hook-log))
                           t)))
             (setq kill-buffer-hook
                   (list (lambda ()
                           (setq hook-log
                                 (cons (list 'hook (buffer-name)) hook-log)))))
             (set-buffer other)
             (list (kill-buffer buf)
                   (get-buffer \"kill-hook\")
                   (nreverse hook-log)
                   (buffer-name))))",
    );
    assert_eq!(
        result,
        "OK (t nil ((query \"kill-hook\") (hook \"kill-hook\")) \"kill-other\")"
    );
}

#[test]
fn run_window_scroll_functions_uses_scrolled_window_buffer_context() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(progn
           (setq hook-log nil)
           (let* ((buf1 (get-buffer-create \"scroll-a\"))
                  (buf2 (get-buffer-create \"scroll-b\")))
             (set-buffer buf1)
             (set-window-buffer (selected-window) buf1)
             (let ((w2 (split-window-internal (selected-window) nil nil nil)))
               (set-window-buffer w2 buf2)
               (set-buffer buf2)
               (setq window-scroll-functions
                     (list (lambda (_w _start)
                             (setq hook-log (buffer-name)))))
               (set-buffer buf1)
               (run-window-scroll-functions w2)
               (list hook-log (buffer-name)))))",
    );
    assert_eq!(result, "OK (\"scroll-b\" \"scroll-a\")");
}

#[test]
fn point_motion_hooks_follow_gnu_interval_boundary_order() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(progn
           (erase-buffer)
           (insert \"abcd\")
           (setq hook-log nil)
           (setq inhibit-point-motion-hooks nil)
           (defalias 'pm-leave-before
             #'(lambda (old new)
                 (setq hook-log (append hook-log (list (list 'leave-before old new))))))
           (defalias 'pm-leave-after
             #'(lambda (old new)
                 (setq hook-log (append hook-log (list (list 'leave-after old new))))))
           (defalias 'pm-enter-before
             #'(lambda (old new)
                 (setq hook-log (append hook-log (list (list 'enter-before old new))))))
           (defalias 'pm-enter-after
             #'(lambda (old new)
                 (setq hook-log (append hook-log (list (list 'enter-after old new))))))
           (put-text-property 1 2 'point-left 'pm-leave-before)
           (put-text-property 2 3 'point-left 'pm-leave-after)
           (put-text-property 3 4 'point-entered 'pm-enter-before)
           (put-text-property 4 5 'point-entered 'pm-enter-after)
           (goto-char 2)
           (goto-char 4)
           hook-log)",
    );
    assert_eq!(
        result,
        "OK ((leave-before 2 4) (leave-after 2 4) (enter-before 2 4) (enter-after 2 4))"
    );
}

#[test]
fn run_window_configuration_change_hook_uses_window_buffer_context() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.eval_str(
        r#"(progn
           (setq hook-log nil)
           (defalias 'wcch-log-current-buffer
             #'(lambda ()
                 (setq hook-log
                       (cons (intern (buffer-name)) hook-log))))
           (defalias 'wcch-log-global-buffer
             #'(lambda ()
                 (setq hook-log
                       (cons (intern (concat "global:" (buffer-name))) hook-log)))))"#,
    )
    .expect("hook setup");

    let buf1 = ev.buffers.create_buffer("wcch-a");
    let buf2 = ev.buffers.create_buffer("wcch-b");
    ev.switch_current_buffer(buf1).expect("switch to buf1");

    let selected_window = crate::emacs_core::window_cmds::builtin_selected_window(&mut ev, vec![])
        .expect("selected window");
    crate::emacs_core::window_cmds::builtin_set_window_buffer(
        &mut ev,
        vec![selected_window, Value::make_buffer(buf1)],
    )
    .expect("selected window buffer");
    let split_window = ev
        .eval_str("(split-window-internal (selected-window) nil nil nil)")
        .expect("split window");
    crate::emacs_core::window_cmds::builtin_set_window_buffer(
        &mut ev,
        vec![split_window, Value::make_buffer(buf2)],
    )
    .expect("split window buffer");

    ev.buffers
        .set_buffer_local_property(
            buf1,
            "window-configuration-change-hook",
            Value::list(vec![Value::symbol("wcch-log-current-buffer")]),
        )
        .expect("buf1 local hook");
    ev.buffers
        .set_buffer_local_property(
            buf2,
            "window-configuration-change-hook",
            Value::list(vec![Value::symbol("wcch-log-current-buffer")]),
        )
        .expect("buf2 local hook");
    crate::emacs_core::custom::builtin_set_default(
        &mut ev,
        vec![
            Value::symbol("window-configuration-change-hook"),
            Value::list(vec![Value::symbol("wcch-log-global-buffer")]),
        ],
    )
    .expect("default hook");
    assert!(
        ev.buffers
            .get(buf1)
            .and_then(|buffer| buffer.buffer_local_value("window-configuration-change-hook"))
            .is_some()
    );
    assert!(
        ev.buffers
            .get(buf2)
            .and_then(|buffer| buffer.buffer_local_value("window-configuration-change-hook"))
            .is_some()
    );
    assert_eq!(
        ev.frames
            .selected_frame()
            .expect("selected frame")
            .window_list()
            .len(),
        2
    );

    ev.switch_current_buffer(buf1).expect("restore buf1");
    super::builtins::builtin_run_window_configuration_change_hook(&mut ev, vec![])
        .expect("run window-configuration-change-hook");

    let hook_log = ev.eval_symbol("hook-log").expect("hook log");
    let items = list_to_vec(&hook_log).expect("hook log list");
    let names: Vec<String> = items
        .iter()
        .map(|value| value.as_symbol_name().expect("symbol").to_string())
        .collect();
    assert!(names.iter().any(|name| name == "wcch-a"), "names={names:?}");
    assert!(names.iter().any(|name| name == "wcch-b"), "names={names:?}");
    assert!(
        names.iter().any(|name| name == "global:wcch-a"),
        "names={names:?}"
    );
    assert_eq!(
        ev.buffers
            .current_buffer()
            .expect("current buffer")
            .name_value(),
        Value::string("wcch-a")
    );
}

#[test]
fn redisplay_runs_window_change_functions_with_selected_frame_context() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(progn
           (setq hook-log nil)
           (let* ((buf1 (get-buffer-create \"wcf-a\"))
                  (buf2 (get-buffer-create \"wcf-b\")))
             (set-window-buffer (selected-window) buf1)
             (let ((w2 (split-window-internal (selected-window) nil nil nil)))
               (set-window-buffer w2 buf2)
               (setq window-size-change-functions
                     (list (lambda (frame)
                             (setq hook-log
                                   (cons (list 'size (eq frame (selected-frame))
                                               (buffer-name))
                                         hook-log)))))
               (setq window-selection-change-functions
                     (list (lambda (frame)
                             (setq hook-log
                                   (cons (list 'selection (eq frame (selected-frame))
                                               (buffer-name))
                                         hook-log)))))
               (setq window-state-change-functions
                     (list (lambda (frame)
                             (setq hook-log
                                   (cons (list 'state (eq frame (selected-frame))
                                               (buffer-name))
                                         hook-log)))))
               (setq window-state-change-hook
                     (list (lambda ()
                             (setq hook-log (cons 'state-hook hook-log)))))
               (select-window w2)
               (redisplay)
               (nreverse hook-log))))",
    );
    assert_eq!(
        result,
        "OK ((size t \"wcf-b\") (selection t \"wcf-b\") (state t \"wcf-b\") state-hook)"
    );
}

#[test]
fn set_frame_window_state_change_forces_state_hooks_on_redisplay() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(progn
           (setq hook-log nil)
           (setq window-state-change-functions
                 (list (lambda (_frame)
                         (setq hook-log (cons 'state hook-log)))))
           (setq window-state-change-hook
                 (list (lambda ()
                         (setq hook-log (cons 'state-hook hook-log)))))
           (set-frame-window-state-change nil t)
           (redisplay)
           (nreverse hook-log))",
    );
    assert_eq!(result, "OK (state state-hook)");
}

#[test]
fn delete_frame_runs_before_and_after_delete_hooks() {
    crate::test_utils::init_test_tracing();
    let result = eval_one_with_frame(
        "(progn
           (setq hook-log nil)
           (let ((f2 (make-frame)))
             (setq delete-frame-functions
                   (list (lambda (frame)
                           (setq hook-log
                                 (cons (list 'before (frame-live-p frame)) hook-log)))))
             (setq after-delete-frame-functions
                   (list (lambda (frame)
                           (setq hook-log
                                 (cons (list 'after (frame-live-p frame)) hook-log)))))
             (delete-frame f2)
             (nreverse hook-log)))",
    );
    assert_eq!(result, "OK ((before t) (after nil))");
}

#[test]
fn first_change_and_before_change_hooks_run_with_inhibit_bound() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(progn
           (setq hook-log nil)
           (setq first-change-hook
                 (list (lambda ()
                         (setq hook-log
                               (cons (list 'first inhibit-modification-hooks) hook-log)))))
           (setq before-change-functions
                 (list (lambda (_beg _end)
                         (setq hook-log
                               (cons (list 'before inhibit-modification-hooks) hook-log)))))
           (insert \"x\")
           (nreverse hook-log))",
    );
    assert_eq!(result, "OK ((first t) (before t))");
}

#[test]
fn inhibit_modification_hooks_is_bound_to_nil_by_default() {
    crate::test_utils::init_test_tracing();
    let result = eval_one("(list (boundp 'inhibit-modification-hooks) inhibit-modification-hooks)");
    assert_eq!(result, "OK (t nil)");
}

#[test]
fn after_change_functions_receive_character_old_len() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(progn
           (erase-buffer)
           (insert \"é\")
           (setq hook-log nil)
           (setq after-change-functions
                 (list (lambda (_beg _end old-len)
                         (setq hook-log (list old-len inhibit-modification-hooks)))))
           (delete-region 1 2)
           hook-log)",
    );
    assert_eq!(result, "OK (1 t)");
}

#[test]
fn before_change_functions_reset_to_nil_on_error() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(progn
           (setq before-change-functions
                 (list (lambda (_beg _end) (error \"boom\"))))
           (condition-case _ (insert \"x\") (error nil))
           before-change-functions)",
    );
    assert_eq!(result, "OK nil");
}

#[test]
fn symbol_operations() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(defvar x 42)
         (boundp 'x)
         (symbol-value 'x)
         (put 'x 'doc \"A variable\")
         (get 'x 'doc)",
    );
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK 42");
    assert_eq!(results[4], r#"OK "A variable""#);
}

// -- Buffer operations -------------------------------------------------

#[test]
fn buffer_create_and_switch() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(get-buffer-create \"test-buf\")
         (set-buffer \"test-buf\")
         (buffer-name)
         (bufferp (current-buffer))",
    );
    assert!(results[0].starts_with("OK #<buffer"));
    assert!(results[1].starts_with("OK #<buffer"));
    assert_eq!(results[2], r#"OK "test-buf""#);
    assert_eq!(results[3], "OK t");
}

#[test]
fn buffer_insert_and_point() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(get-buffer-create \"ed\")
         (set-buffer \"ed\")
         (insert \"hello\")
         (point)
         (goto-char 1)
         (point)
         (buffer-string)
         (point-min)
         (point-max)",
    );
    assert_eq!(results[3], "OK 6"); // after inserting "hello", point is 6 (1-based)
    assert_eq!(results[5], "OK 1"); // after goto-char 1
    assert_eq!(results[6], r#"OK "hello""#);
    assert_eq!(results[7], "OK 1"); // point-min
    assert_eq!(results[8], "OK 6"); // point-max
}

#[test]
fn buffer_delete_region() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(get-buffer-create \"del\")
         (set-buffer \"del\")
         (insert \"abcdef\")
         (delete-region 2 5)
         (buffer-string)",
    );
    assert_eq!(results[4], r#"OK "aef""#);
}

#[test]
fn buffer_delete_and_extract_region_accepts_live_markers_after_insertions() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(with-temp-buffer
           (insert \"abcdef\")
           (let ((start (copy-marker 2))
                 (end (copy-marker 5 t)))
             (goto-char 1)
             (insert \"X\")
             (list (delete-and-extract-region start end)
                   (buffer-string))))",
    );
    assert_eq!(results[0], r#"OK ("bcd" "Xaef")"#);
}

#[test]
fn buffer_delete_and_extract_region_preserves_unibyte_raw_bytes() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(with-temp-buffer
           (set-buffer-multibyte nil)
           (insert-byte 255 1)
           (let ((s (delete-and-extract-region 1 2)))
             (list (multibyte-string-p s)
                   (string-bytes s)
                   (aref s 0)
                   (buffer-size))))",
    );
    assert_eq!(results[0], "OK (nil 1 255 0)");
}

#[test]
fn buffer_erase() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(get-buffer-create \"era\")
        (set-buffer \"era\")
         (insert \"stuff\")
         (erase-buffer)
         (buffer-string)
         (buffer-size)",
    );
    assert_eq!(results[4], r#"OK """#);
    assert_eq!(results[5], "OK 0");
}

#[test]
fn buffer_mutation_read_only_shape_matches_gnu() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(list
           (with-temp-buffer
             (insert \"abc\")
             (setq buffer-read-only t)
             (condition-case err
                 (delete-region 1 2)
               (error (list (car err) (bufferp (car (cdr err)))))))
           (with-temp-buffer
             (insert \"abc\")
             (setq buffer-read-only t)
             (condition-case err
                 (delete-and-extract-region 1 2)
               (error (list (car err) (bufferp (car (cdr err)))))))
           (with-temp-buffer
             (insert \"abc\")
             (setq buffer-read-only t)
             (condition-case err
                 (erase-buffer)
               (error (list (car err) (bufferp (car (cdr err))))))))",
    );
    assert_eq!(
        results[0],
        "OK ((buffer-read-only t) (buffer-read-only t) (buffer-read-only t))"
    );
}

#[test]
fn buffer_mutation_read_only_noop_cases_match_gnu() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(list
           (with-temp-buffer
             (setq buffer-read-only t)
             (delete-region 1 1))
           (with-temp-buffer
             (setq buffer-read-only t)
             (delete-and-extract-region 1 1))
           (with-temp-buffer
             (narrow-to-region 1 1)
             (setq buffer-read-only t)
             (erase-buffer)
             (list (point-min) (point-max) (buffer-string))))",
    );
    assert_eq!(results[0], r#"OK (nil "" (1 1 ""))"#);
}

#[test]
fn match_string_preserves_unibyte_raw_bytes_for_buffer_searches() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(with-temp-buffer
           (set-buffer-multibyte nil)
           (insert-byte 255 1)
           (goto-char 1)
           (re-search-forward \".\")
           (let ((s (match-string 0)))
             (list (multibyte-string-p s)
                   (string-bytes s)
                   (aref s 0))))",
    );
    assert_eq!(results[0], "OK (nil 1 255)");
}

#[test]
fn buffer_narrowing() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(get-buffer-create \"nar\")
         (set-buffer \"nar\")
         (insert \"hello world\")
         (narrow-to-region 7 12)
         (buffer-string)
         (widen)
         (buffer-string)",
    );
    assert_eq!(results[4], r#"OK "world""#);
    assert_eq!(results[6], r#"OK "hello world""#);
}

#[test]
fn buffer_narrowing_accepts_live_marker_bounds_after_insertions() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(with-temp-buffer
           (insert \"abcdef\")
           (let ((start (copy-marker 2))
                 (end (copy-marker 5 t)))
             (goto-char 1)
             (insert \"X\")
             (narrow-to-region start end)
             (list (point-min) (point-max) (buffer-string))))",
    );
    assert_eq!(results[0], r#"OK (3 6 "bcd")"#);
}

#[test]
fn buffer_modified_p() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(get-buffer-create \"mod\")
         (set-buffer \"mod\")
         (buffer-modified-p)
         (insert \"x\")
         (buffer-modified-p)
         (set-buffer-modified-p nil)
         (buffer-modified-p)",
    );
    assert_eq!(results[2], "OK nil");
    assert_eq!(results[4], "OK t");
    assert_eq!(results[6], "OK nil");
}

#[test]
fn buffer_mark() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(get-buffer-create \"mk\")
         (set-buffer \"mk\")
         (insert \"hello\")
         (set-mark 3)
         (mark)",
    );
    assert_eq!(results[4], "OK 3");
}

#[test]
fn buffer_with_current_buffer() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(get-buffer-create \"a\")
         (get-buffer-create \"b\")
         (set-buffer \"a\")
         (insert \"in-a\")
         (save-current-buffer
           (set-buffer \"b\")
           (insert \"in-b\")
           (buffer-string))
         (buffer-name)
         (buffer-string)",
    );
    // save-current-buffer+set-buffer should switch to b, insert, get string, then restore a
    assert_eq!(results[4], r#"OK "in-b""#);
    assert_eq!(results[5], r#"OK "a""#); // current buffer restored
    assert_eq!(results[6], r#"OK "in-a""#); // a's content unchanged
}

#[test]
fn buffer_save_excursion() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(get-buffer-create \"se\")
         (set-buffer \"se\")
         (insert \"abcdef\")
         (goto-char 3)
         (save-excursion
           (goto-char 1)
           (insert \"X\"))
         (point)",
    );
    // save-excursion restores point to the marker, which shifted from 3 to 4
    // because "X" was inserted before it at position 1.
    assert_eq!(results[5], "OK 4");
}

#[test]
fn buffer_save_excursion_marker_survives_exact_gc() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(get-buffer-create \"se-gc\")
         (set-buffer \"se-gc\")
         (erase-buffer)
         (insert \"abcdef\")
         (goto-char (point-max))
         (save-excursion
           (garbage-collect)
           (goto-char 3)
           (insert \"XXX\"))
         (list (point) (buffer-string))",
    );
    assert_eq!(results[6], "OK (10 \"abXXXcdef\")");
}

#[test]
fn buffer_save_excursion_tracks_marker_through_edits() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(with-temp-buffer
           (insert \"0123456789\")
           (goto-char 6)
           (let ((before-point (point)))
             (save-excursion
               (goto-char 3)
               (insert \"XXX\")
               (goto-char 12)
               (delete-char 2))
             (list before-point (point) (buffer-string))))",
    );
    assert_eq!(results[0], "OK (6 9 \"01XXX234567\")");
}

#[test]
fn insert_before_markers_advances_before_markers_at_point() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(with-temp-buffer
           (insert \"ab\")
           (goto-char 1)
           (let ((m (copy-marker (point))))
             (insert-before-markers \"X\")
             (list (buffer-string) (marker-position m))))",
    );
    assert_eq!(results[0], r#"OK ("Xab" 2)"#);
}

#[test]
fn insert_read_only_shape_and_noop_cases_match_gnu() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(list
           (with-temp-buffer
             (setq buffer-read-only t)
             (condition-case err
                 (insert \"x\")
               (error (list (car err) (bufferp (car (cdr err)))))))
           (with-temp-buffer
             (setq buffer-read-only t)
             (condition-case err
                 (insert-char ?x 1)
               (error (list (car err) (bufferp (car (cdr err)))))))
           (with-temp-buffer
             (setq buffer-read-only t)
             (condition-case err
                 (insert-and-inherit \"x\")
               (error (list (car err) (bufferp (car (cdr err)))))))
           (with-temp-buffer
             (setq buffer-read-only t)
             (condition-case err
                 (insert-before-markers-and-inherit \"x\")
               (error (list (car err) (bufferp (car (cdr err)))))))
           (with-temp-buffer
             (setq buffer-read-only t)
             (condition-case err
                 (insert-byte 120 1)
               (error (list (car err) (bufferp (car (cdr err)))))))
           (with-temp-buffer
             (setq buffer-read-only t)
             (list (insert)
                   (insert \"\")
                   (insert-char ?x 0)
                   (insert-byte 120 0)
                   (insert-and-inherit)
                   (insert-and-inherit \"\")
                   (insert-before-markers-and-inherit)
                   (insert-before-markers-and-inherit \"\")
                   (buffer-string))))",
    );
    assert_eq!(
        results[0],
        r#"OK ((buffer-read-only t) (buffer-read-only t) (buffer-read-only t) (buffer-read-only t) (buffer-read-only t) (nil nil nil nil nil nil nil nil ""))"#
    );
}

#[test]
fn lexical_inhibit_read_only_binding_overrides_buffer_read_only() {
    crate::test_utils::init_test_tracing();
    let mut ev = crate::test_utils::runtime_startup_context();
    ev.set_lexical_binding(true);
    let result = ev.eval_str(
        "(with-temp-buffer
           (setq buffer-read-only t)
           (let ((inhibit-read-only t))
             (insert \"ok\")
             (buffer-string)))",
    );
    assert_eq!(format_eval_result(&result), r#"OK "ok""#);
}

#[test]
fn bootstrap_display_warning_does_not_signal_buffer_read_only() {
    crate::test_utils::init_test_tracing();
    let result = bootstrap_eval_one(
        "(condition-case err
             (progn
               (display-warning 'emacs \"hello from neomacs startup\")
               'ok)
           (error (list 'error (car err))))",
    );
    assert_eq!(result, "OK ok");
}

#[test]
fn insert_char_nil_count_defaults_to_one_with_inherit() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(with-temp-buffer
           (insert \"ab\")
           (put-text-property 2 3 'face 'bold)
           (insert-char ?X nil t)
           (list (buffer-substring-no-properties (point-min) (point-max))
                 (get-text-property 3 'face)))",
    );
    assert_eq!(results[0], r#"OK ("abX" bold)"#);
}

#[test]
fn insert_inherit_variants_match_gnu_property_and_marker_semantics() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(list
           (with-temp-buffer
             (insert \"a\")
             (put-text-property 1 2 'face 'bold)
             (insert-and-inherit (propertize \"X\" 'face 'italic 'mouse-face 'highlight))
             (list (buffer-substring-no-properties (point-min) (point-max))
                   (get-text-property 2 'face)
                   (get-text-property 2 'mouse-face)))
           (with-temp-buffer
             (insert \"ab\")
             (put-text-property 1 2 'face 'bold)
             (goto-char 2)
             (let ((m (copy-marker (point))))
               (insert-before-markers-and-inherit
                (propertize \"X\" 'mouse-face 'highlight))
               (list (buffer-substring-no-properties (point-min) (point-max))
                     (marker-position m)
                     (get-text-property 2 'face)
                     (get-text-property 2 'mouse-face)))))",
    );
    assert_eq!(
        results[0],
        r#"OK (("aX" bold highlight) ("aXb" 3 bold highlight))"#
    );
}

#[test]
fn insert_buffer_substring_preserves_source_text_properties() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            r#"(let ((src (get-buffer-create "*eval-sub-src*"))
                     (dst (get-buffer-create "*eval-sub-dst*")))
                 (save-current-buffer (set-buffer src)
                   (erase-buffer)
                   (insert "abcXYZ")
                   (put-text-property 2 5 'face 'bold))
                 (set-buffer dst)
                 (erase-buffer)
                 (insert-buffer-substring src 2 5)
                 (let ((sub (save-current-buffer (set-buffer src)
                              (buffer-substring 2 5)))
                       (copied (buffer-string)))
                   (list sub
                         (get-text-property 1 'face sub)
                         copied
                         (get-text-property 1 'face copied))))"#,
        ),
        r#"OK (#("bcX" 0 3 (face bold)) bold #("bcX" 0 3 (face bold)) bold)"#
    );
}

#[test]
fn compare_buffer_substrings_respects_case_fold_search() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_one(
            r#"(let ((left (get-buffer-create "*eval-cmp-left*"))
                     (right (get-buffer-create "*eval-cmp-right*")))
                 (save-current-buffer (set-buffer left)
                   (erase-buffer)
                   (insert "Abc"))
                 (save-current-buffer (set-buffer right)
                   (erase-buffer)
                   (insert "aBc"))
                 (list
                  (let ((case-fold-search nil))
                    (compare-buffer-substrings left nil nil right nil nil))
                  (let ((case-fold-search t))
                    (compare-buffer-substrings left nil nil right nil nil))
                  (let ((case-fold-search t))
                    (compare-buffer-substrings left 1 2 right 1 3))))"#,
        ),
        "OK (-1 0 -2)"
    );
}

#[test]
fn field_builtins_match_gnu_property_boundary_semantics() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        bootstrap_eval_one(
            r#"(with-temp-buffer
                 (list
                  (progn
                    (insert "abcdefg")
                    (put-text-property 2 5 'field 'left)
                    (put-text-property 5 8 'field 'right)
                    (put-text-property 2 5 'face 'bold)
                    (let ((s (field-string 3)))
                      (list
                       (list (field-beginning 3)
                             (field-end 3)
                             (field-string-no-properties 3))
                       (get-text-property 1 'face s)
                       (list (field-beginning 5)
                             (field-beginning 5 t)
                             (field-end 5)
                             (field-end 5 t))
                       (progn
                         (delete-field 3)
                         (list
                          (buffer-substring-no-properties (point-min) (point-max))
                          (get-text-property 2 'field))))))
                  (progn
                    (erase-buffer)
                    (insert "abcdefg")
                    (put-text-property 2 4 'field 'left)
                    (put-text-property 4 5 'field 'boundary)
                    (put-text-property 5 8 'field 'right)
                    (list (field-beginning 4)
                          (field-beginning 4 t)
                          (field-end 4)
                          (field-end 4 t)
                          (field-beginning 5)
                          (field-beginning 5 t)
                          (field-end 5)
                          (field-end 5 t)))))"#,
        ),
        r#"OK (((2 5 "bcd") bold (2 2 5 8) ("aefg" right)) (2 2 4 8 4 2 5 8))"#
    );
}

#[test]
fn constrain_to_field_matches_gnu_boundary_and_capture_semantics() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        bootstrap_eval_one(
            r#"(with-temp-buffer
                 (list
                  (progn
                    (insert "abcdefg")
                    (put-text-property 2 5 'field 'left)
                    (put-text-property 5 8 'field 'right)
                    (put-text-property 3 4 'capture t)
                    (goto-char 7)
                    (list
                     (constrain-to-field 7 3)
                     (constrain-to-field 7 5)
                     (constrain-to-field 7 5 t)
                     (progn
                       (goto-char 7)
                       (list (constrain-to-field nil 3) (point)))
                     (constrain-to-field 7 3 nil nil 'capture)
                     (constrain-to-field 7 2 nil nil 'capture)))
                  (progn
                    (erase-buffer)
                    (insert "ab\ncd\nef")
                    (put-text-property 1 4 'field 'top)
                    (put-text-property 4 9 'field 'bottom)
                    (list
                     (constrain-to-field 6 2 nil t)
                     (constrain-to-field 6 2 nil nil)
                     (constrain-to-field 6 4 t nil)))))"#,
        ),
        r#"OK ((5 5 7 (5 5) 5 2) (4 4 6))"#
    );
}

#[test]
fn replace_region_contents_preserves_source_properties_and_rejects_self_buffer() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        bootstrap_eval_one(
            r#"(with-temp-buffer
                 (let ((src (get-buffer-create "*rrc-src*"))
                       (s (propertize "CD" 'face 'bold)))
                   (save-current-buffer (set-buffer src)
                     (erase-buffer)
                     (insert "1234")
                     (put-text-property 2 4 'face 'italic))
                   (list
                    (progn
                      (erase-buffer)
                      (insert "abZZef")
                      (replace-region-contents 3 5 s)
                      (list
                       (buffer-substring-no-properties 1 (point-max))
                       (get-text-property 3 'face)))
                    (progn
                      (erase-buffer)
                      (insert "abZZef")
                      (replace-region-contents 3 5 (vector src 2 4))
                      (list
                       (buffer-substring-no-properties 1 (point-max))
                       (get-text-property 3 'face)
                       (get-text-property 4 'face)))
                    (condition-case err
                        (replace-region-contents 3 5 (current-buffer))
                      (error (list (car err) (car (cdr err))))))))"#,
        ),
        r#"OK (("abCDef" bold) ("ab23ef" italic italic) (error "Cannot replace a buffer with itself"))"#
    );
}

#[test]
fn subst_char_in_region_read_only_shape_and_noop_cases_match_gnu() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(list
           (with-temp-buffer
             (insert \"abc\")
             (setq buffer-read-only t)
             (condition-case err
                 (subst-char-in-region 1 2 ?a ?b)
               (error (list (car err) (bufferp (car (cdr err)))))))
           (with-temp-buffer
             (insert \"abc\")
             (setq buffer-read-only t)
             (list (subst-char-in-region 1 1 ?a ?b)
                   (subst-char-in-region 1 4 ?z ?b)
                   (buffer-substring-no-properties (point-min) (point-max)))))",
    );
    assert_eq!(results[0], r#"OK ((buffer-read-only t) (nil nil "abc"))"#);
}

#[test]
fn buffer_undo_list_reflects_recorded_edits() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(with-temp-buffer
           (setq buffer-undo-list nil)
           (insert \"Hello\")
           (let ((after-insert (not (null buffer-undo-list))))
             (undo-boundary)
             (insert \" World\")
             (undo-boundary)
             (delete-region 1 6)
             (undo-boundary)
             (list after-insert
                   (not (null buffer-undo-list))
                   buffer-undo-list)))",
    );
    assert_eq!(
        results[0],
        "OK (t t (nil (\"Hello\" . 1) 12 nil (6 . 12) nil (1 . 6) (t . 0)))"
    );
}

#[test]
fn char_primitives_respect_narrowing() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(with-temp-buffer
           (insert \"Hello, 世界\")
           (narrow-to-region 3 8)
           (goto-char (point-min))
           (list (following-char)
                 (preceding-char)
                 (char-after (point-min))
                 (char-before (point-min))))",
    );
    assert_eq!(results[0], "OK (108 0 108 nil)");
}

#[test]
fn delete_char_respects_narrowing_boundaries() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(with-temp-buffer
           (insert \"abc\")
           (narrow-to-region 1 2)
           (list (progn
                   (goto-char (point-max))
                   (condition-case err
                       (delete-char 1)
                     (error (car err))))
                 (progn
                   (goto-char (point-min))
                   (condition-case err
                       (delete-char -1)
                     (error (car err))))))",
    );
    assert_eq!(results[0], "OK (end-of-buffer beginning-of-buffer)");
}

#[test]
fn navigation_predicates_and_line_positions_respect_narrowing() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(with-temp-buffer
           (insert \"wx\nab\ncd\")
           (narrow-to-region 4 6)
           (goto-char (point-min))
           (list (list (bobp) (eobp) (bolp) (eolp)
                       (line-beginning-position) (line-end-position))
                 (progn
                   (goto-char (point-max))
                   (list (bobp) (eobp) (bolp) (eolp)
                         (line-beginning-position) (line-end-position)))))",
    );
    assert_eq!(results[0], "OK ((t nil t nil 4 6) (nil t nil t 4 6))");
}

#[test]
fn line_position_optional_argument_matches_gnu_current_rules() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(with-temp-buffer
           (insert \"a\nbb\nccc\")
           (goto-char 2)
           (list (line-beginning-position 2)
                 (line-end-position 2)
                 (line-beginning-position 3)
                 (line-end-position 3)))",
    );
    assert_eq!(results[0], "OK (3 5 6 9)");
}

#[test]
fn save_match_data_restores_after_success_and_error() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(set-match-data '(1 2))
         (save-match-data (set-match-data '(3 4)) (match-data))
         (match-data)
         (condition-case err
             (save-match-data
               (set-match-data '(5 6))
               (signal 'error '(\"boom\")))
           (error (car err)))
         (match-data)",
    );
    assert_eq!(results[1], "OK (3 4)");
    assert_eq!(results[2], "OK (1 2)");
    assert_eq!(results[3], "OK error");
    assert_eq!(results[4], "OK (1 2)");
}

#[test]
fn save_mark_and_excursion_restores_mark_and_mark_active() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(save-current-buffer
           (let ((b (get-buffer-create \"smx-eval\")))
             (set-buffer b)
             (erase-buffer)
             (insert \"abcdef\")
             (goto-char 2)
             (set-mark 5)
             (setq mark-active nil)
             (let ((before (list (point) (mark) mark-active)))
               (save-mark-and-excursion
                 (goto-char 4)
                 (set-mark 3)
                 (setq mark-active t))
               (list before (point) (mark) mark-active))))",
    );
    assert_eq!(results[0], "OK ((2 5 nil) 2 5 nil)");
}

#[test]
fn save_window_excursion_restores_selected_window_on_success_and_error() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(let ((w1 (selected-window))
               (w2 (split-window)))
           (prog1
               (list
                (save-window-excursion
                  (select-window w2)
                  (eq (selected-window) w2))
                (eq (selected-window) w1))
             (ignore-errors (delete-window w2))))
         (let ((w1 (selected-window))
               (w2 (split-window)))
           (prog1
               (list
                (condition-case err
                    (save-window-excursion
                      (select-window w2)
                      (error \"boom\"))
                  (error (car err)))
                (eq (selected-window) w1))
             (ignore-errors (delete-window w2))))",
    );
    assert_eq!(results[0], "OK (t t)");
    assert_eq!(results[1], "OK (error t)");
}

#[test]
fn save_window_excursion_restores_window_layout_after_split() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(let ((before (length (window-list))))
           (list
            (let ((wconfig (current-window-configuration)))
              (unwind-protect
                  (progn
                    (split-window-internal (selected-window) nil nil nil)
                    (length (window-list)))
                (set-window-configuration wconfig)))
            (length (window-list))
            before))",
    );
    assert_eq!(results[0], "OK (2 1 1)");
}

#[test]
fn save_window_excursion_with_help_window_restores_original_window_buffer() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        r#"(let* ((orig (generate-new-buffer "*neo-help-orig*"))
                  (help (get-buffer-create "*neo-help-test*")))
             (unwind-protect
                 (progn
                   (switch-to-buffer orig)
                   (with-current-buffer orig
                     (erase-buffer)
                     (insert "alpha\nbeta\n"))
                   (list
                    (buffer-name (current-buffer))
                    (buffer-name (window-buffer (selected-window)))
                    (save-window-excursion
                      (save-excursion
                        (help--window-setup
                         help
                         (lambda ()
                           (with-current-buffer standard-output
                             (insert "help body")))))
                      (list
                       (buffer-name (current-buffer))
                       (buffer-name (window-buffer (selected-window)))))
                    (buffer-name (current-buffer))
                    (buffer-name (window-buffer (selected-window)))))
               (ignore-errors (kill-buffer help))
               (ignore-errors (kill-buffer orig))))"#,
    );
    assert_eq!(
        results[0],
        r#"OK ("*neo-help-orig*" "*neo-help-orig*" ("*neo-help-orig*" "*neo-help-orig*") "*neo-help-orig*" "*neo-help-orig*")"#
    );
}

#[test]
fn save_window_excursion_restores_selected_window_point_and_requests_final_redisplay() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let buffer_id = ev.buffers.create_buffer("*scratch*");
    ev.buffers.set_current(buffer_id);
    ev.buffers
        .get_mut(buffer_id)
        .expect("scratch buffer")
        .insert("0123456789abcdefghijklmnopqrstuvwxyz");
    ev.frames.create_frame("F1", 960, 640, buffer_id);

    let redisplayed_points = Rc::new(RefCell::new(Vec::new()));
    let redisplayed_points_in_cb = Rc::clone(&redisplayed_points);
    ev.redisplay_fn = Some(Box::new(move |ev: &mut Context| {
        let point = crate::emacs_core::window_cmds::builtin_window_point(ev, vec![])
            .expect("window-point during redisplay");
        let Some(point) = point.as_fixnum() else {
            panic!("window-point should produce an integer during redisplay, got {point:?}");
        };
        redisplayed_points_in_cb.borrow_mut().push(point);
    }));

    ev.eval_str(
        r#"(let ((wconfig (current-window-configuration)))
           (unwind-protect
               (progn
                 (set-window-point (selected-window) 10)
                 (redisplay))
             (set-window-configuration wconfig)))"#,
    )
    .expect("save-window-excursion equivalent should evaluate");

    assert_eq!(*redisplayed_points.borrow(), vec![10, 37]);
}

#[test]
fn current_window_configuration_saves_selected_window_live_point() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let buffer_id = ev.buffers.create_buffer("*scratch*");
    ev.buffers.set_current(buffer_id);
    ev.buffers
        .get_mut(buffer_id)
        .expect("scratch buffer")
        .insert("0123456789abcdefghijklmnopqrstuvwxyz");
    ev.frames.create_frame("F1", 960, 640, buffer_id);

    let result = ev
        .eval_str(
            r#"(let* ((w (selected-window))
                (_ (goto-char 10))
                (cfg (current-window-configuration)))
           (goto-char 3)
           (set-window-configuration cfg)
           (list (window-point w) (point)))"#,
        )
        .expect("current-window-configuration round-trip should evaluate");
    assert_eq!(
        result,
        Value::list(vec![Value::fixnum(10), Value::fixnum(10)])
    );
}

#[test]
fn save_selected_window_restores_selected_window_on_success_and_error() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(let ((w1 (selected-window))
               (w2 (split-window)))
           (prog1
               (list
                (save-selected-window
                  (select-window w2)
                  (eq (selected-window) w2))
                (eq (selected-window) w1))
             (ignore-errors (delete-window w2))))
         (let ((w1 (selected-window))
               (w2 (split-window)))
           (prog1
               (list
                (condition-case err
                    (save-selected-window
                      (select-window w2)
                      (error \"boom\"))
                  (error (car err)))
                (eq (selected-window) w1))
             (ignore-errors (delete-window w2))))",
    );
    assert_eq!(results[0], "OK (t t)");
    assert_eq!(results[1], "OK (error t)");
}

#[test]
fn alist_get_comes_from_gnu_subr_runtime() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        r#"(let ((foo '((a . 1) (b . 2))))
             (list
              (alist-get 'a foo)
              (alist-get 'z foo 'missing)
              (progn
                (setf (alist-get 'c foo) 3)
                foo)))"#,
    );
    assert_eq!(results[0], "OK (1 missing ((c . 3) (a . 1) (b . 2)))");
}

#[test]
fn with_local_quit_catches_quit_and_sets_quit_flag() {
    crate::test_utils::init_test_tracing();
    // GNU verified: subr.el's `with-local-quit` macro re-signals the
    // quit via `(eval '(ignore nil))` after setting `quit-flag`, so
    // a top-level evaluation of the form propagates the quit instead
    // of returning nil. Mirror GNU by wrapping the form in a
    // condition-case so we observe both: the propagated quit and the
    // quit-flag handling for the explicit inhibit-quit branch.
    let results = bootstrap_eval_all(
        "(setq quit-flag nil)
         (condition-case nil
             (with-local-quit
               (keyboard-quit)
               'after)
           (quit 'caught-quit))
         (setq quit-flag nil)
         (condition-case err
             (with-local-quit (error \"boom\"))
           (error (car err)))
         quit-flag
         (let ((inhibit-quit t)
               (quit-flag nil))
           (with-local-quit (keyboard-quit))
           (list inhibit-quit quit-flag))",
    );
    assert_eq!(results[1], "OK caught-quit");
    assert_eq!(results[3], "OK error");
    assert_eq!(results[4], "OK nil");
    assert_eq!(results[5], "OK (t t)");
}

#[test]
fn while_processes_quit_flag_without_loop_local_gc() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(condition-case err
             (while (progn (setq quit-flag t) t)
               nil)
           (quit 'quit))
         quit-flag
         (catch 'tag
           (let ((throw-on-input 'tag))
             (while (progn (setq quit-flag 'tag) t)
               nil)
             'missed))",
    );
    assert_eq!(results[0], "OK quit");
    assert_eq!(results[1], "OK nil");
    assert_eq!(results[2], "OK t");
}

#[test]
fn throw_on_input_is_special_and_dynamically_bound() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(special-variable-p 'throw-on-input)
         (let ((throw-on-input 'tag))
           throw-on-input)
         throw-on-input",
    );
    assert_eq!(results[0], "OK t");
    assert_eq!(results[1], "OK tag");
    assert_eq!(results[2], "OK nil");
}

#[test]
fn while_no_input_ignore_events_bootstraps_monitors_changed_like_gnu() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(memq 'monitors-changed while-no-input-ignore-events)
         (special-variable-p 'while-no-input-ignore-events)
         input-pending-p-filter-events",
    );
    assert_eq!(results[0], "OK (monitors-changed)");
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK t");
}

#[test]
fn while_no_input_catches_pending_key_queued_during_body() {
    crate::test_utils::init_test_tracing();

    fn queue_key_for_while_no_input_test(ctx: &mut Context, args: Vec<Value>) -> EvalResult {
        assert!(args.is_empty(), "queue helper should not receive arguments");
        ctx.command_loop.keyboard.pending_input_events.push_back(
            crate::keyboard::InputEvent::KeyPress {
                key: crate::keyboard::KeyEvent::char('k'),
                emacs_frame_id: 0,
            },
        );
        Ok(Value::NIL)
    }

    let mut ev = runtime_startup_context();
    ev.set_variable("noninteractive", Value::NIL);
    ev.defsubr(
        "neo-queue-key-for-while-no-input-test",
        queue_key_for_while_no_input_test,
        0,
        Some(0),
    );

    let result = ev.eval_str(
        "(condition-case err
             (while-no-input
               (neo-queue-key-for-while-no-input-test)
               (eval '(ignore nil) t)
               'missed)
           (error err))",
    );

    assert_eq!(
        crate::emacs_core::error::format_eval_result(&result),
        "OK t"
    );
}

#[test]
fn while_no_input_catches_pending_key_across_load_boundary() {
    crate::test_utils::init_test_tracing();

    fn queue_key_for_while_no_input_test(ctx: &mut Context, args: Vec<Value>) -> EvalResult {
        assert!(args.is_empty(), "queue helper should not receive arguments");
        ctx.command_loop.keyboard.pending_input_events.push_back(
            crate::keyboard::InputEvent::KeyPress {
                key: crate::keyboard::KeyEvent::char('k'),
                emacs_frame_id: 0,
            },
        );
        Ok(Value::NIL)
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let load_path = dir.path().join("while-no-input-load.el");
    std::fs::write(
        &load_path,
        "(neo-queue-key-for-while-no-input-test)\n\
         (eval '(ignore nil) t)\n\
         (setq neo-loaded-after-input t)\n",
    )
    .expect("write load fixture");

    let mut ev = runtime_startup_context();
    ev.set_variable("noninteractive", Value::NIL);
    ev.set_variable(
        "neo-while-no-input-load-file",
        Value::string(load_path.to_string_lossy().into_owned()),
    );
    ev.defsubr(
        "neo-queue-key-for-while-no-input-test",
        queue_key_for_while_no_input_test,
        0,
        Some(0),
    );

    let result = ev.eval_str(
        "(progn
           (setq neo-loaded-after-input nil)
           (list
            (condition-case err
                (while-no-input
                  (load neo-while-no-input-load-file nil t)
                  'missed)
              (error err))
            neo-loaded-after-input))",
    );

    assert_eq!(
        crate::emacs_core::error::format_eval_result(&result),
        "OK (t nil)"
    );
}

#[test]
fn window_and_minibuffer_defvars_are_bound_and_special_like_gnu() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(list (boundp 'minibuffer-scroll-window)
               (special-variable-p 'minibuffer-scroll-window)
               (boundp 'other-window-scroll-buffer)
               (special-variable-p 'other-window-scroll-buffer)
               (boundp 'other-window-scroll-default)
               (special-variable-p 'other-window-scroll-default)
               (boundp 'scroll-minibuffer-conservatively)
               (special-variable-p 'scroll-minibuffer-conservatively)
               scroll-minibuffer-conservatively)",
    );
    assert_eq!(results[0], "OK (t t t t t t t t t)");
}

#[test]
fn input_pending_p_filters_default_ignored_events_like_gnu() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let fid = ev
        .frames
        .create_frame("F1", 960, 640, crate::buffer::BufferId(1));
    let window_id = ev.frames.get(fid).expect("frame").window_list()[0];

    ev.command_loop.keyboard.pending_input_events.push_back(
        crate::keyboard::InputEvent::MonitorsChanged {
            monitors: vec![crate::emacs_core::builtins::NeomacsMonitorInfo {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
                scale: 1.0,
                width_mm: 500,
                height_mm: 300,
                name: Some("DP-1".to_string()),
            }],
        },
    );
    ev.command_loop
        .keyboard
        .pending_input_events
        .push_back(crate::keyboard::InputEvent::SelectWindow { window_id });

    let filtered = crate::emacs_core::reader::builtin_input_pending_p(&mut ev, vec![])
        .expect("default input-pending-p should succeed");
    assert_eq!(filtered, Value::NIL);

    ev.obarray
        .set_symbol_value("input-pending-p-filter-events", Value::NIL);
    let unfiltered = crate::emacs_core::reader::builtin_input_pending_p(&mut ev, vec![])
        .expect("unfiltered input-pending-p should succeed");
    assert_eq!(unfiltered, Value::T);
}

#[test]
fn with_temp_message_accepts_min_arity_and_runs_body() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(with-temp-message nil 42)
         (with-temp-message \"tmp\" 7)
         (condition-case err
             (with-temp-message)
           (error (car err)))",
    );
    assert_eq!(results[0], "OK 42");
    assert_eq!(results[1], "OK 7");
    assert_eq!(results[2], "OK wrong-number-of-arguments");
}

#[test]
fn with_demoted_errors_runtime_semantics() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval_all(
        "(fboundp 'with-demoted-errors)
         (macrop 'with-demoted-errors)
         (with-demoted-errors \"DM %S\" (+ 1 2))
         (condition-case err
             (with-demoted-errors \"DM %S\" (/ 1 0))
           (error (list :error (car err) (cdr err))))
         (condition-case err
             (with-demoted-errors 1 (/ 1 0))
           (error (list :error (car err) (cdr err))))
         (with-demoted-errors \"DM %S\")
         (condition-case err
             (with-demoted-errors)
           (error err))",
    );
    assert_eq!(results[0], "OK t");
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK 3");
    assert_eq!(results[3], "OK nil");
    assert_eq!(results[4], "OK nil");
    assert_eq!(results[5], r#"OK "DM %S""#);
    // After the specbind refactor, with-demoted-errors uses the Elisp
    // macro definition which has (1 . many) arity, not the old (1 . 1).
    assert_eq!(results[6], "OK (wrong-number-of-arguments (1 . many) 0)");
}

#[test]
fn bootstrap_condition_case_unless_debug_calls_debugger_before_handler() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        bootstrap_eval_one(
            "(progn
               (setq neovm-debugger-called nil)
               (let ((debug-on-error t)
                   (debugger (lambda (&rest args)
                               (setq neovm-debugger-called args))))
                 (list (condition-case-unless-debug nil
                           (signal 'error 1)
                         (error 'handled))
                       neovm-debugger-called)))"
        ),
        "OK (handled (error (error . 1)))"
    );
}

#[test]
fn bootstrap_with_demoted_errors_calls_debugger_when_debug_on_error_is_enabled() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        bootstrap_eval_one(
            "(progn
               (setq neovm-debugger-called nil)
               (let ((debug-on-error t)
                   (debugger (lambda (&rest _args)
                               (setq neovm-debugger-called 'debugger))))
                 (list (with-demoted-errors \"DM %S\" (/ 1 0))
                       neovm-debugger-called)))"
        ),
        "OK (nil debugger)"
    );
}

#[test]
fn buffer_char_after_before() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(get-buffer-create \"cb\")
         (set-buffer \"cb\")
         (insert \"abc\")
         (goto-char 2)
         (char-after)
         (char-before)",
    );
    assert_eq!(results[4], "OK 98"); // ?b = 98
    assert_eq!(results[5], "OK 97"); // ?a = 97
}

#[test]
fn buffer_list_and_kill() {
    crate::test_utils::init_test_tracing();
    let results = eval_all(
        "(get-buffer-create \"kill-me\")
         (kill-buffer \"kill-me\")
         (get-buffer \"kill-me\")",
    );
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK nil");
}

#[test]
fn buffer_generate_new_buffer() {
    crate::test_utils::init_test_tracing();
    let results = eval_all_with_subr(
        "(buffer-name (generate-new-buffer \"test\"))
         (buffer-name (generate-new-buffer \"test\"))",
    );
    assert_eq!(results[0], r#"OK "test""#);
    assert_eq!(results[1], r#"OK "test<2>""#);
}

#[test]
fn fillarray_string_writeback_updates_symbol_binding() {
    crate::test_utils::init_test_tracing();
    let result = eval_one("(let ((s (copy-sequence \"abc\"))) (fillarray s ?x) s)");
    assert_eq!(result, r#"OK "xxx""#);
}

#[test]
fn fillarray_alias_string_writeback_updates_symbol_binding() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(progn
            (defalias 'vm-fillarray-alias 'fillarray)
            (let ((s (copy-sequence \"abc\")))
              (vm-fillarray-alias s ?y)
              s))",
    );
    assert_eq!(result, r#"OK "yyy""#);
}

#[test]
fn fillarray_string_writeback_updates_alias_from_prog1_expression() {
    crate::test_utils::init_test_tracing();
    let result = eval_one("(let ((s (copy-sequence \"abc\"))) (fillarray (prog1 s) ?x) s)");
    assert_eq!(result, r#"OK "xxx""#);
}

#[test]
fn fillarray_string_writeback_updates_alias_from_list_car_expression() {
    crate::test_utils::init_test_tracing();
    let result = eval_one("(let ((s (copy-sequence \"abc\"))) (fillarray (car (list s)) ?y) s)");
    assert_eq!(result, r#"OK "yyy""#);
}

#[test]
fn fillarray_string_writeback_updates_vector_alias_element() {
    crate::test_utils::init_test_tracing();
    let result =
        eval_one("(let* ((s (copy-sequence \"abc\")) (v (vector s))) (fillarray s ?x) (aref v 0))");
    assert_eq!(result, r#"OK "xxx""#);
}

#[test]
fn fillarray_string_writeback_updates_cons_alias_element() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(let* ((s (copy-sequence \"abc\")) (cell (cons s nil))) (fillarray s ?y) (car cell))",
    );
    assert_eq!(result, r#"OK "yyy""#);
}

#[test]
fn fillarray_string_writeback_preserves_eq_hash_key_lookup() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(let* ((s (copy-sequence \"abc\")) (ht (make-hash-table :test 'eq)))
           (puthash s 'v ht)
           (fillarray s ?x)
           (gethash s ht))",
    );
    assert_eq!(result, "OK v");
}

#[test]
fn fillarray_string_writeback_preserves_eql_hash_key_lookup() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(let* ((s (copy-sequence \"abc\")) (ht (make-hash-table :test 'eql)))
           (puthash s 'v ht)
           (fillarray s ?y)
           (gethash s ht))",
    );
    assert_eq!(result, "OK v");
}

#[test]
fn fillarray_string_writeback_equal_hash_key_lookup_stays_nil() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(let* ((s (copy-sequence \"abc\")) (ht (make-hash-table :test 'equal)))
           (puthash s 'v ht)
           (fillarray s ?z)
           (gethash s ht))",
    );
    assert_eq!(result, "OK nil");
}

#[test]
fn aset_string_writeback_updates_symbol_binding() {
    crate::test_utils::init_test_tracing();
    let result = eval_one("(let ((s (copy-sequence \"abc\"))) (aset s 1 ?x) s)");
    assert_eq!(result, r#"OK "axc""#);
}

#[test]
fn aset_alias_string_writeback_updates_symbol_binding() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(progn
            (defalias 'vm-aset-alias 'aset)
            (let ((s (copy-sequence \"abc\")))
              (vm-aset-alias s 1 ?y)
              s))",
    );
    assert_eq!(result, r#"OK "ayc""#);
}

#[test]
fn aset_string_writeback_updates_alias_from_prog1_expression() {
    crate::test_utils::init_test_tracing();
    let result = eval_one("(let ((s (copy-sequence \"abc\"))) (aset (prog1 s) 1 ?x) s)");
    assert_eq!(result, r#"OK "axc""#);
}

#[test]
fn aset_string_writeback_updates_alias_from_list_car_expression() {
    crate::test_utils::init_test_tracing();
    let result = eval_one("(let ((s (copy-sequence \"abc\"))) (aset (car (list s)) 1 ?y) s)");
    assert_eq!(result, r#"OK "ayc""#);
}

#[test]
fn aset_string_writeback_updates_vector_alias_element() {
    crate::test_utils::init_test_tracing();
    let result =
        eval_one("(let* ((s (copy-sequence \"abc\")) (v (vector s))) (aset s 1 ?x) (aref v 0))");
    assert_eq!(result, r#"OK "axc""#);
}

#[test]
fn aset_string_writeback_updates_cons_alias_element() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(let* ((s (copy-sequence \"abc\")) (cell (cons s nil))) (aset s 1 ?y) (car cell))",
    );
    assert_eq!(result, r#"OK "ayc""#);
}

#[test]
fn aset_string_writeback_preserves_eq_hash_key_lookup() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(let* ((s (copy-sequence \"abc\")) (ht (make-hash-table :test 'eq)))
           (puthash s 'v ht)
           (aset s 1 ?x)
           (gethash s ht))",
    );
    assert_eq!(result, "OK v");
}

#[test]
fn aset_string_writeback_preserves_eql_hash_key_lookup() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(let* ((s (copy-sequence \"abc\")) (ht (make-hash-table :test 'eql)))
           (puthash s 'v ht)
           (aset s 1 ?y)
           (gethash s ht))",
    );
    assert_eq!(result, "OK v");
}

#[test]
fn aset_string_writeback_equal_hash_key_lookup_stays_nil() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(let* ((s (copy-sequence \"abc\")) (ht (make-hash-table :test 'equal)))
           (puthash s 'v ht)
           (aset s 1 ?z)
           (gethash s ht))",
    );
    assert_eq!(result, "OK nil");
}

// -----------------------------------------------------------------------
// GC integration tests
// -----------------------------------------------------------------------

#[test]
fn gc_collect_retains_reachable() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.eval_str_each("(setq x (cons 1 2))");
    let before = ev.tagged_heap.allocated_count();
    ev.gc_collect();
    let after = ev.tagged_heap.allocated_count();
    // The cons stored in variable `x` must survive.
    assert!(after >= 1, "reachable cons was collected");
    assert!(after <= before, "gc should not increase count");
    // Verify the value is still accessible.
    let results = ev.eval_str_each("(car x)");
    assert_eq!(format_eval_result(&results[0]), "OK 1");
}

#[test]
fn gc_collect_exact_retains_reachable() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();

    ev.eval_str_each("(setq x (cons 11 22))");
    ev.gc_collect_exact();

    let results = ev.eval_str_each("(car x)");
    assert_eq!(format_eval_result(&results[0]), "OK 11");
}

#[test]
fn gc_collect_exact_frees_stack_only_values() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let marker = 0u8;
    ev.tagged_heap.set_stack_bottom(&marker as *const u8);

    ev.gc_collect_exact();
    let baseline = ev.tagged_heap.allocated_count();
    let stack_only = Value::cons(Value::fixnum(31), Value::fixnum(32));
    let keep_visible = [stack_only];
    std::hint::black_box(&keep_visible);
    let after_alloc = ev.tagged_heap.allocated_count();
    assert_eq!(
        after_alloc,
        baseline + 1,
        "stack-only cons should have allocated exactly one object after the baseline collection: baseline={baseline}, after_alloc={after_alloc}"
    );

    ev.gc_collect_exact();

    let after_gc = ev.tagged_heap.allocated_count();
    assert_eq!(
        after_gc, baseline,
        "exact GC must ignore the configured conservative stack scan and free stack-only objects: baseline={baseline}, after_alloc={after_alloc}, after_gc={after_gc}"
    );
}

#[test]
fn gc_collect_exact_inside_extra_root_scope_retains_explicit_slice() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let rooted = Value::cons(Value::fixnum(11), Value::fixnum(22));
    let _unreachable = Value::cons(Value::fixnum(1), Value::fixnum(2));
    let before = ev.tagged_heap.allocated_count();

    let scope = ev.save_specpdl_roots();
    ev.push_specpdl_root(rooted);
    ev.gc_collect_exact();
    ev.restore_specpdl_roots(scope);

    let after = ev.tagged_heap.allocated_count();
    assert_eq!(rooted.cons_car(), Value::fixnum(11));
    assert!(
        after < before,
        "exact collection with explicit roots should free unrelated garbage: before={before}, after={after}"
    );
}

#[test]
fn specpdl_roots_are_traced_across_exact_gc() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();

    let payload = Value::vector(vec![Value::fixnum(29)]);
    let scope = ev.save_specpdl_roots();
    ev.push_specpdl_root(payload);

    ev.gc_collect_exact();

    let rooted = match ev.specpdl.last() {
        Some(SpecBinding::GcRoot { value }) => *value,
        other => panic!("expected specpdl gc root entry, got {other:?}"),
    };
    assert_eq!(
        rooted.as_vector_data().unwrap().as_slice(),
        &[Value::fixnum(29)]
    );

    ev.restore_specpdl_roots(scope);
    assert!(ev.specpdl.is_empty());
}

#[test]
fn eval_str_each_roots_parsed_forms_on_specpdl() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();

    let results = ev.eval_str_each("(setq x (cons 11 22)) (garbage-collect) (car x)");
    assert_eq!(format_eval_result(&results[2]), "OK 11");
}

#[test]
fn prog1_primary_survives_cleanup_garbage_collect() {
    assert_eq!(
        eval_one("(car (prog1 (cons 31 32) (garbage-collect)))"),
        "OK 31"
    );
}

#[test]
fn unwind_protect_primary_survives_cleanup_garbage_collect() {
    assert_eq!(
        eval_one("(car (unwind-protect (cons 41 42) (garbage-collect)))"),
        "OK 41"
    );
}

#[test]
fn let_init_values_survive_gc_stress_until_bindings_own_them() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    ev.gc_stress = true;

    let result = ev.eval_str(
        "(let ((x (cons 51 52))
               (y (cons 61 62)))
           (list (car x) (car y)))",
    );
    assert_eq!(format_eval_result(&result), "OK (51 61)");
}

#[test]
fn specpdl_backtrace_frame_args_survive_exact_gc() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();

    let payload = Value::vector(vec![Value::fixnum(17)]);
    let bt_count = ev.specpdl.len();
    ev.push_backtrace_frame(Value::symbol("runtime-backtrace-active-call"), &[payload]);

    ev.gc_collect_exact();

    // Find the backtrace frame and verify args survived GC.
    let rooted = ev
        .specpdl
        .iter()
        .rev()
        .find_map(|entry| match entry {
            SpecBinding::Backtrace { args, .. } => args.first().copied(),
            _ => None,
        })
        .expect("backtrace frame should remain present");
    assert_eq!(
        rooted.as_vector_data().unwrap().as_slice(),
        &[Value::fixnum(17)]
    );

    ev.unbind_to(bt_count);
}

#[test]
fn specpdl_gc_root_survives_exact_gc() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();

    let payload = Value::vector(vec![Value::fixnum(13)]);
    let bt_count = ev.specpdl.len();
    ev.push_backtrace_frame(Value::symbol("active-call-root"), &[payload]);

    ev.gc_collect_exact();

    let rooted = ev
        .specpdl
        .iter()
        .rev()
        .find_map(|entry| match entry {
            SpecBinding::Backtrace { args, .. } => args.first().copied(),
            _ => None,
        })
        .expect("backtrace frame should remain present");
    assert_eq!(
        rooted.as_vector_data().unwrap().as_slice(),
        &[Value::fixnum(13)]
    );

    ev.unbind_to(bt_count);
}

#[test]
fn specpdl_gc_root_entries_are_traced_across_exact_gc() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();

    let payload = Value::vector(vec![Value::fixnum(17)]);
    let scope = ev.save_specpdl_roots();
    ev.push_specpdl_root(payload);
    ev.gc_collect_exact();

    let rooted = match ev.specpdl.last() {
        Some(SpecBinding::GcRoot { value }) => *value,
        other => panic!("expected specpdl gc root entry, got {other:?}"),
    };
    assert_eq!(
        rooted.as_vector_data().unwrap().as_slice(),
        &[Value::fixnum(17)]
    );
    ev.restore_specpdl_roots(scope);
}

#[test]
fn vm_root_frames_are_traced_across_exact_gc() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();

    let payload = Value::vector(vec![Value::fixnum(37)]);
    ev.push_vm_root_frame();
    ev.push_vm_frame_root(payload);

    ev.gc_collect_exact();

    let rooted = ev
        .vm_root_frames
        .last()
        .expect("vm root frame should remain present")
        .roots[0];
    assert_eq!(
        rooted.as_vector_data().unwrap().as_slice(),
        &[Value::fixnum(37)]
    );

    ev.pop_vm_root_frame();
}

#[test]
fn extra_gc_roots_use_specpdl_when_no_runtime_frame_owns_them() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();

    let payload = Value::vector(vec![Value::fixnum(43)]);

    let scope = ev.save_specpdl_roots();
    ev.push_specpdl_root(payload);
    assert!(matches!(
        ev.specpdl.last(),
        Some(SpecBinding::GcRoot { .. })
    ));
    ev.gc_collect_exact();
    let rooted = match ev.specpdl.last() {
        Some(SpecBinding::GcRoot { value }) => *value,
        other => panic!("expected specpdl gc root entry, got {other:?}"),
    };
    ev.restore_specpdl_roots(scope);

    assert_eq!(
        rooted.as_vector_data().unwrap().as_slice(),
        &[Value::fixnum(43)]
    );
    assert!(ev.specpdl.is_empty());
}

#[test]
fn push_specpdl_root_creates_gc_root_entry_and_restore_removes_it() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();

    let payload = Value::vector(vec![Value::fixnum(44)]);

    let scope = ev.save_specpdl_roots();
    ev.push_specpdl_root(payload);
    assert!(matches!(
        ev.specpdl.last(),
        Some(SpecBinding::GcRoot { .. })
    ));
    ev.gc_collect_exact();
    let rooted = match ev.specpdl.last() {
        Some(SpecBinding::GcRoot { value }) => *value,
        other => panic!("expected specpdl gc root entry, got {other:?}"),
    };
    ev.restore_specpdl_roots(scope);

    assert_eq!(
        rooted.as_vector_data().unwrap().as_slice(),
        &[Value::fixnum(44)]
    );
    assert!(ev.specpdl.is_empty());
}

#[test]
fn lexical_binding_rooting_uses_specpdl() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let payload = Value::vector(vec![Value::fixnum(47)]);
    let sym = intern("specpdl-lexical-binding");

    ev.bind_lexical_value_rooted(sym, payload);

    assert_eq!(
        ev.lexenv_lookup_cached_in(ev.lexenv, sym)
            .expect("lexical binding should exist")
            .as_vector_data()
            .unwrap()
            .as_slice(),
        &[Value::fixnum(47)]
    );
    // bind_lexical_value_rooted uses a temporary specpdl root that is
    // popped after the cons cells are allocated, so specpdl should be empty.
    assert!(
        ev.specpdl.is_empty(),
        "temporary specpdl roots should be released once lexenv owns the binding"
    );
}

#[test]
fn lexical_binding_fallback_uses_specpdl_when_no_frame_is_available() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.gc_stress = true;
    let payload = Value::vector(vec![Value::fixnum(48)]);
    let sym = intern("specpdl-lexical-fallback");

    ev.bind_lexical_value_rooted(sym, payload);

    assert_eq!(
        ev.lexenv_lookup_cached_in(ev.lexenv, sym)
            .expect("lexical binding should exist")
            .as_vector_data()
            .unwrap()
            .as_slice(),
        &[Value::fixnum(48)]
    );
    assert!(
        ev.specpdl.is_empty(),
        "temporary specpdl roots should be released once lexenv owns the binding"
    );
}

#[test]
fn direct_closure_call_uses_specpdl_for_rooting() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    ev.gc_stress = true;

    let callable = ev
        .eval_str(
            "(let ((captured (vector 71)))
               (lambda (x &optional y &rest rest)
                 (list (aref captured 0) x y rest)))",
        )
        .expect("closure should evaluate");

    let specpdl_before = ev.specpdl.len();

    let result = match ev.funcall_general_untraced(
        callable,
        vec![
            Value::fixnum(1),
            Value::fixnum(2),
            Value::fixnum(3),
            Value::fixnum(4),
        ],
    ) {
        Ok(value) => value,
        Err(Flow::Signal(sig)) => panic!(
            "direct closure call should succeed: {} {:?}",
            sig.symbol_name(),
            sig.data
        ),
        Err(other) => panic!("direct closure call should succeed: {other:?}"),
    };

    assert_eq!(
        crate::emacs_core::print::print_value(&result),
        "(71 1 2 (3 4))"
    );
    assert_eq!(
        ev.specpdl.len(),
        specpdl_before,
        "closure call should clean up all specpdl entries"
    );
}

#[test]
fn direct_closure_call_rest_args_preserve_heap_values_under_gc() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.gc_stress = true;

    let callable = ev
        .eval_str("(lambda (&rest rest) (car (cdr (cdr rest))))")
        .expect("lambda should evaluate");

    let result = ev
        .funcall_general_untraced(
            callable,
            vec![Value::fixnum(1), Value::fixnum(2), Value::string("29.1")],
        )
        .expect("rest-arg lambda call should succeed");

    assert_eq!(result, Value::string("29.1"));
}

#[test]
fn direct_context_apply_accepts_uninterned_symbol_function_designators() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let fun = intern_uninterned("vm-apply-uninterned");
    let callable = ev
        .eval_str("(lambda (x) (+ x 1))")
        .expect("lambda should evaluate");
    ev.obarray.set_symbol_function_id(fun, callable);

    let result = ev
        .apply(Value::from_sym_id(fun), vec![Value::fixnum(41)])
        .expect("Context::apply should funcall uninterned symbol");

    assert_eq!(result, Value::fixnum(42));
}

#[test]
fn macro_expansion_scope_uses_specpdl_roots() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    ev.gc_stress = true;
    ev.bind_lexical_value_rooted(intern("macro-scope-a"), Value::fixnum(1));
    ev.bind_lexical_value_rooted(intern("macro-scope-b"), Value::fixnum(2));
    let specpdl_count = ev.specpdl.len();
    let dyn_sym = intern("macro-scope-dyn");
    ev.specbind(dyn_sym, Value::fixnum(9));

    let state = ev.begin_macro_expansion_scope();

    assert!(matches!(
        ev.specpdl.last(),
        Some(SpecBinding::GcRoot { .. })
    ));

    let dynvars = ev
        .obarray
        .symbol_value_id(macroexp_dynvars_symbol())
        .copied()
        .expect("macroexp--dynvars should be bound inside macro expansion scope");
    let printed = crate::emacs_core::print::print_value(&dynvars);
    assert!(printed.contains("macro-scope-dyn"), "{printed}");

    ev.finish_macro_expansion_scope(state);

    assert!(
        ev.specpdl.len() == specpdl_count + 1,
        "macro expansion scope should release only its temporary specpdl roots"
    );
    ev.unbind_to(specpdl_count);
    assert!(ev.specpdl.is_empty());
}

#[test]
fn gc_collect_frees_unreachable() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    // Create orphaned conses that aren't bound to any variable.
    let _ = ev.eval_str("(progn (cons 1 2) (cons 3 4) (cons 5 6) nil)");
    let before = ev.tagged_heap.allocated_count();
    ev.gc_collect();
    let after = ev.tagged_heap.allocated_count();
    // The orphaned conses should have been freed.
    assert!(
        after < before,
        "gc did not free unreachable objects: before={before}, after={after}"
    );
}

#[test]
fn gc_collect_handles_cycles() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    // Create a circular list: (setq x (cons 1 nil)) (setcdr x x)
    let _ = ev.eval_str("(progn (setq x (cons 1 nil)) (setcdr x x) t)");
    // GC should handle cycles without infinite loop.
    ev.gc_collect();
    // x is still reachable.
    let results = ev.eval_str_each("(car x)");
    assert_eq!(format_eval_result(&results[0]), "OK 1");

    // Now remove the root and collect — the cycle should be freed.
    ev.eval_str_each("(setq x nil)");
    let before = ev.tagged_heap.allocated_count();
    ev.gc_collect();
    let after = ev.tagged_heap.allocated_count();
    assert!(
        after < before,
        "cyclic cons not freed: before={before}, after={after}"
    );
}

#[test]
fn gc_safe_point_collects_when_threshold_reached() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.tagged_heap.set_gc_threshold(5);
    // Allocate enough conses to exceed threshold.
    ev.eval_str_each("(progn (cons 1 2) (cons 3 4) (cons 5 6) (cons 7 8) (cons 9 10) nil)");
    assert!(
        ev.gc_count > 0 || ev.gc_pending || ev.tagged_heap.should_collect(),
        "incremental GC should be pending, active, or already finished"
    );
    // With incremental GC, safe point may need multiple calls to finish.
    while ev.gc_count == 0 {
        ev.gc_safe_point();
    }
    assert!(ev.gc_count > 0);
}

#[test]
fn gc_threshold_adapts_after_collection() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    // Create 3 conses that are reachable via variables.
    ev.eval_str_each("(progn (setq a (cons 1 2)) (setq b (cons 3 4)) (setq c (cons 5 6)))");
    ev.gc_collect();
    // GNU uses a byte threshold driven by `gc-cons-threshold` and
    // `gc-cons-percentage`, not a raw object-count heuristic.
    let alive = ev.tagged_heap.allocated_count();
    assert!(alive >= 3);
    let threshold = ev.tagged_heap.gc_threshold();
    assert!(
        threshold >= 800_000,
        "threshold should track GNU's default byte budget, got {threshold}"
    );
}

#[test]
fn gc_runtime_setting_mutation_reloads_threshold_immediately() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();

    ev.eval_str_each(
        "(progn
           (setq gc-cons-percentage nil)
           (setq gc-cons-threshold 1234567))",
    );
    assert_eq!(ev.tagged_heap.gc_threshold(), 1_234_567);

    ev.eval_str_each("(setq gc-cons-threshold 2345678)");
    assert_eq!(ev.tagged_heap.gc_threshold(), 2_345_678);
}

#[test]
fn gc_collect_uses_exact_root_tracing() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();

    ev.eval_str_each("(setq mode-root (cons 7 8))");
    ev.gc_collect();

    let results = ev.eval_str_each("(car mode-root)");
    assert_eq!(format_eval_result(&results[0]), "OK 7");
}

#[test]
fn gc_safe_point_uses_exact_root_tracing() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.tagged_heap.set_gc_threshold(5);

    ev.eval_str_each(
        "(progn
           (setq mode-safe-root (cons 7 8))
           (cons 1 2)
           (cons 3 4)
           (cons 5 6)
           (cons 9 10)
           nil)",
    );

    while ev.gc_count == 0 {
        ev.gc_safe_point();
    }

    let results = ev.eval_str_each("(car mode-safe-root)");
    assert_eq!(format_eval_result(&results[0]), "OK 7");
}

#[test]
fn gc_safe_point_exact_inside_extra_root_scope_retains_explicit_slice() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.tagged_heap.set_gc_threshold(2);
    let rooted = Value::cons(Value::fixnum(21), Value::fixnum(22));
    let _unreachable = Value::cons(Value::fixnum(1), Value::fixnum(2));
    let before = ev.tagged_heap.allocated_count();

    while ev.gc_count == 0 {
        let scope = ev.save_specpdl_roots();
        ev.push_specpdl_root(rooted);
        ev.gc_safe_point_exact();
        ev.restore_specpdl_roots(scope);
    }

    let after = ev.tagged_heap.allocated_count();
    assert_eq!(rooted.cons_car(), Value::fixnum(21));
    assert!(
        after < before,
        "exact safe point with explicit roots should free unrelated garbage: before={before}, after={after}"
    );
}

#[test]
fn gc_safe_point_exact_frees_stack_only_values() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.tagged_heap.set_gc_threshold(1);
    let marker = 0u8;
    ev.tagged_heap.set_stack_bottom(&marker as *const u8);

    ev.gc_collect_exact();
    let baseline = ev.tagged_heap.allocated_count();
    let gc_count_before = ev.gc_count;
    let stack_only = Value::cons(Value::fixnum(41), Value::fixnum(42));
    let keep_visible = [stack_only];
    std::hint::black_box(&keep_visible);
    let after_alloc = ev.tagged_heap.allocated_count();
    assert_eq!(
        after_alloc,
        baseline + 1,
        "stack-only cons should have allocated exactly one object after the baseline collection: baseline={baseline}, after_alloc={after_alloc}"
    );

    while ev.gc_count == gc_count_before {
        ev.gc_safe_point_exact();
    }

    let after_gc = ev.tagged_heap.allocated_count();
    assert_eq!(
        after_gc, baseline,
        "exact GC safe points must ignore the configured conservative stack scan and free stack-only objects: baseline={baseline}, after_alloc={after_alloc}, after_gc={after_gc}"
    );
}

#[test]
fn eval_sub_exact_gc_retains_cons_form() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.tagged_heap.set_gc_threshold(1);

    let form = Value::list(vec![
        Value::symbol("car"),
        Value::list(vec![
            Value::symbol("quote"),
            Value::cons(Value::fixnum(9), Value::fixnum(10)),
        ]),
    ]);
    let result = ev
        .eval_sub(form)
        .map_err(crate::emacs_core::error::map_flow);

    assert_eq!(format_eval_result(&result), "OK 9");
    assert!(ev.gc_count > 0, "exact eval_sub path should trigger GC");
}

#[test]
fn apply_exact_gc_retains_rooted_args() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.tagged_heap.set_gc_threshold(1);

    let arg = Value::cons(Value::fixnum(12), Value::fixnum(13));
    let result = ev
        .apply(Value::symbol("car"), vec![arg])
        .map_err(crate::emacs_core::error::map_flow);

    assert_eq!(format_eval_result(&result), "OK 12");
    assert!(ev.gc_count > 0, "exact apply path should trigger GC");
}

#[test]
fn gc_collect_runs_post_gc_hook() {
    crate::test_utils::init_test_tracing();
    let result = eval_one(
        "(progn
           (setq gc-hook-log nil)
           (setq post-gc-hook
                 (list (lambda ()
                         (setq gc-hook-log (cons 'ran gc-hook-log)))))
           (garbage-collect)
           gc-hook-log)",
    );
    assert_eq!(result, "OK (ran)");
}

#[test]
fn gc_safe_point_runs_post_gc_hook_when_incremental_collection_finishes() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.eval_str_each(
        "(progn
           (setq gc-hook-log nil)
           (setq post-gc-hook
                 (list (lambda ()
                         (setq gc-hook-log (cons 'ran gc-hook-log))))))",
    );
    ev.tagged_heap.set_gc_threshold(5);
    ev.eval_str_each("(progn (cons 1 2) (cons 3 4) (cons 5 6) (cons 7 8) (cons 9 10) nil)");
    while ev.gc_count == 0 {
        ev.gc_safe_point();
    }
    assert!(ev.gc_count > 0);
    let hook_log = ev.obarray().symbol_value("gc-hook-log").copied();
    assert!(hook_log.is_some());
    let entries = list_to_vec(&hook_log.unwrap()).expect("gc-hook-log list");
    assert!(!entries.is_empty());
    assert!(entries.iter().all(|entry| *entry == Value::symbol("ran")));
}

// -----------------------------------------------------------------------
// GC stress tests — force collection between every top-level form
// -----------------------------------------------------------------------

fn eval_stress(src: &str) -> Vec<String> {
    let mut ev = Context::new();
    let forms = crate::emacs_core::value_reader::read_all(src).expect("parse");
    ev.gc_stress = true;
    // Force very low threshold so gc_safe_point triggers on every call
    ev.tagged_heap.set_gc_threshold(1);
    // Root all parsed forms before the eval loop. The Vec<Value>
    // lives on the malloc heap and is invisible to conservative
    // stack scanning; without rooting, the forced low-threshold
    // GC reclaims the cons cells while we are still iterating.
    let roots = ev.save_specpdl_roots();
    for form in &forms {
        ev.push_specpdl_root(*form);
    }
    let mut results = Vec::new();
    for form in &forms {
        let r = ev.eval_form(*form);
        results.push(format_eval_result(&r));
        ev.gc_safe_point();
    }
    ev.restore_specpdl_roots(roots);
    results
}

#[test]
fn gc_stress_arithmetic() {
    crate::test_utils::init_test_tracing();
    let r = eval_stress("(+ 1 2) (* 3 4) (- 10 5)");
    assert_eq!(r, vec!["OK 3", "OK 12", "OK 5"]);
}

#[test]
fn gc_stress_cons_operations() {
    crate::test_utils::init_test_tracing();
    let r = eval_stress(
        "(setq x (cons 1 (cons 2 (cons 3 nil))))
         (car x)
         (car (cdr x))
         (length x)",
    );
    assert_eq!(r, vec!["OK (1 2 3)", "OK 1", "OK 2", "OK 3"]);
}

#[test]
fn gc_stress_vector_operations() {
    crate::test_utils::init_test_tracing();
    let r = eval_stress(
        "(setq v (vector 10 20 30))
         (aref v 0)
         (aset v 1 99)
         (aref v 1)",
    );
    assert_eq!(r, vec!["OK [10 20 30]", "OK 10", "OK 99", "OK 99"]);
}

#[test]
fn gc_stress_hash_table() {
    crate::test_utils::init_test_tracing();
    let r = eval_stress(
        "(setq ht (make-hash-table :test 'equal))
         (puthash \"a\" 1 ht)
         (puthash \"b\" 2 ht)
         (gethash \"a\" ht)
         (gethash \"b\" ht)
         (hash-table-count ht)",
    );
    assert_eq!(r[3], "OK 1");
    assert_eq!(r[4], "OK 2");
    assert_eq!(r[5], "OK 2");
}

#[test]
fn gc_stress_closures() {
    crate::test_utils::init_test_tracing();
    // Test lambdas and funcall survive GC (dynamic binding).
    // Lexical capture across separate top-level forms is a
    // pre-existing limitation unrelated to GC.
    let r = eval_stress(
        "(defalias 'my-add #'(lambda (a b) (+ a b)))
         (setq f (lambda (x) (my-add x 10)))
         (funcall f 5)
         (funcall f 20)",
    );
    assert_eq!(r[2], "OK 15");
    assert_eq!(r[3], "OK 30");
}

#[test]
fn gc_stress_lambda_argument_closure_survives_binding_installation() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    ev.gc_stress = true;
    ev.tagged_heap.set_gc_threshold(1);
    let result = format_eval_result(&ev.eval_str(
        r#"(let ((payload (list 1 2 3)))
             ((lambda (orig)
                (funcall orig))
              (lambda () payload)))"#,
    ));
    assert_eq!(result, "OK (1 2 3)");
}

#[test]
fn gc_stress_direct_lambda_head_roots_fresh_closure_during_arg_eval() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    ev.gc_stress = true;
    ev.tagged_heap.set_gc_threshold(1);
    let result = format_eval_result(&ev.eval_str(
        r#"((lambda (f value)
              (funcall f value))
            (lambda (x) x)
            (prog1 (list 1 2 3)
              (list 4 5 6)
              (list 7 8 9)))"#,
    ));
    assert_eq!(result, "OK (1 2 3)");
}

#[test]
fn gc_stress_builtin_apply_roots_closure_function_argument() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    ev.gc_stress = true;
    ev.tagged_heap.set_gc_threshold(1);
    let result = format_eval_result(&ev.eval_str(
        r#"(let ((payload (list 7 8 9)))
             (let ((f (lambda () payload)))
               (apply f nil)))"#,
    ));
    assert_eq!(result, "OK (7 8 9)");
}

#[test]
fn gc_stress_macro_expansion_result_stays_rooted_for_eval() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.gc_stress = true;
    ev.tagged_heap.set_gc_threshold(1);
    let result = format_eval_result(&ev.eval_str(
        r#"(progn
             (defalias 'vm-gc-expand-put
               (cons 'macro
                     #'(lambda ()
                         (list 'put ''vm-gc-expand-target ''custom-version "29.1"))))
             (vm-gc-expand-put)
             (get 'vm-gc-expand-target 'custom-version))"#,
    ));
    assert_eq!(result, "OK \"29.1\"");
}

#[test]
fn gc_stress_closure_call_restores_outer_lexenv_after_exact_gc() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    ev.gc_stress = true;
    ev.tagged_heap.set_gc_threshold(1);
    let result = format_eval_result(&ev.eval_str(
        r#"(let ((warnings nil))
             (let ((warn (lambda (form)
                           (setq warnings (cons form warnings)))))
               (funcall warn 'a)
               warnings))"#,
    ));
    assert_eq!(result, "OK (a)");
}

#[test]
fn gc_stress_let_star_lexical_binding_roots_evaluated_values() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    ev.gc_stress = true;
    ev.tagged_heap.set_gc_threshold(1);
    let result = format_eval_result(&ev.eval_str(
        r#"(let ((build (lambda () (list 4 5 6))))
             (let* ((x (funcall build))
                    (y x))
               y))"#,
    ));
    assert_eq!(result, "OK (4 5 6)");
}

#[test]
fn gc_stress_prog1_roots_first_value() {
    crate::test_utils::init_test_tracing();
    let r = eval_stress("(prog1 (list 1 2 3) (list 4 5 6) (list 7 8 9))");
    assert_eq!(r[0], "OK (1 2 3)");
}

#[test]
fn gc_stress_apply_env_expander_closure_capturing_uninterned_symbol() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    ev.lexenv = Value::list(vec![Value::T]);
    ev.gc_stress = true;
    ev.tagged_heap.set_gc_threshold(1);
    let result = format_eval_result(&ev.eval_str(
        r#"
        (let ((newenv nil)
              (magic (make-symbol "vm-magic")))
          (let ((var (make-symbol "vm-var")))
            (setq newenv
                  (cons
                   (cons 'vm-head
                         (lambda (&rest args)
                           (if (eq (car args) magic)
                               (list magic var)
                             (cons 'funcall (cons var args)))))
                   newenv))
            (let* ((form '(vm-head 1 2 3))
                   (head (car form))
                   (env-expander (assq head newenv)))
              (apply (cdr env-expander) (cdr form)))))
        "#,
    ));
    assert_eq!(result, "OK (funcall vm-var 1 2 3)");
}

#[test]
fn interpreted_closure_while_can_advance_lexical_loop_variable() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    let result = format_eval_result(&ev.eval_str(
        r#"
        (funcall
         (let ((items '(a b c)))
           (lambda ()
             (let ((l items)
                   (count 0))
               (while l
                 (setq l (cdr l))
                 (setq count (1+ count)))
               count))))
        "#,
    ));
    assert_eq!(result, "OK 3");
}

#[test]
fn interpreted_closure_trim_cache_survives_exact_gc() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_lexical_binding(true);

    ev.eval_str(
        r#"
        (setq vm-interpreted-closure-count 0)
        (fset 'cconv-make-interpreted-closure
              (lambda (args body env docstring iform)
                (setq vm-interpreted-closure-count
                      (1+ vm-interpreted-closure-count))
                (make-interpreted-closure args body env docstring iform)))
        (setq internal-make-interpreted-closure-function
              'cconv-make-interpreted-closure)
        "#,
    )
    .expect("eval forms");

    let filter_fn = ev
        .obarray()
        .symbol_function("cconv-make-interpreted-closure")
        .expect("cconv interpreted closure filter");
    ev.set_interpreted_closure_filter_fn(Some(filter_fn));

    let first = format_eval_result(&ev.eval_str("(funcall (let ((x 1)) (lambda () x)))"));
    assert_eq!(first, "OK 1");

    ev.gc_collect_exact();

    let count = format_eval_result(&ev.eval_str("vm-interpreted-closure-count"));
    assert_eq!(count, "OK 1");
}

#[test]
fn value_lambda_instantiation_uses_interpreted_closure_trim_cache() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_lexical_binding(true);

    ev.eval_str(
        r#"
        (setq vm-interpreted-closure-count 0)
        (fset 'cconv-make-interpreted-closure
              (lambda (args body env docstring iform)
                (setq vm-interpreted-closure-count
                      (1+ vm-interpreted-closure-count))
                (make-interpreted-closure args body env docstring iform)))
        (setq internal-make-interpreted-closure-function
              'cconv-make-interpreted-closure)
        "#,
    )
    .expect("eval forms");

    let filter_fn = ev
        .obarray()
        .symbol_function("cconv-make-interpreted-closure")
        .expect("cconv interpreted closure filter");
    ev.set_interpreted_closure_filter_fn(Some(filter_fn));

    let rendered = format_eval_result(&ev.eval_str(
        r#"(let ((x 1))
             (list (funcall '(lambda () x))
                   (funcall '(lambda () x))
                   vm-interpreted-closure-count))"#,
    ));
    assert_eq!(rendered, "OK (1 1 1)");
}

#[test]
fn gc_stress_aref_on_closure_survives_closure_vector_conversion() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    ev.gc_stress = true;
    ev.tagged_heap.set_gc_threshold(1);
    let result = format_eval_result(&ev.eval_str(
        r#"(let ((payload (list 1 2 3)))
             (let ((closure (lambda () payload)))
               (not (null (aref closure 2)))))"#,
    ));
    assert_eq!(result, "OK t");
}

#[test]
fn gc_stress_cdr_on_lambda_survives_cons_list_conversion() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    ev.gc_stress = true;
    ev.tagged_heap.set_gc_threshold(1);
    let result = format_eval_result(&ev.eval_str(
        r#"(let ((payload (list 1 2 3)))
             (let ((closure (lambda () payload)))
               (not (null (car (cdr closure))))))"#,
    ));
    assert_eq!(result, "OK t");
}

#[test]
fn gc_stress_recursive_function() {
    crate::test_utils::init_test_tracing();
    let r = eval_stress(
        "(defalias 'my-length #'(lambda (lst)
           (if (null lst) 0
             (1+ (my-length (cdr lst))))))
         (my-length '(a b c d e))
         (my-length nil)",
    );
    assert_eq!(r[1], "OK 5");
    assert_eq!(r[2], "OK 0");
}

#[test]
fn gc_stress_setcar_setcdr() {
    crate::test_utils::init_test_tracing();
    let r = eval_stress(
        "(setq x (cons 1 2))
         (setcar x 10)
         (setcdr x 20)
         x",
    );
    assert_eq!(r[3], "OK (10 . 20)");
}

#[test]
fn gc_stress_let_bindings() {
    crate::test_utils::init_test_tracing();
    let r = eval_stress(
        "(let ((a (cons 1 2))
               (b (cons 3 4)))
           (cons (car a) (car b)))",
    );
    assert_eq!(r[0], "OK (1 . 3)");
}

#[test]
fn gc_stress_mapcar() {
    crate::test_utils::init_test_tracing();
    let r = eval_stress("(mapcar '1+ '(1 2 3 4 5))");
    assert_eq!(r[0], "OK (2 3 4 5 6)");
}

#[test]
fn gc_stress_string_operations() {
    crate::test_utils::init_test_tracing();
    let r = eval_stress(
        r#"(setq s (concat "hello" " " "world"))
           (length s)
           (substring s 0 5)"#,
    );
    assert_eq!(r[0], r#"OK "hello world""#);
    assert_eq!(r[1], "OK 11");
    assert_eq!(r[2], r#"OK "hello""#);
}

#[test]
fn gc_stress_nreverse() {
    crate::test_utils::init_test_tracing();
    let r = eval_stress(
        "(setq x (list 1 2 3 4 5))
         (setq y (nreverse x))
         y",
    );
    assert_eq!(r[2], "OK (5 4 3 2 1)");
}

#[test]
fn gc_stress_plist() {
    crate::test_utils::init_test_tracing();
    let r = eval_stress(
        "(setq pl '(a 1 b 2 c 3))
         (plist-get pl 'b)
         (setq pl (plist-put pl 'b 99))
         (plist-get pl 'b)",
    );
    assert_eq!(r[1], "OK 2");
    assert_eq!(r[3], "OK 99");
}

#[test]
fn gc_stress_circular_list_survives() {
    crate::test_utils::init_test_tracing();
    // Create circular list inside a single progn to avoid formatting
    // the circular cons (which would hang the Display impl).
    let r = eval_stress(
        "(progn
           (setq x (cons 42 nil))
           (setcdr x x)
           (car x))",
    );
    assert_eq!(r[0], "OK 42");
}

#[test]
fn gc_stress_many_allocations() {
    crate::test_utils::init_test_tracing();
    // Allocate many short-lived conses; only final result should survive
    // dotimes is no longer a special form; use let+while equivalent
    let r = eval_stress(
        "(let ((result nil) (i 0))
           (while (< i 100)
             (setq result (cons i result))
             (setq i (1+ i)))
           (length result))",
    );
    assert_eq!(r[0], "OK 100");
}

// -----------------------------------------------------------------------
// Lexical closure mutation visibility tests
// -----------------------------------------------------------------------

#[test]
fn lexical_closure_mutation_visible() {
    crate::test_utils::init_test_tracing();
    // Closures must share the same lexical frame — mutations through
    // one closure must be visible to the outer scope.
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    let result = format_eval_result(&ev.eval_str(
        r#"(let ((x 0))
             (let ((f (lambda () (setq x (1+ x)))))
               (funcall f)
               (funcall f)
               x))"#,
    ));
    assert_eq!(result, "OK 2");
}

#[test]
fn lexical_closure_shared_state() {
    crate::test_utils::init_test_tracing();
    // Two closures sharing the same binding (inc + get).
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    let result = format_eval_result(&ev.eval_str(
        r#"(let ((x 0))
             (let ((inc (lambda () (setq x (1+ x))))
                   (get (lambda () x)))
               (funcall inc)
               (funcall inc)
               (funcall inc)
               (funcall get)))"#,
    ));
    assert_eq!(result, "OK 3");
}

#[test]
fn lexical_closure_make_counter() {
    crate::test_utils::init_test_tracing();
    // Classic make-counter pattern with independent counters.
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    let result = format_eval_result(&ev.eval_str(
        r#"(progn
             (defalias 'make-counter #'(lambda ()
               (let ((n 0))
                 (lambda () (setq n (1+ n))))))
             (let ((c1 (make-counter))
                   (c2 (make-counter)))
               (funcall c1)
               (funcall c1)
               (funcall c1)
               (let ((r1 (funcall c1))
                     (r2 (funcall c2)))
                 (list r1 r2))))"#,
    ));
    // c1 called 4 times → 4; c2 called once → 1; independent counters
    assert_eq!(result, "OK (4 1)");
}

#[test]
fn lexical_closure_outer_mutation_visible() {
    crate::test_utils::init_test_tracing();
    // Outer setq visible to closure.
    let mut ev = Context::new();
    ev.set_lexical_binding(true);
    let result = format_eval_result(&ev.eval_str(
        r#"(let ((x 10))
             (let ((f (lambda () x)))
               (setq x 42)
               (funcall f)))"#,
    ));
    assert_eq!(result, "OK 42");
}

#[test]
fn closure_inside_mapcar_lambda_captures_outer_param() {
    crate::test_utils::init_test_tracing();
    // Reproduces the pcase-compile-patterns pattern:
    // (mapcar (lambda (case)
    //           (list case
    //                 (lambda (vars) case)))
    //         '(a b c))
    // Each inner lambda should capture `case` from the outer lambda.
    let mut ev = crate::test_utils::runtime_startup_context();
    ev.set_lexical_binding(true);
    let result = format_eval_result(&ev.eval_str(
        r#"(let ((closures
                 (mapcar (lambda (case)
                           (lambda () case))
                         '(a b c))))
             (list (funcall (car closures))
                   (funcall (car (cdr closures)))
                   (funcall (car (cdr (cdr closures))))))"#,
    ));
    assert_eq!(result, "OK (a b c)");
}

#[test]
fn closure_inside_backquote_mapcar_captures_outer_param() {
    crate::test_utils::init_test_tracing();
    // More closely matches pcase-compile-patterns:
    // The inner lambda is created inside a backquote, after a function call.
    let mut ev = crate::test_utils::runtime_startup_context();
    ev.set_lexical_binding(true);
    let result = format_eval_result(&ev.eval_str(
        r#"(let ((closures
                 (mapcar (lambda (case)
                           (list (car case)
                                 (lambda (vars)
                                   (list case vars))))
                         '((a 1) (b 2) (c 3)))))
             (let ((fn2 (car (cdr (car closures)))))
               (funcall fn2 42)))"#,
    ));
    assert_eq!(result, "OK ((a 1) 42)");
}

#[test]
fn closure_inside_real_backquote_with_fn_call_captures_outer_param() {
    crate::test_utils::init_test_tracing();
    // Replicates the exact pcase-compile-patterns pattern:
    // (mapcar (lambda (case)
    //           `(,(some-fn val (car case))
    //             ,(lambda (vars) (list case vars))))
    //         cases)
    // The inner lambda is inside a REAL backquote (macro), after a function call.
    // This requires loading backquote.el.
    let mut eval = crate::test_utils::runtime_startup_context();
    load_minimal_gnu_backquote_runtime(&mut eval);

    let result = format_eval_result(&eval.eval_str(
        r#"(progn
             (defalias 'my-match #'(lambda (val upat) (list val upat)))
             (let ((closures
                    (mapcar (lambda (case)
                              `(,(my-match 'x (car case))
                                ,(lambda (vars) (list case vars))))
                            '((a 1) (b 2)))))
               (let ((fn1 (car (cdr (car closures)))))
                 (funcall fn1 'matched))))"#,
    ));
    assert_eq!(result, "OK ((a 1) matched)");
}

#[test]
fn real_backquote_computed_symbols_match_runtime_macro_semantics() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    load_minimal_gnu_backquote_runtime(&mut eval);

    let result = format_eval_result(&eval.eval_str(
        r#"(let ((prefix "neovm-bqc-test")
                 (suffixes '("x" "y" "z")))
             (let ((forms
                    (let ((i 0))
                      (mapcar (lambda (s)
                                (setq i (1+ i))
                                `(list ',(intern (concat prefix "-" s)) ,i))
                              suffixes))))
               (mapcar #'eval forms)))"#,
    ));
    assert_eq!(
        result,
        "OK ((neovm-bqc-test-x 1) (neovm-bqc-test-y 2) (neovm-bqc-test-z 3))"
    );
}

#[test]
fn real_backquote_macroexpand_preserves_debug_head_before_splice() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_all_with_subr(
            "(progn
               (fset 'neovm--debug-head
                     (cons 'macro
                           (lambda (condition)
                             `((debug ,@(if (listp condition)
                                            condition
                                          (list condition)))))))
               (macroexpand '(neovm--debug-head error)))"
        )[0],
        "OK ((debug error))"
    );
}

#[test]
fn loaded_subr_condition_case_unless_debug_calls_debugger_before_handler() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    load_minimal_gnu_backquote_runtime(&mut eval);

    assert_eq!(
        format_eval_result(&eval.eval_str(
            r#"(progn
           (setq neovm-debugger-called nil)
           (let ((debug-on-error t)
               (debugger (lambda (&rest args)
                           (setq neovm-debugger-called args))))
             (list (condition-case-unless-debug nil
                       (signal 'error 1)
                     (error 'handled))
                   neovm-debugger-called)))"#
        )),
        "OK (handled (error (error . 1)))"
    );
}

#[test]
fn loaded_subr_condition_case_unless_debug_macroexpand_includes_debug_marker() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    load_minimal_gnu_backquote_runtime(&mut eval);

    assert_eq!(
        format_eval_result(&eval.eval_str(
            r#"(equal
            (macroexpand '(condition-case-unless-debug nil
                            (signal 'error 1)
                            (error 42)))
            '(condition-case nil
               (signal 'error 1)
               ((debug error) 42)))"#
        )),
        "OK t"
    );
}

#[test]
fn macroexpand_environment_shadows_alias_targets_like_gnu() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        eval_all(
            "(let* ((alias-target (make-symbol \"ma-target\"))
                    (alias-head (make-symbol \"ma-head\")))
               (fset alias-target (cons 'macro (lambda (x) (list 'global x))))
               (fset alias-head alias-target)
               (macroexpand (list alias-head 42)
                            (list (cons alias-target
                                        (lambda (x) (list 'env x))))))"
        )[0],
        "OK (env 42)"
    );
}

#[test]
fn lexical_condition_case_debug_marker_calls_debugger_before_handler() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.set_lexical_binding(true);

    assert_eq!(
        format_eval_result(&eval.eval_str(
            r#"(progn
           (setq neovm-debugger-called nil)
           (let ((debug-on-error t)
               (debugger (lambda (&rest args)
                           (setq neovm-debugger-called args))))
             (list (condition-case nil
                       (signal 'error 1)
                     ((debug error) 'handled))
                   neovm-debugger-called)))"#
        )),
        "OK (handled (error (error . 1)))"
    );
}

#[test]
fn real_backquote_nested_eval_chain_matches_gnu_error_shape() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    load_minimal_gnu_backquote_runtime(&mut eval);

    let result = format_eval_result(&eval.eval_str(
        r#"(let ((x 10))
             (let ((template `(let ((y ,,x)) `(+ ,y ,,x))))
               (list template
                     (condition-case e (eval template) (error (cons 'ERR e)))
                     (condition-case e (eval (eval template)) (error (cons 'ERR e))))))"#,
    ));
    assert_eq!(result, r#"ERR (void-function (\,))"#);
}

#[test]
fn condition_case_lexical_handler_binding_restores_outer_let() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.set_lexical_binding(true);

    let result = format_eval_result(&eval.eval_str(
        r#"(let ((outer 'original))
             (list
              (condition-case outer
                  (/ 1 0)
                (arith-error
                 (setq outer (list 'caught (car outer)))
                 outer))
              outer))"#,
    ));
    assert_eq!(result, "OK ((caught arith-error) original)");
}

#[test]
fn gc_stress_lexical_closure_mutation() {
    crate::test_utils::init_test_tracing();
    // GC stress variant of closure mutation.
    let r = eval_stress(
        "(let ((x 0))
           (let ((f (lambda () (setq x (1+ x)))))
             (funcall f)
             (funcall f)
             (funcall f)
             x))",
    );
    assert_eq!(r[0], "OK 3");
}

#[test]
fn evaluator_face_table_has_standard_faces() {
    crate::test_utils::init_test_tracing();
    let ev = Context::new();
    let ft = ev.face_table();

    // Standard faces must exist
    assert!(ft.get("default").is_some(), "missing default face");
    assert!(ft.get("bold").is_some(), "missing bold face");
    assert!(ft.get("italic").is_some(), "missing italic face");
    assert!(ft.get("mode-line").is_some(), "missing mode-line face");
    assert!(
        ft.get("minibuffer-prompt").is_some(),
        "missing minibuffer-prompt face"
    );

    // Resolve should apply inheritance (bold inherits from default)
    let bold = ft.resolve("bold");
    assert!(
        bold.foreground.is_some(),
        "bold should inherit foreground from default"
    );
    assert!(
        bold.weight.map_or(false, |w| w.is_bold()),
        "bold face should have bold weight",
    );
}

#[test]
fn advice_around_compiler_macro_pattern() {
    crate::test_utils::init_test_tracing();
    // Reproduce the cl-macs pattern: macroexp--compiler-macro calls a
    // compiler-macro handler. condition-case-unless-debug should catch
    // wrong-number-of-arguments errors.
    let results = eval_all(
        r#"
        ;; Simulate a compiler-macro handler that needs 2 args
        (defalias 'my-cmacro-handler #'(lambda (form arg)
          (list 'optimized form arg)))

        ;; But it gets called with wrong arity via apply
        (condition-case err
            (apply 'my-cmacro-handler '((my-fn 1 2) 1 2))
          (wrong-number-of-arguments
           (list 'caught-wna err)))
        "#,
    );
    for (i, r) in results.iter().enumerate() {
        eprintln!("cmacro[{i}]: {r}");
    }
}

#[test]
fn oclosure_define_basic() {
    crate::test_utils::init_test_tracing();
    // Test basic oclosure-define usage - the pattern that fails in loadup
    let results = eval_all(
        r#"
        ;; oclosure-define should create a type
        (condition-case err
            (oclosure-define my-test-ocl "A test oclosure type.")
          (error (list 'error err)))
        ;; Check if it worked
        (condition-case err
            (oclosure-define my-test-ocl2 "Another test." (slot1))
          (error (list 'error err)))
        "#,
    );
    for (i, r) in results.iter().enumerate() {
        eprintln!("oclosure-define[{i}]: {r}");
    }
}

#[test]
fn oclosure_define_macroexpand() {
    crate::test_utils::init_test_tracing();
    // Trace what oclosure-define expands to
    let results = eval_all(
        r#"
        ;; Check if oclosure-define is a macro
        (fboundp 'oclosure-define)
        (macroexpand-1 '(oclosure-define my-test-ocl "Test type."))
        (macroexpand-1 '(oclosure-define my-test-ocl2 "Test2." (slot1)))
        "#,
    );
    for (i, r) in results.iter().enumerate() {
        eprintln!("macroexpand-ocl[{i}]: {r}");
    }
}

#[test]
fn cl_defstruct_keyword_handling() {
    crate::test_utils::init_test_tracing();
    // Test cl-defstruct with :copier/:constructor keywords
    // These fail with (invalid-function :copier) in loadup
    let results = eval_all(
        r#"
        ;; Check if cl-defstruct is a macro
        (fboundp 'cl-defstruct)
        (condition-case err
            (macroexpand '(cl-defstruct (my-test-struct (:copier nil)) field1 field2))
          (error (list 'macroexpand-error err)))
        (condition-case err
            (cl-defstruct (my-test-struct (:copier nil)) field1 field2)
          (error (list 'error err)))
        "#,
    );
    for (i, r) in results.iter().enumerate() {
        eprintln!("cl-defstruct[{i}]: {r}");
    }
}

#[test]
fn cl_deftype_basic() {
    crate::test_utils::init_test_tracing();
    // Test cl-deftype which fails in ring.el with (void-variable ring)
    let results = eval_all(
        r#"
        (condition-case err
            (cl-deftype my-ring-test nil '(satisfies ring-p))
          (error (list 'error err)))
        "#,
    );
    for (i, r) in results.iter().enumerate() {
        eprintln!("cl-deftype[{i}]: {r}");
    }
}

#[test]
fn bootstrap_window_system_modes_match_gnu_defaults() {
    crate::test_utils::init_test_tracing();
    let eval = Context::new();
    assert_eq!(
        eval.obarray().symbol_value("menu-bar-mode"),
        Some(&Value::T),
        "GNU initializes menu-bar-mode to t in frame.c"
    );
    assert_eq!(
        eval.obarray().symbol_value("tool-bar-mode"),
        Some(&Value::T),
        "GNU initializes tool-bar-mode to t for window-system builds"
    );
}
