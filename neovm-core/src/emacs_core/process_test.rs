use super::*;
use crate::emacs_core::load::{apply_runtime_startup_state, create_bootstrap_evaluator_cached};
use crate::emacs_core::{Context, format_eval_result, parse_forms};
use std::cell::RefCell;
use std::rc::Rc;

/// Create an evaluator with minimal Elisp shims for process testing.
/// These shims mirror GNU Emacs Elisp functions that wrap C-level builtins.
fn eval_with_process_shims() -> Context {
    let mut ev = Context::new();
    // Define minimal Elisp shims matching GNU Emacs subr.el/env.el
    let shims = r#"
(defalias 'getenv #'(lambda (variable &optional frame)
  (getenv-internal variable)))
(defalias 'setenv #'(lambda (variable &optional value substitute)
  (setenv-internal variable value t)))
(defalias 'start-process #'(lambda (name buffer program &rest args)
  (make-process :name name :buffer buffer
                :command (if program (cons program args)))))
(defalias 'start-process-shell-command #'(lambda (name buffer command)
  (start-process name buffer shell-file-name
                 shell-command-switch command)))
(defalias 'shell-command-to-string #'(lambda (command)
  (with-output-to-string
    (call-process shell-file-name nil standard-output nil
                  shell-command-switch command))))
"#;
    let forms = parse_forms(shims).expect("parse shims");
    for form in &forms {
        let _ = ev.eval_expr(form);
    }
    ev
}

fn eval_one(src: &str) -> String {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let forms = parse_forms(src).expect("parse");
    let result = ev.eval_expr(&forms[0]);
    format_eval_result(&result)
}

fn eval_all(src: &str) -> Vec<String> {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let forms = parse_forms(src).expect("parse");
    ev.eval_forms(&forms)
        .iter()
        .map(format_eval_result)
        .collect()
}

fn bootstrap_eval_one(src: &str) -> String {
    let mut ev = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut ev).expect("runtime startup state");
    let forms = parse_forms(src).expect("parse");
    let result = ev.eval_expr(&forms[0]);
    format_eval_result(&result)
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

fn eval_one_in_context(ev: &mut Context, src: &str) -> String {
    let forms = parse_forms(src).expect("parse");
    let result = ev.eval_expr(&forms[0]);
    format_eval_result(&result)
}

/// Find the path of a binary, trying /bin, /usr/bin, and PATH lookup.
fn find_bin(name: &str) -> String {
    for dir in &["/bin", "/usr/bin", "/run/current-system/sw/bin"] {
        let path = format!("{}/{}", dir, name);
        if std::path::Path::new(&path).exists() {
            return path;
        }
    }
    // Fallback: try to find via `which`
    if let Ok(output) = std::process::Command::new("which").arg(name).output() {
        if output.status.success() {
            return String::from_utf8_lossy(&output.stdout).trim().to_string();
        }
    }
    // Last resort: return the bare name and let Command search PATH
    name.to_string()
}

fn tmp_file(label: &str) -> String {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("time should be monotonic")
        .as_nanos();
    format!("/tmp/neovm-{label}-{}-{nonce}.txt", std::process::id())
}

// -- ProcessManager unit tests ------------------------------------------

#[test]
fn process_manager_create_and_query() {
    let mut pm = ProcessManager::new();
    let id = pm.create_process(
        "test".into(),
        Some("*test*".into()),
        "/bin/echo".into(),
        vec!["hello".into()],
    );
    assert!(id > 0);
    assert!(pm.get(id).is_some());
    assert_eq!(pm.get(id).unwrap().name, "test");
    assert_eq!(pm.get(id).unwrap().command, "/bin/echo");
    assert_eq!(pm.process_status(id), Some(&ProcessStatus::Run));
}

#[test]
fn process_manager_kill() {
    let mut pm = ProcessManager::new();
    let id = pm.create_process("p".into(), None, "prog".into(), vec![]);
    assert!(pm.kill_process(id));
    assert_eq!(pm.process_status(id), Some(&ProcessStatus::Signal(9)));
}

#[test]
fn process_manager_delete() {
    let mut pm = ProcessManager::new();
    let id = pm.create_process("p".into(), None, "prog".into(), vec![]);
    assert!(pm.delete_process(id));
    assert!(pm.get(id).is_none());
}

#[test]
fn process_manager_send_input() {
    let mut pm = ProcessManager::new();
    let id = pm.create_process("p".into(), None, "prog".into(), vec![]);
    assert!(pm.send_input(id, "hello "));
    assert!(pm.send_input(id, "world"));
    assert_eq!(pm.get(id).unwrap().stdin_queue, "hello world");
}

#[test]
fn process_manager_find_by_name() {
    let mut pm = ProcessManager::new();
    let id = pm.create_process("my-proc".into(), None, "prog".into(), vec![]);
    assert_eq!(pm.find_by_name("my-proc"), Some(id));
    assert_eq!(pm.find_by_name("nonexistent"), None);
}

#[test]
fn process_manager_list() {
    let mut pm = ProcessManager::new();
    let id1 = pm.create_process("a".into(), None, "p".into(), vec![]);
    let id2 = pm.create_process("b".into(), None, "q".into(), vec![]);
    let ids = pm.list_processes();
    assert!(ids.contains(&id1));
    assert!(ids.contains(&id2));
    assert_eq!(ids.len(), 2);
}

#[test]
fn process_manager_env() {
    let mut pm = ProcessManager::new();
    pm.setenv("NEOVM_TEST_VAR".into(), Some("hello".into()));
    assert_eq!(pm.getenv("NEOVM_TEST_VAR"), Some("hello".into()));
    pm.setenv("NEOVM_TEST_VAR".into(), None);
    assert_eq!(pm.getenv("NEOVM_TEST_VAR"), None);
}

// -- Elisp-level tests --------------------------------------------------

