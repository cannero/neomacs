//! Shared oracle helpers for Elisp unit tests.
//!
//! These helpers are intentionally test-only and require GNU Emacs
//! available on PATH (or via `NEOVM_FORCE_ORACLE_PATH`).

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::emacs_core::{EvalError, Evaluator, Value, parse_forms, print_value};

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

pub(crate) const ORACLE_PROP_CASES: u32 = 10;

pub(crate) fn oracle_prop_enabled() -> bool {
    std::env::var_os("NEOVM_FORCE_ORACLE_PATH").is_some()
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

fn write_oracle_form_file(form: &str) -> Result<tempfile::TempPath, String> {
    let mut file = tempfile::Builder::new()
        .prefix("neovm-oracle-form-")
        .suffix(".el")
        .tempfile()
        .map_err(|e| format!("failed to create oracle form file: {e}"))?;
    file.write_all(form.as_bytes())
        .map_err(|e| format!("failed to write oracle form file: {e}"))?;
    file.flush()
        .map_err(|e| format!("failed to flush oracle form file: {e}"))?;
    Ok(file.into_temp_path())
}

pub(crate) fn run_oracle_eval(form: &str) -> Result<String, String> {
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
           (form-file (getenv "NEOVM_ORACLE_FORM_FILE"))
           (form (with-temp-buffer
                   (insert-file-contents form-file)
                   (goto-char (point-min))
                   (read (current-buffer)))))
      (princ (concat "OK " (prin1-to-string (neovm--oracle-normalize (eval form t)))))))
  (error
   (princ
    (concat "ERR "
            (prin1-to-string
             (neovm--oracle-normalize (cons (car err) (cdr err))))))))"#;
    let oracle_bin = oracle_emacs_path();

    let mem_limit = oracle_mem_limit_bytes();
    let mut cmd = Command::new(&oracle_bin);
    cmd.env("NEOVM_ORACLE_FORM_FILE", form_path.as_os_str())
        .args(["--batch", "-Q", "--eval", program]);

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

pub(crate) fn run_neovm_eval(form: &str) -> Result<String, String> {
    run_neovm_eval_with_load(form, &[])
}

/// Run a NeoVM evaluation after pre-loading Elisp files.
///
/// `load_files` are paths relative to the project `lisp/` directory,
/// loaded in order.  The caller is responsible for listing dependencies
/// before dependents (e.g. `"emacs-lisp/oclosure.el"` before
/// `"emacs-lisp/nadvice.el"`).
pub(crate) fn run_neovm_eval_with_load(form: &str, load_files: &[&str]) -> Result<String, String> {
    let mut eval = Evaluator::new();
    // Match oracle's (eval form t): evaluate with lexical binding enabled.
    eval.set_lexical_binding(true);

    if !load_files.is_empty() {
        // Set up load-path from the project's lisp/ tree so that any
        // `require` calls inside the loaded files can find dependencies.
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let project_root = manifest.parent().expect("project root");
        let lisp_dir = project_root.join("lisp");
        let subdirs = [
            "",
            "emacs-lisp",
            "progmodes",
            "language",
            "international",
            "textmodes",
            "vc",
            "leim",
        ];
        let mut load_path_entries = Vec::new();
        for sub in &subdirs {
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

    let forms = parse_forms(form).map_err(|e| format!("parse error: {e}"))?;
    let Some(first) = forms.first() else {
        return Err("no form parsed".to_string());
    };
    let rendered = match eval.eval_expr(first) {
        Ok(value) => format!("OK {}", print_value(&value)),
        Err(EvalError::Signal { symbol, data }) => {
            let mut values = Vec::with_capacity(data.len() + 1);
            values.push(Value::Symbol(symbol));
            values.extend(data);
            format!("ERR {}", print_value(&Value::list(values)))
        }
        Err(EvalError::UncaughtThrow { tag, value }) => {
            format!(
                "ERR (no-catch {} {})",
                print_value(&tag),
                print_value(&value),
            )
        }
    };
    Ok(rendered)
}

pub(crate) fn eval_oracle_and_neovm(form: &str) -> (String, String) {
    std::thread::scope(|s| {
        let oracle_handle = s.spawn(|| run_oracle_eval(form).expect("oracle eval should run"));
        let neovm = run_neovm_eval(form).expect("neovm eval should run");
        let oracle = oracle_handle.join().expect("oracle thread panicked");
        (oracle, neovm)
    })
}

pub(crate) fn eval_oracle_and_neovm_with_bootstrap(form: &str) -> (String, String) {
    std::thread::scope(|s| {
        let oracle_handle = s.spawn(|| run_oracle_eval(form).expect("oracle eval should run"));
        let neovm = run_neovm_eval_with_bootstrap(form).expect("neovm eval should run");
        let oracle = oracle_handle.join().expect("oracle thread panicked");
        (oracle, neovm)
    })
}

pub(crate) fn assert_ok_eq(expected_payload: &str, oracle: &str, neovm: &str) {
    let expected = format!("OK {expected_payload}");
    assert_eq!(oracle, expected, "oracle should match expected payload");
    assert_eq!(neovm, expected, "neovm should match expected payload");
    assert_eq!(neovm, oracle, "neovm and oracle should match");
}

pub(crate) fn assert_oracle_parity_with_load(form: &str, load_files: &[&str]) {
    let (oracle, neovm) = std::thread::scope(|s| {
        let oracle_handle = s.spawn(|| run_oracle_eval(form).expect("oracle eval should run"));
        let neovm = run_neovm_eval_with_load(form, load_files).expect("neovm eval should run");
        let oracle = oracle_handle.join().expect("oracle thread panicked");
        (oracle, neovm)
    });
    assert_eq!(neovm, oracle, "oracle parity mismatch for form: {form}");
}

/// Run a NeoVM evaluation using a fully bootstrapped evaluator.
/// Uses pdump cache when available for faster startup.
pub(crate) fn run_neovm_eval_with_bootstrap(form: &str) -> Result<String, String> {
    let mut eval = crate::emacs_core::load::create_bootstrap_evaluator_cached()
        .map_err(|e| format!("bootstrap failed: {e:?}"))?;

    let forms = parse_forms(form).map_err(|e| format!("parse error: {e}"))?;
    let Some(first) = forms.first() else {
        return Err("no form parsed".to_string());
    };
    let rendered = match eval.eval_expr(first) {
        Ok(value) => format!("OK {}", print_value(&value)),
        Err(EvalError::Signal { symbol, data }) => {
            let mut values = Vec::with_capacity(data.len() + 1);
            values.push(Value::Symbol(symbol));
            values.extend(data);
            format!("ERR {}", print_value(&Value::list(values)))
        }
        Err(EvalError::UncaughtThrow { tag, value }) => {
            format!(
                "ERR (no-catch {} {})",
                print_value(&tag),
                print_value(&value),
            )
        }
    };
    Ok(rendered)
}

pub(crate) fn assert_oracle_parity_with_bootstrap(form: &str) {
    let t0 = std::time::Instant::now();
    let (oracle, neovm) = std::thread::scope(|s| {
        let oracle_handle = s.spawn(|| run_oracle_eval(form).expect("oracle eval should run"));
        let neovm = run_neovm_eval_with_bootstrap(form).expect("neovm eval should run");
        let oracle = oracle_handle.join().expect("oracle thread panicked");
        (oracle, neovm)
    });
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
