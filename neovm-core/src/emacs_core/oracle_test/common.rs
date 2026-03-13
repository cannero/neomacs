//! Shared oracle helpers for Elisp unit tests.
//!
//! These helpers are intentionally test-only and require GNU Emacs
//! available on PATH (or via `NEOVM_FORCE_ORACLE_PATH`).

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use crate::emacs_core::{EvalError, Evaluator, Value, print_value_with_buffers};

/// Maximum virtual address space (in bytes) for each spawned oracle Emacs
/// process.  This prevents runaway evaluations from consuming unbounded
/// memory and triggering the system OOM killer.
/// Overridable via `NEOVM_ORACLE_MEM_LIMIT_MB` (default: 500 MB).
fn oracle_mem_limit_bytes() -> u64 {
    let mb: u64 = std::env::var("NEOVM_ORACLE_MEM_LIMIT_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500);
    mb * 1024 * 1024
}

/// Optional virtual address space cap (in bytes) for the NeoVM side of an
/// oracle test process. This is unset by default because NeoVM runs in-process
/// inside the test binary; when enabled, nextest's per-test process isolation
/// keeps the limit scoped to the current test.
///
/// Set `NEOVM_NEOVM_MEM_LIMIT_MB` to enable it.
fn neovm_mem_limit_bytes() -> Option<u64> {
    let mb: u64 = std::env::var("NEOVM_NEOVM_MEM_LIMIT_MB")
        .ok()
        .and_then(|v| v.parse().ok())?;
    Some(mb * 1024 * 1024)
}

fn apply_address_space_limit(limit_bytes: u64) -> Result<(), String> {
    unsafe {
        let mut current = std::mem::MaybeUninit::<libc::rlimit>::uninit();
        if libc::getrlimit(libc::RLIMIT_AS, current.as_mut_ptr()) != 0 {
            return Err(format!(
                "failed to read RLIMIT_AS: {}",
                std::io::Error::last_os_error()
            ));
        }
        let current = current.assume_init();
        let target = if current.rlim_max == libc::RLIM_INFINITY {
            limit_bytes as libc::rlim_t
        } else {
            std::cmp::min(limit_bytes as libc::rlim_t, current.rlim_max)
        };
        let rlim = libc::rlimit {
            rlim_cur: target,
            rlim_max: target,
        };
        if libc::setrlimit(libc::RLIMIT_AS, &rlim) != 0 {
            return Err(format!(
                "failed to set RLIMIT_AS to {} MB: {}",
                limit_bytes / (1024 * 1024),
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

fn ensure_neovm_mem_limit() -> Result<(), String> {
    static APPLY_RESULT: OnceLock<Result<(), String>> = OnceLock::new();

    let Some(limit_bytes) = neovm_mem_limit_bytes() else {
        return Ok(());
    };

    APPLY_RESULT
        .get_or_init(|| apply_address_space_limit(limit_bytes))
        .clone()
}

pub(crate) const ORACLE_PROP_CASES: u32 = 10;

pub(crate) fn oracle_prop_enabled() -> bool {
    std::env::var_os("NEOVM_FORCE_ORACLE_PATH").is_some()
}

fn oracle_timing_enabled() -> bool {
    std::env::var_os("NEOVM_ORACLE_TIMING").is_some()
}

fn ensure_oracle_timing_tracing() {
    static INIT: OnceLock<()> = OnceLock::new();
    if !oracle_timing_enabled() {
        return;
    }
    INIT.get_or_init(|| {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_test_writer()
            .try_init();
    });
}

macro_rules! return_if_neovm_enable_oracle_proptest_not_set {
    () => {
        if !$crate::emacs_core::oracle_test::common::oracle_prop_enabled() {
            tracing::info!(
                "skipping {}:{}: set NEOVM_FORCE_ORACLE_PATH=/path/to/emacs",
                module_path!(),
                line!()
            );
            return;
        }
    };
    ($ret:expr) => {
        if !$crate::emacs_core::oracle_test::common::oracle_prop_enabled() {
            tracing::info!(
                "skipping {}:{}: set NEOVM_FORCE_ORACLE_PATH=/path/to/emacs",
                module_path!(),
                line!()
            );
            return $ret;
        }
    };
}

pub(crate) use return_if_neovm_enable_oracle_proptest_not_set;

fn oracle_emacs_path() -> String {
    std::env::var("NEOVM_FORCE_ORACLE_PATH").unwrap_or_else(|_| "emacs".to_string())
}

fn write_temp_elisp_file(
    prefix: &str,
    suffix: &str,
    content: &str,
) -> Result<tempfile::TempPath, String> {
    let mut file = tempfile::Builder::new()
        .prefix(prefix)
        .suffix(suffix)
        .tempfile()
        .map_err(|e| format!("failed to create oracle form file: {e}"))?;
    file.write_all(content.as_bytes())
        .map_err(|e| format!("failed to write oracle form file: {e}"))?;
    file.flush()
        .map_err(|e| format!("failed to flush oracle form file: {e}"))?;
    Ok(file.into_temp_path())
}

fn write_oracle_form_file(form: &str) -> Result<tempfile::TempPath, String> {
    write_temp_elisp_file("neovm-oracle-form-", ".el", form)
}

fn project_lisp_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().expect("project root").join("lisp")
}

fn project_lisp_subdirs() -> &'static [&'static str] {
    &[
        "",
        "emacs-lisp",
        "progmodes",
        "language",
        "international",
        "textmodes",
        "vc",
        "leim",
    ]
}

fn ensure_nonempty_form(form: &str) -> Result<(), String> {
    if form.trim().is_empty() {
        Err("no form parsed".to_string())
    } else {
        Ok(())
    }
}

fn run_oracle_eval_inner(form: &str, load_files: &[&str]) -> Result<String, String> {
    let form_path = write_oracle_form_file(form)?;
    let program = r#"(condition-case err
    (progn
      (defun neovm--oracle-normalize (v)
        (cond
         ((and (functionp v) (eq (type-of v) 'interpreted-function))
          (let ((args (aref v 0))
                (body (aref v 1))
                (env (aref v 2)))
            (if (null env)
                (cons 'lambda (cons args body))
              (cons 'closure (cons env (cons args body))))))
         ((consp v)
          (cons (neovm--oracle-normalize (car v))
                (neovm--oracle-normalize (cdr v))))
         ((vectorp v)
          (apply #'vector (mapcar #'neovm--oracle-normalize (append v nil))))
         (t v)))
    (let* ((coding-system-for-read 'utf-8-unix)
           (coding-system-for-write 'utf-8-unix)
           (_ (set-language-environment "UTF-8"))
           (load-root (getenv "NEOVM_ORACLE_LOAD_ROOT"))
           (load-files (split-string (or (getenv "NEOVM_ORACLE_LOAD_FILES") "") "\n" t))
           (form-file (getenv "NEOVM_ORACLE_FORM_FILE"))
           (result
            (let ((source-buf (generate-new-buffer " *neovm-oracle-form*")))
              (unwind-protect
                  (progn
                    (when load-root
                      (let ((extra-load-path nil))
                        (dolist (sub '("" "emacs-lisp" "progmodes" "language"
                                       "international" "textmodes" "vc" "leim"))
                          (let ((dir (if (equal sub "")
                                         load-root
                                       (expand-file-name sub load-root))))
                            (when (file-directory-p dir)
                              (push dir extra-load-path))))
                        (setq load-path (append (nreverse extra-load-path) load-path))))
                    (dolist (file load-files)
                      (load file nil t nil t))
                    (with-current-buffer source-buf
                      (insert-file-contents form-file)
                      (goto-char (point-min)))
                    (let ((last nil))
                      (condition-case nil
                          (while t
                            (setq last (eval (read source-buf) t)))
                        (end-of-file last))))
                (when (buffer-live-p source-buf)
                  (kill-buffer source-buf))))))
      (princ (concat "OK " (prin1-to-string (neovm--oracle-normalize result))))))
  (error
   (princ
    (concat "ERR "
            (prin1-to-string
             (neovm--oracle-normalize (cons (car err) (cdr err))))))))"#;
    let oracle_bin = oracle_emacs_path();
    let lisp_dir = project_lisp_dir();
    let oracle_load_files = load_files
        .iter()
        .map(|file| lisp_dir.join(file).to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n");

    let mem_limit = oracle_mem_limit_bytes();
    let mut cmd = Command::new(&oracle_bin);
    cmd.env("NEOVM_ORACLE_FORM_FILE", form_path.as_os_str())
        .env("NEOVM_ORACLE_LOAD_ROOT", &lisp_dir)
        .env("NEOVM_ORACLE_LOAD_FILES", oracle_load_files)
        .env("EMACSNATIVELOADPATH", "/dev/null")
        .args([
            "--batch",
            "-Q",
            "--eval",
            "(setq native-comp-jit-compilation nil inhibit-automatic-native-compilation t native-comp-enable-subr-trampolines nil)",
            "--eval",
            &program,
        ]);

    // Safety: `pre_exec` runs between fork and exec in the child process.
    // We only call `setrlimit` which is async-signal-safe.
    unsafe {
        cmd.pre_exec(move || {
            let rlim = libc::rlimit {
                rlim_cur: mem_limit as libc::rlim_t,
                rlim_max: mem_limit as libc::rlim_t,
            };
            if libc::setrlimit(libc::RLIMIT_AS, &rlim) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let output = cmd
        .output()
        .map_err(|e| format!("failed to run oracle Emacs: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "oracle Emacs failed: status={}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_oracle_eval_inner_raw(form: &str, load_files: &[&str]) -> Result<String, String> {
    let form_path = write_oracle_form_file(form)?;
    let program = r#"(condition-case err
    (progn
      (let* ((coding-system-for-read 'utf-8-unix)
             (coding-system-for-write 'utf-8-unix)
             (_ (set-language-environment "UTF-8"))
             (load-root (getenv "NEOVM_ORACLE_LOAD_ROOT"))
             (load-files (split-string (or (getenv "NEOVM_ORACLE_LOAD_FILES") "") "\n" t))
             (form-file (getenv "NEOVM_ORACLE_FORM_FILE"))
             (result
              (let ((source-buf (generate-new-buffer " *neovm-oracle-form*")))
                (unwind-protect
                    (progn
                      (when load-root
                        (let ((extra-load-path nil))
                          (dolist (sub '("" "emacs-lisp" "progmodes" "language"
                                         "international" "textmodes" "vc" "leim"))
                            (let ((dir (if (equal sub "")
                                           load-root
                                         (expand-file-name sub load-root))))
                              (when (file-directory-p dir)
                                (push dir extra-load-path))))
                          (setq load-path (append (nreverse extra-load-path) load-path))))
                      (dolist (file load-files)
                        (load file nil t nil t))
                      (with-current-buffer source-buf
                        (insert-file-contents form-file)
                        (goto-char (point-min)))
                      (let ((last nil))
                        (condition-case nil
                            (while t
                              (setq last (eval (read source-buf) t)))
                          (end-of-file last))))
                  (when (buffer-live-p source-buf)
                    (kill-buffer source-buf))))))
        (princ (concat "OK " (prin1-to-string result)))))
  (error
   (princ (concat "ERR " (prin1-to-string err)))))"#;
    let oracle_bin = oracle_emacs_path();
    let lisp_dir = project_lisp_dir();
    let oracle_load_files = load_files
        .iter()
        .map(|file| lisp_dir.join(file).to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n");

    let mem_limit = oracle_mem_limit_bytes();
    let mut cmd = Command::new(&oracle_bin);
    cmd.env("NEOVM_ORACLE_FORM_FILE", form_path.as_os_str())
        .env("NEOVM_ORACLE_LOAD_ROOT", &lisp_dir)
        .env("NEOVM_ORACLE_LOAD_FILES", oracle_load_files)
        .env("EMACSNATIVELOADPATH", "/dev/null")
        .args([
            "--batch",
            "-Q",
            "--eval",
            "(setq native-comp-jit-compilation nil inhibit-automatic-native-compilation t native-comp-enable-subr-trampolines nil)",
            "--eval",
            &program,
        ]);

    unsafe {
        cmd.pre_exec(move || {
            let rlim = libc::rlimit {
                rlim_cur: mem_limit as libc::rlim_t,
                rlim_max: mem_limit as libc::rlim_t,
            };
            if libc::setrlimit(libc::RLIMIT_AS, &rlim) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let output = cmd
        .output()
        .map_err(|e| format!("failed to run oracle Emacs: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "oracle Emacs failed: status={}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(crate) fn run_oracle_eval(form: &str) -> Result<String, String> {
    run_oracle_eval_inner(form, &[])
}

pub(crate) fn run_oracle_eval_with_load(form: &str, load_files: &[&str]) -> Result<String, String> {
    run_oracle_eval_inner(form, load_files)
}

pub(crate) fn run_oracle_eval_with_load_raw(
    form: &str,
    load_files: &[&str],
) -> Result<String, String> {
    run_oracle_eval_inner_raw(form, load_files)
}

pub(crate) fn run_oracle_eval_with_bootstrap(form: &str) -> Result<String, String> {
    run_oracle_eval(form)
}

/// Run a NeoVM evaluation in a bare-core evaluator.
///
/// This intentionally does **not** load the bootstrapped runtime image.
/// Use it for core-language parity only.  Features that GNU Emacs exposes
/// from dumped startup state (for example `backquote`) must use
/// `run_neovm_eval_with_bootstrap`.
pub(crate) fn run_neovm_eval(form: &str) -> Result<String, String> {
    run_neovm_eval_with_load(form, &[])
}

fn run_neovm_eval_in_temp_buffer(
    eval: &mut Evaluator,
    form: &str,
) -> Result<Result<Value, EvalError>, String> {
    let saved_buf = eval.buffers.current_buffer().map(|b| b.id);
    let temp_name = eval
        .buffers
        .generate_new_buffer_name(" *neovm-oracle-form*");
    let temp_id = eval.buffers.create_buffer(&temp_name);

    {
        let Some(buf) = eval.buffers.get_mut(temp_id) else {
            return Err("failed to create temp buffer".to_string());
        };
        buf.insert(form);
        buf.pt = 0;
    }

    let mut result = Ok(Value::Nil);
    loop {
        match crate::emacs_core::reader::builtin_read(eval, vec![Value::Buffer(temp_id)]) {
            Ok(read_form) => {
                result = eval
                    .eval_value(&read_form)
                    .map_err(crate::emacs_core::error::map_flow);
                if result.is_err() {
                    break;
                }
            }
            Err(crate::emacs_core::error::Flow::Signal(sig))
                if sig.symbol_name() == "end-of-file" =>
            {
                break;
            }
            Err(flow) => {
                result = Err(crate::emacs_core::error::map_flow(flow));
                break;
            }
        }
    }

    let killed = eval.buffers.kill_buffer(temp_id);
    debug_assert!(killed, "temp oracle buffer should be killable");
    if let Some(saved_id) = saved_buf {
        eval.buffers.set_current(saved_id);
    }

    Ok(result)
}

/// Run a NeoVM evaluation after pre-loading Elisp files.
///
/// `load_files` are paths relative to the project `lisp/` directory,
/// loaded in order.  The caller is responsible for listing dependencies
/// before dependents (e.g. `"emacs-lisp/oclosure.el"` before
/// `"emacs-lisp/nadvice.el"`).
pub(crate) fn run_neovm_eval_with_load(form: &str, load_files: &[&str]) -> Result<String, String> {
    ensure_neovm_mem_limit()?;
    let mut eval = Evaluator::new();
    // Match oracle's (eval form t): evaluate with lexical binding enabled.
    eval.set_lexical_binding(true);

    if !load_files.is_empty() {
        // Set up load-path from the project's lisp/ tree so that any
        // `require` calls inside the loaded files can find dependencies.
        let lisp_dir = project_lisp_dir();
        let mut load_path_entries = Vec::new();
        for sub in project_lisp_subdirs() {
            let dir = if sub.is_empty() {
                lisp_dir.clone()
            } else {
                lisp_dir.join(sub)
            };
            if dir.is_dir() {
                load_path_entries.push(Value::string(dir.to_string_lossy().to_string()));
            }
        }
        eval.set_variable("load-path", Value::list(load_path_entries));

        for file in load_files {
            let path = lisp_dir.join(file);
            eval.load_file_internal(&path)
                .map_err(|e| format!("failed to load '{}': {e:?}", path.display()))?;
        }
    }

    ensure_nonempty_form(form)?;

    let result = run_neovm_eval_in_temp_buffer(&mut eval, form)?;
    let rendered = render_neovm_oracle_result(&eval, result);
    Ok(rendered)
}

/// Compare GNU Emacs runtime against NeoVM bare-core evaluation.
///
/// Prefer `eval_oracle_and_neovm_with_bootstrap` when the form depends on
/// dumped runtime features rather than raw evaluator semantics.
pub(crate) fn eval_oracle_and_neovm(form: &str) -> (String, String) {
    // Keep oracle tests deterministic.  Running GNU Emacs and NeoVM in
    // parallel inside one test process exposes unrelated shared-state and
    // thread-local interactions; nextest already provides cross-test
    // parallelism, so per-test comparisons should stay sequential.
    let neovm = run_neovm_eval(form).expect("neovm eval should run");
    let oracle = run_oracle_eval(form).expect("oracle eval should run");
    (oracle, neovm)
}

pub(crate) fn eval_oracle_and_neovm_with_bootstrap(form: &str) -> (String, String) {
    let neovm = run_neovm_eval_with_bootstrap(form).expect("neovm eval should run");
    let oracle = run_oracle_eval_with_bootstrap(form).expect("oracle eval should run");
    (oracle, neovm)
}

pub(crate) fn assert_ok_eq(expected_payload: &str, oracle: &str, neovm: &str) {
    let expected = format!("OK {expected_payload}");
    assert_eq!(oracle, expected, "oracle should match expected payload");
    assert_eq!(neovm, expected, "neovm should match expected payload");
    assert_eq!(neovm, oracle, "neovm and oracle should match");
}

pub(crate) fn assert_oracle_parity_with_load(form: &str, load_files: &[&str]) {
    let neovm = run_neovm_eval_with_load(form, load_files).expect("neovm eval should run");
    let oracle = run_oracle_eval_with_load(form, load_files).expect("oracle eval should run");
    assert_eq!(neovm, oracle, "oracle parity mismatch for form: {form}");
}

pub(crate) fn run_neovm_eval_with_bootstrap_and_load(
    form: &str,
    load_files: &[&str],
) -> Result<String, String> {
    ensure_neovm_mem_limit()?;
    let mut eval = crate::emacs_core::load::create_bootstrap_evaluator_cached()
        .map_err(|e| format!("bootstrap failed: {e:?}"))?;
    crate::emacs_core::load::apply_runtime_startup_state(&mut eval)
        .map_err(|e| format!("startup state failed: {e:?}"))?;

    let lisp_dir = project_lisp_dir();
    for file in load_files {
        let path = lisp_dir.join(file);
        eval.load_file_internal(&path)
            .map_err(|e| format!("failed to load '{}': {e:?}", path.display()))?;
    }

    ensure_nonempty_form(form)?;

    let result = run_neovm_eval_in_temp_buffer(&mut eval, form)?;
    Ok(render_neovm_oracle_result(&eval, result))
}

pub(crate) fn assert_oracle_parity_with_bootstrap_and_load(form: &str, load_files: &[&str]) {
    let neovm =
        run_neovm_eval_with_bootstrap_and_load(form, load_files).expect("neovm eval should run");
    let oracle = run_oracle_eval_with_load(form, load_files).expect("oracle eval should run");
    assert_eq!(neovm, oracle, "oracle parity mismatch for form: {form}");
}

pub(crate) fn run_neovm_eval_with_bootstrap_and_load_raw(
    form: &str,
    load_files: &[&str],
) -> Result<String, String> {
    ensure_neovm_mem_limit()?;
    let mut eval = crate::emacs_core::load::create_bootstrap_evaluator_cached()
        .map_err(|e| format!("bootstrap failed: {e:?}"))?;
    crate::emacs_core::load::apply_runtime_startup_state(&mut eval)
        .map_err(|e| format!("startup state failed: {e:?}"))?;

    let lisp_dir = project_lisp_dir();
    for file in load_files {
        let path = lisp_dir.join(file);
        eval.load_file_internal(&path)
            .map_err(|e| format!("failed to load '{}': {e:?}", path.display()))?;
    }

    ensure_nonempty_form(form)?;

    let result = run_neovm_eval_in_temp_buffer(&mut eval, form)?;
    Ok(render_neovm_raw_oracle_result(&eval, result))
}

pub(crate) fn assert_oracle_parity_with_bootstrap_and_load_raw(form: &str, load_files: &[&str]) {
    let neovm = run_neovm_eval_with_bootstrap_and_load_raw(form, load_files)
        .expect("neovm eval should run");
    let oracle = run_oracle_eval_with_load_raw(form, load_files).expect("oracle eval should run");
    assert_eq!(neovm, oracle, "oracle parity mismatch for form: {form}");
}

/// Run a NeoVM evaluation using a fully bootstrapped evaluator.
/// Uses pdump cache when available for faster startup.
pub(crate) fn run_neovm_eval_with_bootstrap(form: &str) -> Result<String, String> {
    ensure_neovm_mem_limit()?;
    ensure_oracle_timing_tracing();
    let log_timing = oracle_timing_enabled();
    let bootstrap_t0 = std::time::Instant::now();
    let mut eval = crate::emacs_core::load::create_bootstrap_evaluator_cached()
        .map_err(|e| format!("bootstrap failed: {e:?}"))?;
    if log_timing {
        tracing::info!(
            "oracle-timing: neovm-bootstrap-cache {:.3?}",
            bootstrap_t0.elapsed()
        );
    }
    let startup_t0 = std::time::Instant::now();
    crate::emacs_core::load::apply_runtime_startup_state(&mut eval)
        .map_err(|e| format!("startup state failed: {e:?}"))?;
    if log_timing {
        tracing::info!(
            "oracle-timing: neovm-startup-state {:.3?}",
            startup_t0.elapsed()
        );
    }

    ensure_nonempty_form(form)?;

    crate::emacs_core::perf_trace::reset_hotpath_stats();
    let eval_t0 = std::time::Instant::now();
    let result = run_neovm_eval_in_temp_buffer(&mut eval, form)?;
    if log_timing {
        tracing::info!("oracle-timing: neovm-form-eval {:.3?}", eval_t0.elapsed());
        crate::emacs_core::perf_trace::log_hotpath_stats("oracle-hotpath");
    }

    let rendered = render_neovm_oracle_result(&eval, result);
    Ok(rendered)
}

pub(crate) fn assert_oracle_parity_with_bootstrap(form: &str) {
    let t0 = std::time::Instant::now();
    let log_timing = oracle_timing_enabled();
    ensure_oracle_timing_tracing();
    if log_timing {
        tracing::info!("oracle-timing: neovm-start");
    }
    let neovm_t0 = std::time::Instant::now();
    let neovm = run_neovm_eval_with_bootstrap(form).expect("neovm eval should run");
    if log_timing {
        tracing::info!("oracle-timing: neovm-done {:.3?}", neovm_t0.elapsed());
        tracing::info!("oracle-timing: oracle-start");
    }
    let oracle_t0 = std::time::Instant::now();
    let oracle = run_oracle_eval_with_bootstrap(form).expect("oracle eval should run");
    if log_timing {
        tracing::info!("oracle-timing: oracle-done {:.3?}", oracle_t0.elapsed());
    }
    let t1 = std::time::Instant::now();
    tracing::info!("total: {:.3?}", t1 - t0);
    assert_eq!(neovm, oracle, "oracle parity mismatch for form: {form}");
}

pub(crate) fn assert_err_kind(oracle: &str, neovm: &str, err_kind: &str) {
    assert!(
        oracle.starts_with("ERR "),
        "oracle should return an error: {oracle}"
    );
    assert!(
        neovm.starts_with("ERR "),
        "neovm should return an error: {neovm}"
    );

    let oracle_payload = oracle
        .strip_prefix("ERR ")
        .expect("oracle payload should have ERR prefix")
        .trim();
    let neovm_payload = neovm
        .strip_prefix("ERR ")
        .expect("neovm payload should have ERR prefix")
        .trim();

    assert!(
        !oracle_payload.is_empty(),
        "oracle error should include a message"
    );
    assert!(
        !neovm_payload.is_empty(),
        "neovm error should include a message"
    );
    assert!(
        oracle_payload.contains(err_kind),
        "oracle error kind should contain '{err_kind}': {oracle_payload}"
    );
    assert!(
        neovm_payload.contains(err_kind),
        "neovm error kind should contain '{err_kind}': {neovm_payload}"
    );
}

fn normalize_neovm_oracle_value(value: Value) -> Value {
    match value {
        Value::Lambda(_) => normalize_interpreted_function_for_oracle(value).unwrap_or(value),
        Value::Cons(cell) => {
            let pair = crate::emacs_core::value::read_cons(cell);
            let car = normalize_neovm_oracle_value(pair.car);
            crate::emacs_core::eval::push_scratch_gc_root(car);
            let cdr = normalize_neovm_oracle_value(pair.cdr);
            crate::emacs_core::eval::push_scratch_gc_root(cdr);
            let out = Value::cons(car, cdr);
            crate::emacs_core::eval::push_scratch_gc_root(out);
            out
        }
        Value::Vector(id) => {
            let items = crate::emacs_core::value::with_heap(|h| h.get_vector(id).clone());
            let mut normalized = Vec::with_capacity(items.len());
            for item in items {
                let item = normalize_neovm_oracle_value(item);
                crate::emacs_core::eval::push_scratch_gc_root(item);
                normalized.push(item);
            }
            let out = Value::vector(normalized);
            crate::emacs_core::eval::push_scratch_gc_root(out);
            out
        }
        _ => value,
    }
}

fn normalize_interpreted_function_for_oracle(value: Value) -> Option<Value> {
    let lambda = value.get_lambda_data()?.clone();
    let closure_vec = crate::emacs_core::builtins::lambda_to_closure_vector(&value);
    if closure_vec.len() < 3 {
        return None;
    }

    let args = normalize_neovm_oracle_value(closure_vec[0]);
    crate::emacs_core::eval::push_scratch_gc_root(args);

    let body_forms = crate::emacs_core::value::list_to_vec(&closure_vec[1]).unwrap_or_default();
    let mut elements = Vec::with_capacity(body_forms.len() + 3);

    if lambda.env.is_some() {
        elements.push(Value::symbol("closure"));
        let env = normalize_neovm_oracle_value(closure_vec[2]);
        crate::emacs_core::eval::push_scratch_gc_root(env);
        elements.push(env);
    } else {
        elements.push(Value::symbol("lambda"));
    }

    elements.push(args);
    for form in body_forms {
        let form = normalize_neovm_oracle_value(form);
        crate::emacs_core::eval::push_scratch_gc_root(form);
        elements.push(form);
    }

    let out = Value::list(elements);
    crate::emacs_core::eval::push_scratch_gc_root(out);
    Some(out)
}

fn render_neovm_oracle_result(eval: &Evaluator, result: Result<Value, EvalError>) -> String {
    let saved_roots = crate::emacs_core::eval::save_scratch_gc_roots();
    let rendered = match result {
        Ok(value) => {
            let value = normalize_neovm_oracle_value(value);
            format!("OK {}", print_value_with_buffers(&value, &eval.buffers))
        }
        Err(EvalError::Signal { symbol, data }) => {
            let mut values = Vec::with_capacity(data.len() + 1);
            values.push(Value::Symbol(symbol));
            values.extend(data);
            let payload = normalize_neovm_oracle_value(Value::list(values));
            format!("ERR {}", print_value_with_buffers(&payload, &eval.buffers))
        }
        Err(EvalError::UncaughtThrow { tag, value }) => {
            let tag = normalize_neovm_oracle_value(tag);
            let value = normalize_neovm_oracle_value(value);
            format!(
                "ERR (no-catch {} {})",
                print_value_with_buffers(&tag, &eval.buffers),
                print_value_with_buffers(&value, &eval.buffers),
            )
        }
    };
    crate::emacs_core::eval::restore_scratch_gc_roots(saved_roots);
    rendered
}

fn render_neovm_raw_oracle_result(eval: &Evaluator, result: Result<Value, EvalError>) -> String {
    match result {
        Ok(value) => format!("OK {}", print_value_with_buffers(&value, &eval.buffers)),
        Err(EvalError::Signal { symbol, data }) => {
            let mut values = Vec::with_capacity(data.len() + 1);
            values.push(Value::Symbol(symbol));
            values.extend(data);
            format!(
                "ERR {}",
                print_value_with_buffers(&Value::list(values), &eval.buffers)
            )
        }
        Err(EvalError::UncaughtThrow { tag, value }) => {
            format!(
                "ERR (no-catch {} {})",
                print_value_with_buffers(&tag, &eval.buffers),
                print_value_with_buffers(&value, &eval.buffers),
            )
        }
    }
}