#[test]
fn start_process_and_query() {
    let echo = find_bin("echo");
    let results = eval_all(&format!(
        r#"(start-process "my-proc" nil "{echo}" "hello")
           (process-status 1)
           (process-name 1)
           (process-buffer 1)"#,
    ));
    assert_eq!(results[0], "OK 1");
    assert_eq!(results[1], "OK run");
    assert_eq!(results[2], r#"OK "my-proc""#);
    assert_eq!(results[3], "OK nil");
}

#[test]
fn start_process_with_buffer() {
    let cat = find_bin("cat");
    let results = eval_all(&format!(
        r#"(start-process "p" "*output*" "{cat}")
           (bufferp (process-buffer 1))
           (equal (buffer-name (process-buffer 1)) "*output*")"#,
    ));
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK t");
}

#[test]
fn start_process_buffer_name_program_and_arg_contracts_match_oracle() {
    let cat = find_bin("cat");
    let results = eval_all(&format!(
        r#"(with-temp-buffer
             (let ((p (start-process "neo-sp-contract-buffer" (current-buffer) "{cat}")))
               (unwind-protect
                   (list (processp p)
                         (null (condition-case err (process-send-eof nil) (error err)))
                         (null (condition-case err (process-running-child-p nil) (error err))))
                 (ignore-errors (delete-process p)))))
           (condition-case err (start-process 'neo-sp-contract-name nil "{cat}") (error err))
           (condition-case err (start-process t nil "{cat}") (error err))
           (condition-case err (start-process nil nil "{cat}") (error err))
           (condition-case err (start-process "neo-sp-contract-buf-symbol" 'x "{cat}") (error err))
           (condition-case err (start-process "neo-sp-contract-buf-t" t "{cat}") (error err))
           (condition-case err (start-process "neo-sp-contract-buf-int" 1 "{cat}") (error err))
           (condition-case err (start-process "neo-sp-contract-prog-symbol" nil 'cat) (error err))
           (condition-case err (start-process "neo-sp-contract-prog-t" nil t) (error err))
           (processp (start-process "neo-sp-contract-prog-nil" nil nil))
           (condition-case err (start-process "neo-sp-contract-arg-symbol" nil "{cat}" 'a) (error err))
           (condition-case err (start-process "neo-sp-contract-arg-t" nil "{cat}" t) (error err))
           (condition-case err (start-process "neo-sp-contract-arg-nil" nil "{cat}" nil) (error err))
           (condition-case err (start-process "neo-sp-contract-arg-int" nil "{cat}" 1) (error err))"#,
    ));
    assert_eq!(results[0], "OK (t t t)");
    assert_eq!(results[1], r#"OK (error ":name value not a string")"#);
    assert_eq!(results[2], r#"OK (error ":name value not a string")"#);
    assert_eq!(results[3], r#"OK (error ":name value not a string")"#);
    assert_eq!(results[4], "OK (wrong-type-argument stringp x)");
    assert_eq!(results[5], "OK (wrong-type-argument stringp t)");
    assert_eq!(results[6], "OK (wrong-type-argument stringp 1)");
    assert_eq!(results[7], "OK (wrong-type-argument stringp cat)");
    assert_eq!(results[8], "OK (wrong-type-argument stringp t)");
    assert_eq!(results[9], "OK t");
    assert_eq!(results[10], "OK (wrong-type-argument stringp a)");
    assert_eq!(results[11], "OK (wrong-type-argument stringp t)");
    assert_eq!(results[12], "OK (wrong-type-argument stringp nil)");
    assert_eq!(results[13], "OK (wrong-type-argument stringp 1)");
}

#[test]
fn call_process_and_start_file_process_string_contracts_match_oracle() {
    let echo = find_bin("echo");
    let results = eval_all(&format!(
        r#"(condition-case err (call-process nil) (error err))
           (condition-case err (call-process t) (error err))
           (condition-case err (call-process 'foo) (error err))
           (condition-case err (call-process "{echo}" nil nil nil 'x) (error err))
           (condition-case err (call-process "{echo}" nil nil nil t) (error err))
           (condition-case err (call-process "{echo}" nil nil nil nil) (error err))
           (with-temp-buffer
             (insert "x")
             (condition-case err (call-process-region (point-min) (point-min) nil) (error err)))
           (with-temp-buffer
             (insert "x")
             (condition-case err (call-process-region (point-min) (point-min) t) (error err)))
           (with-temp-buffer
             (insert "x")
             (condition-case err (call-process-region (point-min) (point-min) 'foo) (error err)))
           (with-temp-buffer
             (insert "x")
             (condition-case err (call-process-region (point-min) (point-min) "{echo}" nil nil nil 'x) (error err)))
           (with-temp-buffer
             (insert "x")
             (condition-case err (call-process-region (point-min) (point-min) "{echo}" nil nil nil t) (error err)))
           (with-temp-buffer
             (insert "x")
             (condition-case err (call-process-region (point-min) (point-min) "{echo}" nil nil nil nil) (error err)))
           (condition-case err (start-file-process "neo-sfp-contract-arg-symbol" nil "{echo}" 'x) (error err))
           (condition-case err (start-file-process "neo-sfp-contract-arg-t" nil "{echo}" t) (error err))
           (condition-case err (start-file-process "neo-sfp-contract-arg-nil" nil "{echo}" nil) (error err))
           (condition-case err (start-file-process "neo-sfp-contract-program-symbol" nil 'echo) (error err))
           (condition-case err (start-file-process "neo-sfp-contract-program-t" nil t) (error err))
           (let ((p (start-file-process "neo-sfp-contract-program-nil" nil nil)))
             (unwind-protect (processp p) (ignore-errors (delete-process p))))"#,
    ));

    assert_eq!(results[0], "OK (wrong-type-argument stringp nil)");
    assert_eq!(results[1], "OK (wrong-type-argument stringp t)");
    assert_eq!(results[2], "OK (wrong-type-argument stringp foo)");
    assert_eq!(results[3], "OK (wrong-type-argument stringp x)");
    assert_eq!(results[4], "OK (wrong-type-argument stringp t)");
    assert_eq!(results[5], "OK (wrong-type-argument stringp nil)");
    assert_eq!(results[6], "OK (wrong-type-argument stringp nil)");
    assert_eq!(results[7], "OK (wrong-type-argument stringp t)");
    assert_eq!(results[8], "OK (wrong-type-argument stringp foo)");
    assert_eq!(results[9], "OK (wrong-type-argument stringp x)");
    assert_eq!(results[10], "OK (wrong-type-argument stringp t)");
    assert_eq!(results[11], "OK (wrong-type-argument stringp nil)");
    assert_eq!(results[12], "OK (wrong-type-argument stringp x)");
    assert_eq!(results[13], "OK (wrong-type-argument stringp t)");
    assert_eq!(results[14], "OK (wrong-type-argument stringp nil)");
    assert_eq!(results[15], "OK (wrong-type-argument stringp echo)");
    assert_eq!(results[16], "OK (wrong-type-argument stringp t)");
    assert_eq!(results[17], "OK t");
}

#[test]
fn delete_process_removes() {
    let echo = find_bin("echo");
    let results = eval_all(&format!(
        r#"(start-process "p" nil "{echo}")
           (delete-process 1)
           (process-list)"#,
    ));
    assert_eq!(results[2], "OK nil");
}

#[test]
fn process_send_string_test() {
    let cat = find_bin("cat");
    let results = eval_all(&format!(
        r#"(start-process "p" nil "{cat}")
           (process-send-string 1 "hello")"#,
    ));
    assert_eq!(results[1], "OK nil");
}

#[test]
fn process_exit_status_initial() {
    let echo = find_bin("echo");
    let results = eval_all(&format!(
        r#"(start-process "p" nil "{echo}")
           (process-exit-status 1)"#,
    ));
    assert_eq!(results[1], "OK 0");
}

#[test]
fn process_list_test() {
    let echo = find_bin("echo");
    let cat = find_bin("cat");
    let results = eval_all(&format!(
        r#"(start-process "a" nil "{echo}")
           (start-process "b" nil "{cat}")
           (process-list)"#,
    ));
    // Process list contains two entries.  Order may vary.
    let list_str = &results[2];
    assert!(list_str.contains("1"));
    assert!(list_str.contains("2"));
}

#[test]
fn call_process_echo() {
    let echo = find_bin("echo");
    // call-process with echo, inserting into current buffer
    let results = eval_all(&format!(
        r#"(get-buffer-create "cp-test")
           (set-buffer "cp-test")
           (call-process "{echo}" nil t nil "hello" "world")
           (buffer-string)"#,
    ));
    // Exit code should be 0.
    assert_eq!(results[2], "OK 0");
    // Buffer should contain "hello world\n".
    assert_eq!(results[3], "OK \"hello world\n\"");
}

#[test]
fn call_process_no_destination() {
    let echo = find_bin("echo");
    // call-process with nil destination discards output
    let results = eval_all(&format!(
        r#"(get-buffer-create "cp-nil")
           (set-buffer "cp-nil")
           (call-process "{echo}" nil nil nil "hello")
           (buffer-string)"#,
    ));
    assert_eq!(results[2], "OK 0");
    assert_eq!(results[3], r#"OK """#);
}

#[test]
fn call_process_display_requests_redisplay_after_buffer_insert() {
    let echo = find_bin("echo");
    let mut ev = Context::new();
    let buf_id = ev.buffers.create_buffer("*cp-display*");
    assert!(ev.buffers.switch_current(buf_id));

    let redisplay_calls = Rc::new(RefCell::new(Vec::new()));
    let calls_in_cb = redisplay_calls.clone();
    ev.redisplay_fn = Some(Box::new(move |ev: &mut Context| {
        let current_id = ev.buffers.current_buffer_id().expect("current buffer");
        calls_in_cb.borrow_mut().push(
            ev.buffers
                .get(current_id)
                .expect("current buffer")
                .buffer_string(),
        );
    }));

    crate::emacs_core::callproc::builtin_call_process(
        &mut ev,
        vec![
            Value::string(echo),
            Value::Nil,
            Value::True,
            Value::True,
            Value::string("hello"),
        ],
    )
    .expect("call-process should succeed");

    assert_eq!(
        ev.buffers
            .get(buf_id)
            .expect("display buffer")
            .buffer_string(),
        "hello\n"
    );
    assert_eq!(*redisplay_calls.borrow(), vec!["hello\n".to_string()]);
}

#[test]
fn call_process_infile_feeds_stdin() {
    let cat = find_bin("cat");
    let infile = tmp_file("cp-infile");
    std::fs::write(&infile, "infile-data").expect("write infile");
    let results = eval_all(&format!(
        r#"(with-temp-buffer
             (list
               (call-process "{cat}" "{infile}" t nil)
               (buffer-string)))"#
    ));
    assert_eq!(results[0], r#"OK (0 "infile-data")"#);
    let _ = std::fs::remove_file(&infile);
}

#[test]
fn call_process_destination_buffer_name_inserts_there() {
    let echo = find_bin("echo");
    let results = eval_all(&format!(
        r#"(get-buffer-create "cp-src")
           (get-buffer-create "cp-dst")
           (set-buffer "cp-src")
           (erase-buffer)
           (set-buffer "cp-dst")
           (erase-buffer)
           (set-buffer "cp-src")
           (call-process "{echo}" nil "cp-dst" nil "hello")
           (list
             (with-current-buffer "cp-src" (buffer-string))
             (with-current-buffer "cp-dst" (buffer-string)))"#,
    ));
    assert_eq!(results[7], "OK 0");
    assert_eq!(results[8], "OK (\"\" \"hello\n\")");
}

#[test]
fn call_process_file_destination_collects_stdout_and_stderr() {
    let sh = find_bin("sh");
    let out = tmp_file("cp-file");
    let _ = std::fs::remove_file(&out);
    let results = eval_all(&format!(
        r#"(call-process "{sh}" nil '(:file "{out}") nil "-c" "echo out; echo err >&2")
           (with-temp-buffer (insert-file-contents "{out}") (buffer-string))"#
    ));
    assert_eq!(results[0], "OK 0");
    assert!(results[1].contains("out"));
    assert!(results[1].contains("err"));
    let _ = std::fs::remove_file(&out);
}

#[test]
fn call_process_pair_destination_splits_stderr_to_file() {
    let sh = find_bin("sh");
    let out = tmp_file("cp-pair-out");
    let err = tmp_file("cp-pair-err");
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&err);
    let results = eval_all(&format!(
        r#"(call-process "{sh}" nil '((:file "{out}") "{err}") nil "-c" "echo out; echo err >&2")
           (with-temp-buffer (insert-file-contents "{out}") (buffer-string))
           (with-temp-buffer (insert-file-contents "{err}") (buffer-string))"#
    ));
    assert_eq!(results[0], "OK 0");
    assert!(results[1].contains("out"));
    assert!(!results[1].contains("err"));
    assert!(results[2].contains("err"));
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&err);
}

#[test]
fn call_process_integer_destination_returns_nil() {
    let echo = find_bin("echo");
    // Any integer destination behaves like 0: discard and return nil.
    let results = eval_all(&format!(
        r#"(get-buffer-create "cp-int")
           (set-buffer "cp-int")
           (call-process "{echo}" nil 2 nil "hello")
           (buffer-string)"#,
    ));
    assert_eq!(results[2], "OK nil");
    assert_eq!(results[3], r#"OK """#);
}

#[test]
fn call_process_false() {
    let false_bin = find_bin("false");
    // false exits with code 1
    let result = eval_one(&format!(r#"(call-process "{false_bin}")"#));
    assert_eq!(result, "OK 1");
}

#[test]
fn call_process_region_test() {
    let cat = find_bin("cat");
    let results = eval_all(&format!(
        r#"(get-buffer-create "cpr-test")
           (set-buffer "cpr-test")
           (insert "hello world")
           (call-process-region 1 12 "{cat}" nil t)
           (buffer-string)"#,
    ));
    // exit code 0
    assert_eq!(results[3], "OK 0");
    // Buffer should contain original text plus piped output
    assert!(results[4].contains("hello world"));
}

#[test]
fn call_process_region_display_requests_redisplay_after_buffer_insert() {
    let cat = find_bin("cat");
    let mut ev = Context::new();
    let buf_id = ev.buffers.create_buffer("*cpr-display*");
    assert!(ev.buffers.switch_current(buf_id));

    let redisplay_calls = Rc::new(RefCell::new(Vec::new()));
    let calls_in_cb = redisplay_calls.clone();
    ev.redisplay_fn = Some(Box::new(move |ev: &mut Context| {
        let current_id = ev.buffers.current_buffer_id().expect("current buffer");
        calls_in_cb.borrow_mut().push(
            ev.buffers
                .get(current_id)
                .expect("current buffer")
                .buffer_string(),
        );
    }));

    crate::emacs_core::callproc::builtin_call_process_region(
        &mut ev,
        vec![
            Value::string("xyz"),
            Value::Nil,
            Value::string(cat),
            Value::Nil,
            Value::True,
            Value::True,
        ],
    )
    .expect("call-process-region should succeed");

    assert_eq!(
        ev.buffers
            .get(buf_id)
            .expect("display buffer")
            .buffer_string(),
        "xyz"
    );
    assert_eq!(*redisplay_calls.borrow(), vec!["xyz".to_string()]);
}

#[test]
fn call_process_region_destination_buffer_name_inserts_there() {
    let cat = find_bin("cat");
    let results = eval_all(&format!(
        r#"(get-buffer-create "cpr-src")
           (get-buffer-create "cpr-dst")
           (with-current-buffer "cpr-src" (erase-buffer) (insert "abc"))
           (with-current-buffer "cpr-dst" (erase-buffer))
           (with-current-buffer "cpr-src"
             (call-process-region (point-min) (point-max) "{cat}" nil "cpr-dst" nil))
           (list
             (with-current-buffer "cpr-src" (buffer-string))
             (with-current-buffer "cpr-dst" (buffer-string)))"#,
    ));
    assert_eq!(results[4], "OK 0");
    assert_eq!(results[5], r#"OK ("abc" "abc")"#);
}

#[test]
fn call_process_region_file_destination_writes_file() {
    let cat = find_bin("cat");
    let out = tmp_file("cpr-file");
    let _ = std::fs::remove_file(&out);
    let results = eval_all(&format!(
        r#"(with-temp-buffer
             (insert "abc")
             (call-process-region (point-min) (point-max) "{cat}" nil '(:file "{out}") nil))
           (with-temp-buffer (insert-file-contents "{out}") (buffer-string))"#
    ));
    assert_eq!(results[0], "OK 0");
    assert_eq!(results[1], r#"OK "abc""#);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn call_process_region_start_nil_uses_whole_buffer() {
    let cat = find_bin("cat");
    let results = eval_all(&format!(
        r#"(with-temp-buffer
             (insert "abc")
             (list (call-process-region nil nil "{cat}" nil t nil)
                   (buffer-string)))"#
    ));
    assert_eq!(results[0], r#"OK (0 "abcabc")"#);
}

#[test]
fn call_process_region_start_string_uses_string_input() {
    let cat = find_bin("cat");
    let results = eval_all(&format!(
        r#"(with-temp-buffer
             (insert "abc")
             (list (call-process-region "xyz" nil "{cat}" nil t nil)
                   (buffer-string)))"#
    ));
    assert_eq!(results[0], r#"OK (0 "abcxyz")"#);
}

#[test]
fn call_process_region_start_string_with_delete_signals_wrong_type() {
    let cat = find_bin("cat");
    let result = eval_one(&format!(
        r#"(condition-case err
               (call-process-region "xyz" nil "{cat}" t t nil)
             (error (car err)))"#
    ));
    assert_eq!(result, "OK wrong-type-argument");
}

#[test]
fn call_process_region_accepts_marker_positions() {
    let cat = find_bin("cat");
    let results = eval_all(&format!(
        r#"(with-temp-buffer
             (insert "abcdef")
             (goto-char 3)
             (let ((m (copy-marker (point))))
               (list (call-process-region m (point-max) "{cat}" nil t nil)
                     (buffer-string))))"#
    ));
    assert_eq!(results[0], r#"OK (0 "abcdefcdef")"#);
}

#[test]
fn call_process_region_reversed_bounds_are_accepted() {
    let cat = find_bin("cat");
    let results = eval_all(&format!(
        r#"(with-temp-buffer
             (insert "abc")
             (list (call-process-region (point-max) (point-min) "{cat}" nil t nil)
                   (buffer-string)))"#
    ));
    assert_eq!(results[0], r#"OK (0 "abcabc")"#);
}

#[test]
fn call_process_region_reversed_bounds_with_delete_delete_region() {
    let cat = find_bin("cat");
    let results = eval_all(&format!(
        r#"(with-temp-buffer
             (insert "abc")
             (list (call-process-region (point-max) (point-min) "{cat}" t t nil)
                   (buffer-string)))"#
    ));
    assert_eq!(results[0], r#"OK (0 "abc")"#);
}

#[test]
fn call_process_region_negative_start_signals_args_out_of_range() {
    let cat = find_bin("cat");
    let result = eval_one(&format!(
        r#"(with-temp-buffer
             (insert "abc")
             (condition-case err
                 (call-process-region -1 2 "{cat}" nil t nil)
               (error (car err))))"#
    ));
    assert_eq!(result, "OK args-out-of-range");
}

#[test]
fn call_process_region_huge_end_signals_args_out_of_range() {
    let cat = find_bin("cat");
    let result = eval_one(&format!(
        r#"(with-temp-buffer
             (insert "abc")
             (condition-case err
                 (call-process-region 1 999999 "{cat}" nil t nil)
               (error (car err))))"#
    ));
    assert_eq!(result, "OK args-out-of-range");
}

#[test]
fn call_process_region_integer_destination_returns_nil() {
    let cat = find_bin("cat");
    let results = eval_all(&format!(
        r#"(get-buffer-create "cpr-int")
           (set-buffer "cpr-int")
           (erase-buffer)
           (insert "abc")
           (call-process-region 1 4 "{cat}" nil 3 nil)
           (buffer-string)"#,
    ));
    assert_eq!(results[4], "OK nil");
    assert_eq!(results[5], r#"OK "abc""#);
}

#[test]
fn shell_command_to_string_test() {
    let result = eval_one(r#"(shell-command-to-string "echo -n hello")"#);
    assert_eq!(result, r#"OK "hello""#);
}

#[test]
fn shell_command_to_string_with_pipe() {
    let result = eval_one(r#"(shell-command-to-string "echo hello | tr a-z A-Z")"#);
    assert_eq!(result, "OK \"HELLO\n\"");
}

#[test]
fn getenv_path() {
    // PATH should always be set — use getenv-internal (C builtin)
    let result = eval_one(r#"(getenv-internal "PATH")"#);
    assert!(result.starts_with("OK \""));
}

#[test]
fn getenv_nonexistent() {
    let result = eval_one(r#"(getenv-internal "NEOVM_DEFINITELY_NOT_SET_12345")"#);
    assert_eq!(result, "OK nil");
}

#[test]
fn getenv_name_must_be_string() {
    let result = eval_one(r#"(condition-case err (getenv-internal nil) (error err))"#);
    assert_eq!(result, "OK (wrong-type-argument stringp nil)");
}

#[test]
fn getenv_accepts_optional_nil_env_arg() {
    let result = eval_one(
        r#"(condition-case err
               (let ((v (getenv-internal "HOME" nil)))
                 (if (stringp v) 'string v))
             (error err))"#,
    );
    assert_eq!(result, "OK string");
}

#[test]
fn getenv_rejects_more_than_two_args() {
    let result =
        eval_one(r#"(condition-case err (getenv-internal "HOME" nil nil) (error (car err)))"#);
    assert_eq!(result, "OK wrong-number-of-arguments");
}

#[test]
fn setenv_and_getenv() {
    let results = eval_all(
        r#"(setenv "NEOVM_TEST_SETENV" "myvalue")
           (getenv "NEOVM_TEST_SETENV")"#,
    );
    assert_eq!(results[0], r#"OK "myvalue""#);
    assert_eq!(results[1], r#"OK "myvalue""#);
}

#[test]
fn setenv_unset() {
    let results = eval_all(
        r#"(setenv "NEOVM_TEST_UNSET" "val")
           (setenv "NEOVM_TEST_UNSET")
           (getenv "NEOVM_TEST_UNSET")"#,
    );
    assert_eq!(results[2], "OK nil");
}

#[test]
fn setenv_name_must_be_string() {
    let result = eval_one(r#"(condition-case err (setenv nil "v") (error err))"#);
    assert_eq!(result, "OK (wrong-type-argument stringp nil)");
}

#[test]
fn setenv_accepts_sequence_value_and_sets_environment() {
    let vector_result = eval_one(
        r#"(let ((old (getenv "NEOVM_TEST_SETENV_SEQ")))
             (unwind-protect
                 (progn
                   (setenv "NEOVM_TEST_SETENV_SEQ" [118 97 108])
                   (getenv "NEOVM_TEST_SETENV_SEQ"))
               (setenv "NEOVM_TEST_SETENV_SEQ" old)))"#,
    );
    assert_eq!(vector_result, r#"OK "val""#);

    let list_result = eval_one(
        r#"(let ((old (getenv "NEOVM_TEST_SETENV_SEQ")))
             (unwind-protect
                 (progn
                   (setenv "NEOVM_TEST_SETENV_SEQ" '(118 97 108))
                   (getenv "NEOVM_TEST_SETENV_SEQ"))
               (setenv "NEOVM_TEST_SETENV_SEQ" old)))"#,
    );
    assert_eq!(list_result, r#"OK "val""#);
}

#[test]
fn setenv_substitute_flag_controls_expansion_and_requires_string() {
    let unsubstituted = eval_one(
        r#"(let ((old (getenv "NEOVM_TEST_SETENV_SEQ")))
             (unwind-protect
                 (progn
                   (setenv "NEOVM_TEST_SETENV_SEQ" "$HOME")
                   (getenv "NEOVM_TEST_SETENV_SEQ"))
               (setenv "NEOVM_TEST_SETENV_SEQ" old)))"#,
    );
    assert_eq!(unsubstituted, r#"OK "$HOME""#);

    let substituted = eval_one(
        r#"(let ((old (getenv "NEOVM_TEST_SETENV_SEQ")))
             (unwind-protect
                 (progn
                   (setenv "NEOVM_TEST_SETENV_SEQ" "$HOME" t)
                   (getenv "NEOVM_TEST_SETENV_SEQ"))
               (setenv "NEOVM_TEST_SETENV_SEQ" old)))"#,
    );
    assert!(substituted.starts_with("OK \""));
    assert_ne!(substituted, r#"OK "$HOME""#);

    let type_err = eval_one(
        r#"(condition-case err (setenv "NEOVM_TEST_SETENV_SEQ" [118 97 108] t) (error err))"#,
    );
    assert_eq!(type_err, "OK (wrong-type-argument stringp [118 97 108])");
}

#[test]
fn setenv_rejects_non_sequence_value() {
    let result = eval_one(r#"(condition-case err (setenv "NEOVM_TEST_SETENV_SEQ" 1) (error err))"#);
    assert_eq!(result, "OK (wrong-type-argument sequencep 1)");
}

#[test]
fn setenv_rejects_too_many_args() {
    let result = eval_one(
        r#"(condition-case err (setenv "NEOVM_TEST_SETENV_SEQ" "v" nil nil) (error (car err)))"#,
    );
    assert_eq!(result, "OK wrong-number-of-arguments");
}

#[test]
fn set_binary_mode_stream_contract_matches_oracle() {
    let results = eval_all(
        r#"(condition-case err (set-binary-mode 'stdin t) (error err))
           (condition-case err (set-binary-mode 'stdout nil) (error err))
           (condition-case err (set-binary-mode 'stderr t) (error err))
           (condition-case err (set-binary-mode 'foo t) (error err))
           (condition-case err (set-binary-mode nil t) (error err))
           (condition-case err (set-binary-mode t t) (error err))
           (condition-case err (set-binary-mode 1 t) (error err))"#,
    );
    assert_eq!(results[0], "OK t");
    assert_eq!(results[1], "OK t");
    assert_eq!(results[2], "OK t");
    assert_eq!(results[3], r#"OK (error "unsupported stream" foo)"#);
    assert_eq!(results[4], r#"OK (error "unsupported stream" nil)"#);
    assert_eq!(results[5], r#"OK (error "unsupported stream" t)"#);
    assert_eq!(results[6], "OK (wrong-type-argument symbolp 1)");
}

#[test]
fn call_process_bad_program() {
    let result = eval_one(r#"(call-process "/nonexistent/program_xyz")"#);
    assert!(result.contains("ERR"));
}

#[test]
fn call_process_bad_program_signals_file_missing() {
    let result = eval_one(
        r#"(condition-case err (call-process "/nonexistent/program_xyz") (error (car err)))"#,
    );
    assert_eq!(result, "OK file-missing");
}

#[test]
fn call_process_missing_infile_signals_file_missing() {
    let cat = find_bin("cat");
    let result = eval_one(&format!(
        r#"(condition-case err (call-process "{cat}" "/nonexistent/neovm-process-infile") (error (car err)))"#
    ));
    assert_eq!(result, "OK file-missing");
}

#[test]
fn call_process_region_bad_program_signals_file_missing() {
    let result = eval_one(
        r#"(condition-case err (call-process-region 1 1 "/nonexistent/program_xyz") (error (car err)))"#,
    );
    assert_eq!(result, "OK file-missing");
}

#[test]
fn call_process_symbol_destination_signals_wrong_type_argument() {
    let echo = find_bin("echo");
    let result = eval_one(&format!(
        r#"(condition-case err (call-process "{echo}" nil 'foo nil "x") (error err))"#
    ));
    assert_eq!(result, "OK (wrong-type-argument stringp foo)");
}

#[test]
fn call_process_bad_stderr_target_signals_wrong_type_argument() {
    let echo = find_bin("echo");
    let result = eval_one(&format!(
        r#"(condition-case err (call-process "{echo}" nil '(t 99) nil "x") (error err))"#
    ));
    assert_eq!(result, "OK (wrong-type-argument stringp 99)");
}

#[test]
fn process_status_wrong_arg_type() {
    let result = eval_one(r#"(process-status 999)"#);
    assert!(result.contains("ERR"));
}

#[test]
fn start_process_multiple_args() {
    let echo = find_bin("echo");
    let results = eval_all(&format!(
        r#"(start-process "echo" nil "{echo}" "a" "b" "c")
           (process-name 1)"#,
    ));
    assert_eq!(results[0], "OK 1");
    assert_eq!(results[1], r#"OK "echo""#);
}

#[test]
fn process_runtime_introspection_controls() {
    let cat = find_bin("cat");
    let results = eval_all(&format!(
        r#"(let ((p (start-process "proc-introspect" nil "{cat}")))
             (list
              (processp p)
              (equal (process-live-p p) '(run open listen connect stop))
              (integerp (process-id p))
              (process-contact p t)
              (process-filter p)
              (set-process-filter p nil)
              (set-process-filter p 'ignore)
              (process-filter p)
              (process-sentinel p)
              (set-process-sentinel p nil)
              (set-process-sentinel p 'ignore)
              (process-sentinel p)
              (set-process-plist p '(a 1))
              (process-get p 'a)
              (process-put p 'k 2)
              (process-get p 'k)
              (process-query-on-exit-flag p)
              (set-process-query-on-exit-flag p nil)
              (process-query-on-exit-flag p)
              (delete-process p)
              (process-live-p p)))"#,
    ));
    assert_eq!(
        results[0],
        "OK (t t t t internal-default-process-filter internal-default-process-filter ignore ignore internal-default-process-sentinel internal-default-process-sentinel ignore ignore (a 1 k 2) 1 (a 1 k 2) 2 t nil nil nil nil)"
    );
}

#[test]
fn process_contact_keyword_matrix_for_network_and_pipe() {
    let result = eval_one(
        r#"(list
            (let ((p (make-network-process :name "neo-contact-key-net" :server t :service 0)))
              (unwind-protect
                  (let ((port (process-contact p :service))
                        (local (process-contact p :local)))
                    (list
                     (stringp (process-contact p :name))
                     (eq (process-contact p :server) t)
                     (integerp port)
                     (and (vectorp local)
                          (= (length local) 5)
                          (= (aref local 0) 127)
                          (= (aref local 4) port))
                     (null (process-contact p :remote))
                     (null (process-contact p :coding))
                     (null (process-contact p :foo))))
                (ignore-errors (delete-process p))))
            (let ((p (make-pipe-process :name "neo-contact-key-pipe")))
              (unwind-protect
                  (list
                   (stringp (process-contact p :name))
                   (null (process-contact p :server))
                   (null (process-contact p :service))
                   (null (process-contact p :local))
                   (null (process-contact p :remote))
                   (null (process-contact p :coding))
                   (null (process-contact p :foo)))
                (ignore-errors (delete-process p)))))"#,
    );
    assert_eq!(result, "OK ((t t t t t t t) (t t t t t t t))");
}

#[test]
fn process_stale_mutator_matrix_matches_oracle() {
    let cat = find_bin("cat");
    let result = eval_one(&format!(
        r#"(let ((p (start-process "proc-stale-mutator" nil "{cat}")))
             (unwind-protect
                 (progn
                   (delete-process p)
                   (list
                    (set-process-filter p 'ignore)
                    (set-process-sentinel p 'ignore)
                    (set-process-plist p '(a 1))
                    (process-put p 'k 2)
                    (set-process-query-on-exit-flag p nil)
                    (set-process-buffer p nil)
                    (set-process-coding-system p 'utf-8-unix)
                    (set-process-inherit-coding-system-flag p t)
                    (set-process-thread p nil)
                    (set-process-window-size p 10 20)
                    (set-process-datagram-address p nil)))
               (ignore-errors (delete-process p))))"#,
    ));
    assert_eq!(
        result,
        "OK (ignore ignore (a 1 k 2) (a 1 k 2) nil nil nil t nil nil nil)"
    );
}

#[test]
fn process_stale_control_matrix_matches_oracle() {
    let cat = find_bin("cat");
    let result = eval_one(&format!(
        r#"(let ((p (start-process "proc-stale-control" nil "{cat}")))
             (unwind-protect
                 (progn
                   (delete-process p)
                   (list
                    (condition-case err (continue-process p) (error (car err)))
                    (condition-case err (interrupt-process p) (error (car err)))
                    (condition-case err (kill-process p) (error (car err)))
                    (condition-case err (stop-process p) (error (car err)))
                    (condition-case err (quit-process p) (error (car err)))
                    (let ((rv (signal-process p 0)))
                      (or (eq rv 0) (eq rv -1)))
                    (set-process-query-on-exit-flag p nil)
                    (process-query-on-exit-flag p)
                    (process-live-p p)
                    (process-status p)
                    (process-exit-status p)))
               (ignore-errors (delete-process p))))"#,
    ));
    assert_eq!(
        result,
        "OK (error error error error error t nil nil nil signal 9)"
    );
}

#[test]
fn process_attributes_runtime_shape_matches_oracle() {
    let result = eval_one(
        r#"(let ((attrs (process-attributes (emacs-pid))))
             (list
              (listp attrs)
              (null (assq 'pid attrs))
              (let ((pair (assq 'user attrs)))
                (and (consp pair) (stringp (cdr pair))))
              (let ((pair (assq 'group attrs)))
                (and (consp pair) (stringp (cdr pair))))
              (let ((pair (assq 'euid attrs)))
                (and (consp pair) (integerp (cdr pair))))
              (let ((pair (assq 'egid attrs)))
                (and (consp pair) (integerp (cdr pair))))
              (let ((pair (assq 'comm attrs)))
                (and (consp pair) (stringp (cdr pair))))
              (let ((pair (assq 'state attrs)))
                (and (consp pair) (stringp (cdr pair))))
              (let ((pair (assq 'ppid attrs)))
                (and (consp pair) (integerp (cdr pair))))
              (let ((pair (assq 'pgrp attrs)))
                (and (consp pair) (integerp (cdr pair))))
              (let ((pair (assq 'sess attrs)))
                (and (consp pair) (integerp (cdr pair))))
              (let ((pair (assq 'tpgid attrs)))
                (and (consp pair) (integerp (cdr pair))))
              (let ((pair (assq 'minflt attrs)))
                (and (consp pair) (integerp (cdr pair))))
              (let ((pair (assq 'majflt attrs)))
                (and (consp pair) (integerp (cdr pair))))
              (let ((pair (assq 'cminflt attrs)))
                (and (consp pair) (integerp (cdr pair))))
              (let ((pair (assq 'cmajflt attrs)))
                (and (consp pair) (integerp (cdr pair))))
              (let ((pair (assq 'pri attrs)))
                (and (consp pair) (integerp (cdr pair))))
              (let ((pair (assq 'nice attrs)))
                (and (consp pair) (integerp (cdr pair))))
              (let ((pair (assq 'thcount attrs)))
                (and (consp pair) (integerp (cdr pair))))
              (let ((pair (assq 'vsize attrs)))
                (and (consp pair) (integerp (cdr pair))))
              (let ((pair (assq 'rss attrs)))
                (and (consp pair) (integerp (cdr pair))))
              (let ((pair (assq 'ttname attrs)))
                (and (consp pair) (stringp (cdr pair))))
              (process-attributes -1)
              (condition-case err (process-attributes 'x) (error err))
              (process-attributes 999999999)))"#,
    );
    assert_eq!(
        result,
        "OK (t t t t t t t t t t t t t t t t t t t t t t nil (wrong-type-argument numberp x) nil)"
    );
}

#[test]
fn process_attributes_timing_memory_shape_matches_oracle() {
    let result = eval_one(
        r#"(let ((attrs (process-attributes (emacs-pid))))
             (list
              (let ((pair (assq 'utime attrs)))
                (and (consp pair) (consp (cdr pair))))
              (let ((pair (assq 'stime attrs)))
                (and (consp pair) (consp (cdr pair))))
              (let ((pair (assq 'time attrs)))
                (and (consp pair) (consp (cdr pair))))
              (let ((pair (assq 'cutime attrs)))
                (and (consp pair) (consp (cdr pair))))
              (let ((pair (assq 'cstime attrs)))
                (and (consp pair) (consp (cdr pair))))
              (let ((pair (assq 'ctime attrs)))
                (and (consp pair) (consp (cdr pair))))
              (let ((pair (assq 'start attrs)))
                (and (consp pair) (consp (cdr pair))))
              (let ((pair (assq 'etime attrs)))
                (and (consp pair) (consp (cdr pair))))
              (let ((pair (assq 'pcpu attrs)))
                (and (consp pair) (floatp (cdr pair))))
              (let ((pair (assq 'pmem attrs)))
                (and (consp pair) (floatp (cdr pair))))
              (let ((pair (assq 'args attrs)))
                (and (consp pair) (stringp (cdr pair))))
              (null (assq 'pid attrs))))"#,
    );
    assert_eq!(result, "OK (t t t t t t t t t t t t)");
}

#[test]
fn accept_process_output_and_get_process_runtime_surface() {
    let cat = find_bin("cat");
    let results = eval_all(&format!(
        r#"(condition-case err (accept-process-output) (error err))
           (condition-case err (accept-process-output nil 0.01) (error err))
           (condition-case err (accept-process-output 1) (error err))
           (condition-case err (accept-process-output nil "x") (error err))
           (let ((p (start-process "proc-get-probe" nil "{cat}")))
             (list
              (processp (get-process "proc-get-probe"))
              (eq p (get-process "proc-get-probe"))
              (accept-process-output p 0.0)
              (delete-process p)
              (accept-process-output p 0.0)
              (get-process "proc-get-probe")))
           (condition-case err (get-process 'proc-get-probe) (error err))"#,
    ));
    assert_eq!(results[0], "OK nil");
    assert_eq!(results[1], "OK nil");
    assert_eq!(results[2], "OK (wrong-type-argument processp 1)");
    assert_eq!(results[3], r#"OK (wrong-type-argument numberp "x")"#);
    assert_eq!(results[4], "OK (t t nil nil nil nil)");
    assert_eq!(
        results[5],
        "OK (wrong-type-argument stringp proc-get-probe)"
    );
}

#[test]
fn accept_process_output_millis_contract_matches_oracle() {
    let results = eval_all(
        r#"(condition-case err (accept-process-output nil 0.1 "x") (error err))
           (condition-case err (accept-process-output nil nil "x") (error err))
           (condition-case err (accept-process-output nil 1 "x") (error err))
           (condition-case err (accept-process-output nil 0.1 nil) (error err))
           (condition-case err (accept-process-output nil 0.1 0) (error err))
           (condition-case err (accept-process-output nil 1 2) (error err))"#,
    );
    assert_eq!(results[0], r#"OK (wrong-type-argument fixnump "x")"#);
    assert_eq!(results[1], r#"OK (wrong-type-argument fixnump "x")"#);
    assert_eq!(results[2], r#"OK (wrong-type-argument fixnump "x")"#);
    assert_eq!(results[3], "OK nil");
    assert_eq!(results[4], "OK (wrong-type-argument fixnump 0.1)");
    assert_eq!(results[5], "OK nil");
}

#[test]
fn accept_process_output_roots_callbacks_across_gc() {
    let echo = find_bin("echo");
    let mut ev = Context::new();
    let forms = parse_forms(&format!(
        r#"(progn
             (fset 'proc-root-filter
                   (lambda (_proc string)
                     (garbage-collect)
                     (setq proc-root-filter-data string)))
             (fset 'proc-root-sentinel
                   (lambda (_proc msg)
                     (setq proc-root-sentinel-data msg)))
             (setq proc-root-filter-data nil
                   proc-root-sentinel-data nil)
             (let ((p (make-process :name "proc-rooting"
                                    :buffer nil
                                    :command (list "{echo}" "out")
                                    :connection-type 'pipe)))
               (unwind-protect
                   (progn
                     (set-process-filter p 'proc-root-filter)
                     (set-process-sentinel p 'proc-root-sentinel)
                     (accept-process-output p 0.1)
                     (accept-process-output p 0.1)
                     (list proc-root-filter-data proc-root-sentinel-data))
                 (condition-case nil
                     (delete-process p)
                   (error nil)))))"#,
    ))
    .expect("parse");
    let result = ev.eval_expr(&forms[0]);
    assert_eq!(
        format_eval_result(&result),
        r#"OK ("out
" "finished
")"#
    );
}

#[test]
fn accept_process_output_waiting_for_target_still_services_other_processes() {
    let cat = find_bin("cat");
    let echo = find_bin("echo");
    let mut ev = Context::new();
    let result = eval_one_in_context(
        &mut ev,
        &format!(
            r#"(let ((target (make-process :name "apio-target"
                                      :buffer nil
                                      :command (list "{cat}")
                                      :connection-type 'pipe))
                 (other (make-process :name "apio-other"
                                     :buffer nil
                                     :command (list "{echo}" "other")
                                     :connection-type 'pipe))
                 (other-output nil))
             (unwind-protect
                 (progn
                   (set-process-filter other
                                       (lambda (_proc string)
                                         (setq other-output
                                               (cons string other-output))))
                   (list (accept-process-output target 0.1)
                         (nreverse other-output)))
               (condition-case nil (delete-process target) (error nil))
               (condition-case nil (delete-process other) (error nil))))"#,
        ),
    );
    assert_eq!(
        result,
        r#"OK (nil ("other
"))"#
    );
}

#[test]
fn accept_process_output_just_this_one_suspends_other_processes() {
    let cat = find_bin("cat");
    let echo = find_bin("echo");
    let mut ev = Context::new();
    let result = eval_one_in_context(
        &mut ev,
        &format!(
            r#"(let ((target (make-process :name "apio-target-only"
                                      :buffer nil
                                      :command (list "{cat}")
                                      :connection-type 'pipe))
                 (other (make-process :name "apio-other-only"
                                     :buffer nil
                                     :command (list "{echo}" "other")
                                     :connection-type 'pipe))
                 (other-output nil))
             (unwind-protect
                 (progn
                   (set-process-filter other
                                       (lambda (_proc string)
                                         (setq other-output
                                               (cons string other-output))))
                   (list (accept-process-output target 0.1 nil t)
                         (nreverse other-output)))
               (condition-case nil (delete-process target) (error nil))
               (condition-case nil (delete-process other) (error nil))))"#,
        ),
    );
    assert_eq!(result, "OK (nil nil)");
}

#[test]
fn accept_process_output_integer_just_this_one_suppresses_timers() {
    let cat = find_bin("cat");
    let mut ev = Context::new();
    let setup = parse_forms(
        r#"(progn
             (fset 'apio-wait-timer-callback
                   (lambda () (setq apio-wait-timer-fired t)))
             (setq apio-wait-timer-fired nil))"#,
    )
    .expect("parse timer callback setup");
    ev.eval_expr(&setup[0]).expect("install timer callback");

    let pid = ev
        .processes
        .create_process("apio-wait-target".into(), None, cat, Vec::new());
    ev.processes
        .spawn_child(pid, false)
        .expect("spawn target child");
    ev.timers.add_timer(
        0.0,
        0.0,
        Value::symbol("apio-wait-timer-callback"),
        vec![],
        false,
    );

    let first = builtin_accept_process_output(
        &mut ev,
        vec![
            Value::Int(pid as i64),
            Value::Float(0.0, next_float_id()),
            Value::Nil,
            Value::Int(1),
        ],
    )
    .expect("accept-process-output with integer just-this-one");
    let after_first = ev
        .eval_symbol("apio-wait-timer-fired")
        .expect("timer flag after timer-suppressed wait");
    let second = builtin_accept_process_output(
        &mut ev,
        vec![Value::Nil, Value::Float(0.0, next_float_id())],
    )
    .expect("accept-process-output should service timers without target restriction");
    let after_second = ev
        .eval_symbol("apio-wait-timer-fired")
        .expect("timer flag after unrestricted wait");

    assert_eq!(first, Value::Nil);
    assert_eq!(after_first, Value::Nil);
    assert_eq!(second, Value::Nil);
    assert_eq!(after_second, Value::True);
}

#[test]
fn accept_process_output_runs_default_process_filter() {
    let echo = find_bin("echo");
    let mut ev = Context::new();
    let _ = ev.buffers.create_buffer("*apio-default-filter*");
    let pid = ev.processes.create_process(
        "apio-default-filter".into(),
        Some("*apio-default-filter*".into()),
        echo,
        vec!["out".into()],
    );
    ev.processes
        .spawn_child(pid, false)
        .expect("spawn output process");

    assert_eq!(
        builtin_process_filter(&mut ev, vec![Value::Int(pid as i64)]).expect("process-filter"),
        Value::symbol("internal-default-process-filter")
    );

    let first = builtin_accept_process_output(
        &mut ev,
        vec![Value::Int(pid as i64), Value::Float(0.1, next_float_id())],
    )
    .expect("first accept-process-output");
    let second = builtin_accept_process_output(
        &mut ev,
        vec![Value::Int(pid as i64), Value::Float(0.1, next_float_id())],
    )
    .expect("second accept-process-output");
    let buf_id = ev
        .buffers
        .find_buffer_by_name("*apio-default-filter*")
        .expect("default filter should create process buffer");
    let text = ev
        .buffers
        .get(buf_id)
        .expect("process buffer")
        .buffer_string();

    assert_eq!(first, Value::True);
    assert_eq!(second, Value::Nil);
    assert_eq!(text, "out\n");
}

#[test]
fn accept_process_output_restores_current_buffer_and_match_data() {
    let echo = find_bin("echo");
    let mut ev = Context::new();
    let setup = parse_forms(
        r#"(fset 'apio-restore-filter
                  (lambda (_proc _string)
                    (set-buffer (get-buffer-create "*apio-restore-other*"))
                    (string-match "bb" "abba")))"#,
    )
    .expect("parse restore filter");
    ev.eval_expr(&setup[0]).expect("install restore filter");

    let home_id = ev.buffers.create_buffer("*apio-restore-home*");
    assert!(ev.buffers.switch_current(home_id));
    let _ = eval_one_in_context(&mut ev, r#"(string-match "yz" "xyz")"#);
    let before_match = parse_forms("(match-data)").expect("parse match-data");
    let before_match_data = ev
        .eval_expr(&before_match[0])
        .expect("capture match-data before callback");
    let before_buffer = ev.buffers.current_buffer_id();

    let pid = ev
        .processes
        .create_process("apio-restore".into(), None, echo, vec!["out".into()]);
    ev.processes
        .spawn_child(pid, false)
        .expect("spawn restore process");
    builtin_set_process_filter(
        &mut ev,
        vec![Value::Int(pid as i64), Value::symbol("apio-restore-filter")],
    )
    .expect("install process filter");

    let result = builtin_accept_process_output(
        &mut ev,
        vec![Value::Int(pid as i64), Value::Float(0.1, next_float_id())],
    )
    .expect("accept-process-output with restoring filter");
    let after_match_data = ev
        .eval_expr(&before_match[0])
        .expect("capture match-data after callback");

    assert_eq!(result, Value::True);
    assert_eq!(ev.buffers.current_buffer_id(), before_buffer);
    assert_eq!(after_match_data, before_match_data);
}

#[test]
fn accept_process_output_preserves_process_callback_runtime_state() {
    let echo = find_bin("echo");
    let mut ev = Context::new();
    let result = eval_one_in_context(
        &mut ev,
        &format!(
            r#"(progn
                 (fset 'apio-state-filter
                       (lambda (_proc string)
                         (setq apio-state-filter-observed
                               (list (current-buffer)
                                     (match-data)
                                     deactivate-mark
                                     last-nonmenu-event))
                         (set-buffer (get-buffer-create "*apio-state-other*"))
                         (string-match "bb" "abba")
                         (setq deactivate-mark nil)
                         (setq last-nonmenu-event 'changed)
                         (setq apio-state-filter-string string)))
                 (fset 'apio-state-sentinel
                       (lambda (_proc msg)
                         (setq apio-state-sentinel-observed
                               (list (current-buffer)
                                     (match-data)
                                     deactivate-mark
                                     last-nonmenu-event))
                         (set-buffer (get-buffer-create "*apio-state-other*"))
                         (string-match "cc" "acca")
                         (setq deactivate-mark nil)
                         (setq last-nonmenu-event 'changed)
                         (setq apio-state-sentinel-msg msg)))
                 (setq apio-state-filter-observed nil
                       apio-state-sentinel-observed nil
                       apio-state-filter-string nil
                       apio-state-sentinel-msg nil
                       last-nonmenu-event 'before
                       deactivate-mark 'keep)
                 (let ((home (get-buffer-create "*apio-state-home*")))
                   (set-buffer home)
                   (string-match "yz" "xyz")
                   (let ((before-buffer (current-buffer))
                         (before-match (match-data))
                         (p (make-process :name "apio-state"
                                          :buffer nil
                                          :command (list "{echo}" "out")
                                          :connection-type 'pipe)))
                     (unwind-protect
                         (progn
                           (set-process-filter p 'apio-state-filter)
                           (set-process-sentinel p 'apio-state-sentinel)
                           (accept-process-output p 0.1)
                           (accept-process-output p 0.1)
                           (list apio-state-filter-string
                                 apio-state-sentinel-msg
                                 (eq (current-buffer) before-buffer)
                                 (equal (match-data) before-match)
                                 deactivate-mark
                                 last-nonmenu-event
                                 (eq (nth 0 apio-state-filter-observed) before-buffer)
                                 (equal (nth 1 apio-state-filter-observed) before-match)
                                 (nth 2 apio-state-filter-observed)
                                 (nth 3 apio-state-filter-observed)
                                 (eq (nth 0 apio-state-sentinel-observed) before-buffer)
                                 (equal (nth 1 apio-state-sentinel-observed) before-match)
                                 (nth 2 apio-state-sentinel-observed)
                                 (nth 3 apio-state-sentinel-observed)))
                       (condition-case nil
                           (delete-process p)
                         (error nil))))))"#,
        ),
    );
    assert_eq!(
        result,
        r#"OK ("out
" "finished
" t t keep before t t keep t t t keep t)"#
    );
}

#[test]
fn make_network_process_open_sentinel_uses_shared_callback_runtime_state() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test listener");
    let port = listener.local_addr().expect("listener local addr").port();
    let accept_thread = std::thread::spawn(move || {
        let _ = listener.accept();
    });
    let mut ev = Context::new();
    let result = eval_one_in_context(
        &mut ev,
        &format!(
            r#"(progn
             (fset 'apio-net-open-sentinel
                   (lambda (_proc msg)
                     (setq apio-net-open-state
                           (list msg
                                 (eq (current-buffer) apio-net-before-buffer)
                                 (equal (match-data) apio-net-before-match)
                                 deactivate-mark
                                 last-nonmenu-event))
                     (set-buffer (get-buffer-create "*apio-net-other*"))
                     (string-match "bb" "abba")
                     (setq deactivate-mark nil)
                     (setq last-nonmenu-event 'changed)))
             (setq last-nonmenu-event 'before
                   deactivate-mark 'keep
                   apio-net-open-state nil)
             (let ((home (get-buffer-create "*apio-net-home*")))
               (set-buffer home)
               (string-match "yz" "xyz")
               (setq apio-net-before-buffer (current-buffer)
                     apio-net-before-match (match-data))
               (let ((p (make-network-process :name "apio-net-open"
                                               :host "127.0.0.1"
                                               :service {port}
                                               :sentinel 'apio-net-open-sentinel)))
                 (unwind-protect
                     (list (car apio-net-open-state)
                           (nth 1 apio-net-open-state)
                           (nth 2 apio-net-open-state)
                           (nth 3 apio-net-open-state)
                           (nth 4 apio-net-open-state)
                           (eq (current-buffer) apio-net-before-buffer)
                           (equal (match-data) apio-net-before-match)
                           deactivate-mark
                           last-nonmenu-event)
                   (condition-case nil
                       (delete-process p)
                     (error nil))))))"#,
        ),
    );
    let _ = accept_thread.join();
    assert_eq!(
        result,
        r#"OK ("open
" t t keep t t t keep before)"#
    );
}

#[test]
fn sleep_for_uses_shared_wait_path_for_process_output_and_timers() {
    let echo = find_bin("echo");
    let mut ev = Context::new();
    let setup = parse_forms(
        r#"(progn
             (fset 'sleep-shared-filter
                   (lambda (_proc string) (setq sleep-shared-output string)))
             (fset 'sleep-shared-timer
                   (lambda () (setq sleep-shared-timer-fired 'done)))
             (setq sleep-shared-output nil
                   sleep-shared-timer-fired nil))"#,
    )
    .expect("parse sleep-for callback setup");
    ev.eval_expr(&setup[0])
        .expect("install sleep-for callback setup");

    let pid = ev
        .processes
        .create_process("sleep-shared".into(), None, echo, vec!["out".into()]);
    ev.processes
        .spawn_child(pid, false)
        .expect("spawn sleep-for process");
    builtin_set_process_filter(
        &mut ev,
        vec![Value::Int(pid as i64), Value::symbol("sleep-shared-filter")],
    )
    .expect("install sleep-for process filter");
    ev.timers
        .add_timer(0.0, 0.0, Value::symbol("sleep-shared-timer"), vec![], false);

    crate::emacs_core::timer::builtin_sleep_for(&mut ev, vec![Value::Float(0.05, next_float_id())])
        .expect("sleep-for should use the shared wait path");

    assert_eq!(
        ev.eval_symbol("sleep-shared-output")
            .expect("sleep-for process output variable"),
        Value::string("out\n")
    );
    assert_eq!(
        ev.eval_symbol("sleep-shared-timer-fired")
            .expect("sleep-for timer variable"),
        Value::symbol("done")
    );
}

#[test]
fn process_mark_type_thread_send_and_running_child_runtime_surface() {
    let cat = find_bin("cat");
    let results = eval_all(&format!(
        r#"(let ((p (start-process "proc-mark-type-thread-send" nil "{cat}")))
             (unwind-protect
                 (list
                  (processp p)
                  (eq (process-type p) 'real)
                  (not (processp (process-thread p)))
                  (markerp (process-mark p))
                  (marker-buffer (process-mark p))
                  (marker-position (process-mark p))
                  (process-running-child-p p)
                  (processp (process-send-eof p))
                  (with-temp-buffer
                    (insert "abc")
                    (process-send-region p (point-min) (point-max)))
                  (delete-process p)
                  (process-live-p p))
               (ignore-errors (delete-process p))))
           (condition-case err (process-send-eof) (error (car err)))
           (condition-case err (process-running-child-p) (error (car err)))
           (condition-case err (process-mark 'x) (error err))
           (condition-case err (process-type 'x) (error err))
           (condition-case err (process-thread 'x) (error err))
           (condition-case err (process-send-region 'x 1 1) (error err))
           (condition-case err (process-send-eof 'x) (error err))
           (condition-case err (process-running-child-p 'x) (error err))
           (condition-case err (process-send-eof nil nil) (error (car err)))
           (condition-case err (process-running-child-p nil nil) (error (car err)))"#,
    ));
    assert_eq!(results[0], "OK (t t t t nil nil nil t nil nil nil)");
    assert_eq!(results[1], "OK error");
    assert_eq!(results[2], "OK error");
    assert_eq!(results[3], "OK (wrong-type-argument processp x)");
    assert_eq!(results[4], "OK (wrong-type-argument processp x)");
    assert_eq!(results[5], "OK (wrong-type-argument processp x)");
    assert_eq!(results[6], "OK (wrong-type-argument processp x)");
    assert_eq!(results[7], "OK (wrong-type-argument processp x)");
    assert_eq!(results[8], "OK (wrong-type-argument processp x)");
    assert_eq!(results[9], "OK wrong-number-of-arguments");
    assert_eq!(results[10], "OK wrong-number-of-arguments");
}

#[test]
fn process_coding_tty_and_kill_buffer_query_runtime_surface() {
    let cat = find_bin("cat");
    let results = eval_all(&format!(
        r#"(let ((p (start-process "proc-coding-tty-query" nil "{cat}")))
             (unwind-protect
                 (list
                  (equal (process-coding-system p) '(utf-8-unix . utf-8-unix))
                  (process-datagram-address p)
                  (process-inherit-coding-system-flag p)
                  (process-kill-buffer-query-function)
                  (stringp (process-tty-name p))
                  (stringp (process-tty-name p 'stdin))
                  (stringp (process-tty-name p 'stdout))
                  (stringp (process-tty-name p 'stderr))
                  (condition-case err (process-tty-name p 0) (error err))
                  (let ((pp (make-pipe-process :name "proc-coding-tty-query-pipe")))
                    (unwind-protect
                        (list
                         (null (process-tty-name pp))
                         (null (process-tty-name pp nil))
                         (null (process-tty-name pp 'stdin))
                         (null (process-tty-name pp 'stdout))
                         (null (process-tty-name pp 'stderr)))
                      (ignore-errors (delete-process pp))))
                  (let ((np (make-network-process :name "proc-coding-tty-query-network" :server t :service 0)))
                    (unwind-protect
                        (list
                         (null (process-tty-name np))
                         (null (process-tty-name np nil))
                         (null (process-tty-name np 'stdin))
                         (null (process-tty-name np 'stdout))
                         (null (process-tty-name np 'stderr)))
                      (ignore-errors (delete-process np))))
                  (delete-process p)
                  (process-live-p p))
               (ignore-errors (delete-process p))))
           (condition-case err (process-coding-system 'x) (error err))
           (condition-case err (process-datagram-address 'x) (error err))
           (condition-case err (process-inherit-coding-system-flag 'x) (error err))
           (condition-case err (process-tty-name 'x) (error err))
           (condition-case err (process-tty-name nil) (error err))
           (condition-case err (process-tty-name 'x t) (error err))
           (condition-case err (process-kill-buffer-query-function nil) (error (car err)))
           (condition-case err (process-coding-system) (error (car err)))
           (condition-case err (process-datagram-address) (error (car err)))
           (condition-case err (process-inherit-coding-system-flag) (error (car err)))
           (condition-case err (process-tty-name) (error (car err)))"#,
    ));
    assert_eq!(
        results[0],
        "OK (t nil nil t t t t t (error \"Unknown stream\" 0) (t t t t t) (t t t t t) nil nil)"
    );
    assert_eq!(results[1], "OK (wrong-type-argument processp x)");
    assert_eq!(results[2], "OK (wrong-type-argument processp x)");
    assert_eq!(results[3], "OK (wrong-type-argument processp x)");
    assert_eq!(results[4], "OK (wrong-type-argument processp x)");
    assert_eq!(results[5], "OK (wrong-type-argument processp nil)");
    assert_eq!(results[6], "OK (wrong-type-argument processp x)");
    assert_eq!(results[7], "OK wrong-number-of-arguments");
    assert_eq!(results[8], "OK wrong-number-of-arguments");
    assert_eq!(results[9], "OK wrong-number-of-arguments");
    assert_eq!(results[10], "OK wrong-number-of-arguments");
    assert_eq!(results[11], "OK wrong-number-of-arguments");
}

#[test]
fn process_list_network_serial_runtime_surface() {
    let results = bootstrap_eval_all(
        r#"(mapcar (lambda (s)
                     (list s
                           (fboundp s)
                           (subrp (symbol-function s))
                           (subr-arity (symbol-function s))
                           (commandp s)))
                   '(list-system-processes
                     num-processors
                     make-network-process
                     make-pipe-process
                     make-serial-process
                     serial-process-configure
                     set-network-process-option))
           (let ((n0 (num-processors))
                 (n1 (num-processors t)))
             (list
              (listp (list-system-processes))
              (integerp (car (list-system-processes)))
              (not (null (member (emacs-pid) (list-system-processes))))
              (condition-case err (list-system-processes nil) (error (car err)))
              (integerp n0)
              (integerp n1)
              (> n0 0)
              (= n0 n1)
              (condition-case err (num-processors 1 2) (error (car err)))
              (list-processes)
              (list-processes nil)
              (list-processes t)
              (list-processes nil nil)
              (list-processes nil t)
              (condition-case err (list-processes nil nil nil) (error (car err)))
              (listp (list-processes--refresh))
              (equal (car (list-processes--refresh)) "")
              (condition-case err (list-processes--refresh nil) (error (car err)))))
           (list
            (make-network-process)
            (condition-case err (make-network-process :name "np") (error err))
            (condition-case err (make-network-process :name 1) (error err))
            (condition-case err (make-network-process :service 80) (error err))
            (let ((p (make-network-process :name "np-server" :server t :service 0)))
              (unwind-protect
                  (processp p)
                (ignore-errors (delete-process p))))
            (make-pipe-process)
            (let ((p (make-pipe-process :name "pp")))
              (unwind-protect
                  (processp p)
                (ignore-errors (delete-process p))))
            (condition-case err (make-pipe-process :name 1) (error err))
            (make-serial-process)
            (condition-case err (make-serial-process :name "sp" :port t :speed 9600) (error err))
            (condition-case err (make-serial-process :name "sp" :port 1 :speed 9600) (error err))
            (condition-case err (make-serial-process :name "sp") (error err))
            (condition-case err (make-serial-process :name "sp" :port "/tmp/no-port") (error err))
            (with-temp-buffer
              (condition-case err (serial-process-configure) (error (car err))))
            (with-temp-buffer
              (let ((p (start-process "serial-cfg-proc" nil "cat")))
                (unwind-protect
                    (condition-case err (serial-process-configure p) (error (car err)))
                  (ignore-errors (delete-process p)))))
            (condition-case err (set-network-process-option) (error (car err)))
            (condition-case err (set-network-process-option 1 :foo 1) (error err))
            (let ((p (start-process "netopt-real" nil "cat")))
              (unwind-protect
                  (condition-case err (set-network-process-option p :foo 1) (error err))
                (ignore-errors (delete-process p))))
            (let ((p (make-network-process :name "netopt-network" :server t :service 0)))
              (unwind-protect
                  (condition-case err (set-network-process-option p :foo 1) (error err))
                (ignore-errors (delete-process p)))))"#,
    );

    assert_eq!(
        results[0],
        "OK ((list-system-processes t t (0 . 0) nil) (num-processors t t (0 . 1) nil) (make-network-process t t (0 . many) nil) (make-pipe-process t t (0 . many) nil) (make-serial-process t t (0 . many) nil) (serial-process-configure t t (0 . many) nil) (set-network-process-option t t (3 . 4) nil))"
    );
    assert_eq!(
        results[1],
        "OK (t t t wrong-number-of-arguments t t t t wrong-number-of-arguments nil nil nil nil nil wrong-number-of-arguments t t wrong-number-of-arguments)"
    );
    assert_eq!(
        results[2],
        "OK (nil (wrong-type-argument stringp nil) (error \":name value not a string\") (error \"Missing :name keyword parameter\") t nil t (error \":name value not a string\") nil (wrong-type-argument stringp t) (wrong-type-argument stringp 1) (error \"No port specified\") (error \":speed not specified\") error error wrong-number-of-arguments (wrong-type-argument processp 1) (error \"Process is not a network process\") (error \"Unknown or unsupported option\"))"
    );
}

#[test]
fn list_processes_refresh_returns_propertized_spacer() {
    let result = bootstrap_eval_one(r#"(list-processes--refresh)"#);
    assert_eq!(
        result,
        r##"OK ("" header-line-indent #(" " 0 1 (display (space :align-to (+ header-line-indent-width 0)))))"##
    );
}

#[test]
fn minibuffer_sort_preprocess_history_sequence_contract() {
    let results = eval_all(
        r#"(minibuffer--sort-preprocess-history nil)
           (minibuffer--sort-preprocess-history "")
           (minibuffer--sort-preprocess-history [97])
           (minibuffer--sort-preprocess-history '(97))
           (condition-case err (minibuffer--sort-preprocess-history 1) (error err))
           (condition-case err (minibuffer--sort-preprocess-history) (error err))"#,
    );

    assert_eq!(results[0], "OK nil");
    assert_eq!(results[1], "OK nil");
    assert_eq!(results[2], "OK nil");
    assert_eq!(results[3], "OK nil");
    assert_eq!(results[4], "OK (wrong-type-argument sequencep 1)");
    assert_eq!(
        results[5],
        "OK (wrong-number-of-arguments minibuffer--sort-preprocess-history 0)"
    );
}

#[test]
fn window_adjust_process_window_size_requires_list_window() {
    let results = eval_all(
        r#"(condition-case err (window-adjust-process-window-size 1 2) (error err))
           (condition-case err (window-adjust-process-window-size-largest 1 2) (error err))
           (condition-case err (window-adjust-process-window-size-smallest 1 2) (error err))
           (window-adjust-process-window-size nil nil)
           (window-adjust-process-window-size-largest nil nil)
           (window-adjust-process-window-size-smallest nil nil)"#,
    );

    assert_eq!(results[0], "OK (wrong-type-argument listp 2)");
    assert_eq!(results[1], "OK (wrong-type-argument listp 2)");
    assert_eq!(results[2], "OK (wrong-type-argument listp 2)");
    assert_eq!(results[3], "OK nil");
    assert_eq!(results[4], "OK nil");
    assert_eq!(results[5], "OK nil");
}

#[test]
fn network_interface_broadcast_derivation_helpers() {
    let ipv4_address = int_vector(&[192, 168, 1, 30, 0]);
    let ipv4_netmask = int_vector(&[255, 255, 255, 0, 0]);
    let ipv4_raw = int_vector(&[0, 0, 0, 0, 0]);
    assert_eq!(
        derive_network_interface_list_broadcast(
            NetworkAddressFamily::Ipv4,
            &ipv4_address,
            &ipv4_netmask,
            &ipv4_raw,
        ),
        int_vector(&[192, 168, 1, 255, 0])
    );
    assert_eq!(
        derive_network_interface_info_broadcast(
            NetworkAddressFamily::Ipv4,
            &ipv4_address,
            &ipv4_address,
        ),
        int_vector(&[0, 0, 0, 0, 0])
    );
    let ipv4_nontrivial_raw = int_vector(&[172, 17, 255, 255, 0]);
    assert_eq!(
        derive_network_interface_info_broadcast(
            NetworkAddressFamily::Ipv4,
            &int_vector(&[172, 17, 0, 1, 0]),
            &ipv4_nontrivial_raw,
        ),
        ipv4_nontrivial_raw
    );

    let ipv6_address = int_vector(&[9224, 33287, 9568, 22592, 60060, 9727, 65190, 14566, 0]);
    let ipv6_netmask = int_vector(&[65535, 65535, 65535, 65535, 0, 0, 0, 0, 0]);
    assert_eq!(
        derive_network_interface_list_broadcast(
            NetworkAddressFamily::Ipv6,
            &ipv6_address,
            &ipv6_netmask,
            &int_vector(&[0, 0, 0, 0, 0, 0, 0, 0, 0]),
        ),
        int_vector(&[9224, 33287, 9568, 22592, 65535, 65535, 65535, 65535, 0])
    );
}

#[test]
fn network_lookup_literal_family_filtering_helpers() {
    let loopback_v4 = int_vector(&[127, 0, 0, 1, 0]);
    let loopback_v6 = int_vector(&[0, 0, 0, 0, 0, 0, 0, 1, 0]);

    let v4_any = resolve_network_lookup_addresses("127.0.0.1", None);
    let v4_only = resolve_network_lookup_addresses("127.0.0.1", Some(NetworkAddressFamily::Ipv4));
    let v4_rejected =
        resolve_network_lookup_addresses("127.0.0.1", Some(NetworkAddressFamily::Ipv6));
    assert!(!v4_any.is_empty());
    assert_eq!(v4_any, v4_only);
    assert_eq!(v4_any[0], loopback_v4);
    assert!(v4_rejected.is_empty());

    let v6_any = resolve_network_lookup_addresses("::1", None);
    let v6_only = resolve_network_lookup_addresses("::1", Some(NetworkAddressFamily::Ipv6));
    let v6_rejected = resolve_network_lookup_addresses("::1", Some(NetworkAddressFamily::Ipv4));
    assert_eq!(v6_any, v6_only);
    if let Some(first) = v6_any.first() {
        assert_eq!(first, &loopback_v6);
    }
    assert!(v6_rejected.is_empty());
}

#[test]
fn network_lookup_embedded_nul_normalizes_like_c_strings() {
    let plain = resolve_network_lookup_addresses("abc", None);
    let embedded_nul = resolve_network_lookup_addresses("abc\0def", None);
    assert_eq!(embedded_nul, plain);

    let empty = resolve_network_lookup_addresses("", None);
    let nul_only = resolve_network_lookup_addresses("\0", None);
    assert_eq!(nul_only, empty);
}

#[test]
fn process_network_interface_and_signal_runtime_surface() {
    let results = eval_all(
        r#"(mapcar (lambda (s)
                     (let ((fn (and (fboundp s) (symbol-function s))))
                       (list s
                             (fboundp s)
                             (and fn (subrp fn))
                             (and fn (subr-arity fn))
                             (commandp s))))
                   '(process-connection
                     format-network-address
                     network-interface-list
                     network-interface-info
                     network-lookup-address-info
                     signal-names))
           (let* ((ifname (or (and (fboundp 'network-interface-list)
                                   (stringp (car (car (network-interface-list))))
                                   (car (car (network-interface-list))))
                              "lo")))
             (list
              (format-network-address [127 0 0 1 80])
              (format-network-address [127 0 0 1 80] t)
              (format-network-address [0 0 0 0 0 0 0 1 80])
              (format-network-address [0 0 0 0 0 0 0 1 80] t)
              (format-network-address "x")
              (format-network-address nil)
              (format-network-address [1])
              (format-network-address [127 0 0 1 65536])
              (format-network-address [0 0 0 0 0 0 0 1 65536])
              (condition-case err (format-network-address) (error err))
              (listp (network-interface-list))
              (consp (car (network-interface-list)))
              (stringp (car (car (network-interface-list))))
              (vectorp (cdr (car (network-interface-list))))
              (listp (network-interface-list nil))
              (let ((entry (car (network-interface-list t))))
                (and (listp entry)
                     (= (length entry) 4)
                     (vectorp (nth 1 entry))
                     (vectorp (nth 2 entry))
                     (vectorp (nth 3 entry))))
              (let* ((entries (network-interface-list t))
                     (ok t))
                (while (and ok entries)
                  (let* ((entry (car entries))
                         (addr (nth 1 entry))
                         (bc (nth 2 entry))
                         (mask (nth 3 entry))
                         (len (length addr))
                         (limit (if (= len 5) 4 8))
                         (bits-mask (if (= len 5) #xff #xffff))
                         (idx 0)
                         (vals nil))
                    (while (< idx limit)
                      (setq vals
                            (append vals
                                    (list (logand bits-mask
                                                  (logior (aref addr idx)
                                                          (lognot (aref mask idx)))))))
                      (setq idx (1+ idx)))
                    (setq vals (append vals '(0)))
                    (setq ok (equal bc (apply #'vector vals))))
                  (setq entries (cdr entries)))
                ok)
              (condition-case err (network-interface-list nil nil nil) (error err))
              (condition-case err (network-interface-list nil t) (error err))
              (let* ((entries (network-interface-list t 'ipv4))
                     (ok t))
                (while (and ok entries)
                  (let* ((entry (car entries))
                         (addr (nth 1 entry)))
                    (setq ok (and (vectorp addr) (= (length addr) 5))))
                  (setq entries (cdr entries)))
                ok)
              (let* ((entries (network-interface-list t 'ipv6))
                     (ok t))
                (while (and ok entries)
                  (let* ((entry (car entries))
                         (addr (nth 1 entry)))
                    (setq ok (and (vectorp addr) (= (length addr) 9))))
                  (setq entries (cdr entries)))
                ok)
              (let* ((entries (network-interface-list nil 'ipv4))
                     (ok t))
                (while (and ok entries)
                  (let* ((entry (car entries))
                         (addr (cdr entry)))
                    (setq ok (and (vectorp addr) (= (length addr) 5))))
                  (setq entries (cdr entries)))
                ok)
              (let* ((entries (network-interface-list nil 'ipv6))
                     (ok t))
                (while (and ok entries)
                  (let* ((entry (car entries))
                         (addr (cdr entry)))
                    (setq ok (and (vectorp addr) (= (length addr) 9))))
                  (setq entries (cdr entries)))
                ok)
              (let ((info (network-interface-info ifname)))
                (and (listp info)
                     (= (length info) 5)
                     (vectorp (car info))
                     (vectorp (nth 1 info))
                     (vectorp (nth 2 info))
                     (or (null (nth 3 info))
                         (consp (nth 3 info)))
                     (listp (nth 4 info))))
              (let ((lo-info (network-interface-info "lo")))
                (and (listp lo-info)
                     (= (length lo-info) 5)
                     (vectorp (car lo-info))
                     (vectorp (nth 1 lo-info))
                     (vectorp (nth 2 lo-info))))
              (let* ((ifname (car (car (network-interface-list nil 'ipv4))))
                     (info (and ifname (network-interface-info ifname)))
                     (entries (network-interface-list nil 'ipv4))
                     (found nil))
                (while entries
                  (let ((entry (car entries)))
                    (if (and (equal (car entry) ifname)
                             (equal (cdr entry) (car info)))
                        (setq found t)))
                  (setq entries (cdr entries)))
                (or (null ifname) found))
              (let* ((info (network-interface-info ifname))
                     (addr (car info))
                     (bc (nth 1 info))
                     (mask (nth 2 info))
                     (len (length addr)))
                (and (or (= len 5) (= len 9))
                     (= (length bc) len)
                     (= (length mask) len)))
              (let* ((lo-info (network-interface-info "lo"))
                     (addr (car lo-info))
                     (bc (nth 1 lo-info))
                     (mask (nth 2 lo-info)))
                (and (= (length addr) (length bc))
                     (= (length addr) (length mask))))
              (equal (network-interface-info (concat "lo" (string 0) "x"))
                     (network-interface-info "lo"))
              (condition-case err (network-interface-info nil) (error err))
              (condition-case err (network-interface-info "abcdefghijklmnop") (error err))
              (condition-case err (network-interface-info (concat "abcdefghijklmnop" (string 0))) (error err))
              (condition-case err (network-interface-info (concat "aaaaaaaaaaaaaa" (string 233))) (error err))
              (null (network-interface-info (concat "aaaaaaaaaaaaa" (string 233))))
              (listp (network-lookup-address-info "localhost"))
              (vectorp (car (network-lookup-address-info "localhost")))
              (listp (network-lookup-address-info "localhost" 'ipv4))
              (vectorp (car (network-lookup-address-info "localhost" 'ipv6)))
              (let* ((v4-any (network-lookup-address-info "127.0.0.1"))
                     (v4-only (network-lookup-address-info "127.0.0.1" 'ipv4)))
                (and (equal v4-any v4-only)
                     (consp v4-only)
                     (equal (car v4-only) [127 0 0 1 0])))
              (null (network-lookup-address-info "127.0.0.1" 'ipv6))
              (let* ((v6-any (network-lookup-address-info "::1"))
                     (v6-only (network-lookup-address-info "::1" 'ipv6)))
                (and (equal v6-any v6-only)
                     (or (null v6-only)
                         (equal (car v6-only) [0 0 0 0 0 0 0 1 0]))))
              (null (network-lookup-address-info "::1" 'ipv4))
              (let* ((entries (network-lookup-address-info "localhost" 'ipv4))
                     (ok t))
                (while (and ok entries)
                  (setq ok (= (length (car entries)) 5))
                  (setq entries (cdr entries)))
                ok)
              (let* ((entries (network-lookup-address-info "localhost" 'ipv6))
                     (ok t))
                (while (and ok entries)
                  (setq ok (= (length (car entries)) 9))
                  (setq entries (cdr entries)))
                ok)
              (equal (network-lookup-address-info (concat "abc" (string 0) "def"))
                     (network-lookup-address-info "abc"))
              (equal (network-lookup-address-info (string 0))
                     (network-lookup-address-info ""))
              (condition-case err (network-lookup-address-info "localhost" t) (error err))
              (condition-case err (network-lookup-address-info "localhost" 'ipv4 t) (error err))
              (condition-case err (network-lookup-address-info 1) (error err))
              (listp (signal-names))
              (stringp (car (signal-names)))
              (not (null (member "KILL" (signal-names))))
              (condition-case err (signal-names nil) (error err))
              (condition-case err (process-connection nil) (error err))))"#,
    );

    assert_eq!(
        results[0],
        "OK ((process-connection nil nil nil nil) (format-network-address t t (1 . 2) nil) (network-interface-list t t (0 . 2) nil) (network-interface-info t t (1 . 1) nil) (network-lookup-address-info t t (1 . 3) nil) (signal-names t t (0 . 0) nil))"
    );
    assert_eq!(
        results[1],
        "OK (\"127.0.0.1:80\" \"127.0.0.1\" \"[0:0:0:0:0:0:0:1]:80\" \"0:0:0:0:0:0:0:1\" \"x\" nil nil nil nil (wrong-number-of-arguments format-network-address 0) t t t t t t t (wrong-number-of-arguments network-interface-list 3) (error \"Unsupported address family\") t t t t t t t t t t (wrong-type-argument stringp nil) (error \"interface name too long\") (error \"interface name too long\") (error \"interface name too long\") t t t t t t t t t t t t t (error \"Unsupported family\") (error \"Unsupported hints value\") (wrong-type-argument stringp 1) t t t (wrong-number-of-arguments signal-names 1) (void-function process-connection))"
    );
}
