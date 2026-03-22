use super::*;
use crate::emacs_core::eval::{Evaluator, value_to_expr};
use crate::emacs_core::expr::Expr;
use crate::emacs_core::fontset::{
    DEFAULT_FONTSET_NAME, FontSpecEntry, matching_entries_for_fontset,
};
use crate::emacs_core::intern::{intern, resolve_sym};
use crate::emacs_core::value::{HashTableTest, Value, list_to_vec, with_heap};
use crate::emacs_core::{format_eval_result, parse_forms};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Once;
use std::time::{SystemTime, UNIX_EPOCH};

struct CacheWriteFailGuard;

static TEST_TRACING_INIT: Once = Once::new();

fn init_test_tracing() {
    TEST_TRACING_INIT.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug")),
            )
            .try_init();
    });
}

impl CacheWriteFailGuard {
    fn set(phase: u8) -> Self {
        set_cache_write_fail_phase_for_test(phase);
        Self
    }
}

impl Drop for CacheWriteFailGuard {
    fn drop(&mut self) {
        clear_cache_write_fail_phase_for_test();
    }
}

fn bootstrap_fixture_path(
    load_path: &[String],
    name: &str,
    prefer_compiled: bool,
) -> Option<PathBuf> {
    for dir in load_path {
        let base = PathBuf::from(dir).join(name);
        if prefer_compiled {
            let elc = compiled_suffixed_path(&base);
            if elc.exists() {
                return Some(elc);
            }
            let el = source_suffixed_path(&base);
            if el.exists() {
                return Some(el);
            }
        } else {
            let el = source_suffixed_path(&base);
            if el.exists() {
                return Some(el);
            }
            let elc = compiled_suffixed_path(&base);
            if elc.exists() {
                return Some(elc);
            }
        }
        if base.exists() {
            return Some(base);
        }
    }
    None
}

fn format_eval_error(eval: &Evaluator, err: &EvalError) -> String {
    match err {
        EvalError::Signal { symbol, data } => {
            let mut items = Vec::with_capacity(data.len() + 1);
            items.push(Value::Symbol(*symbol));
            items.extend(data.iter().copied());
            crate::emacs_core::print::print_value_with_buffers(&Value::list(items), &eval.buffers)
        }
        EvalError::UncaughtThrow { tag, value } => format!(
            "(throw {} {})",
            crate::emacs_core::print::print_value_with_buffers(tag, &eval.buffers),
            crate::emacs_core::print::print_value_with_buffers(value, &eval.buffers),
        ),
    }
}

fn partial_bootstrap_eval_until(stop_before: &str, prefer_compiled: bool) -> Evaluator {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let lisp_dir = project_root.join("lisp");
    assert!(
        lisp_dir.is_dir(),
        "lisp/ directory not found at {}",
        lisp_dir.display()
    );

    let mut eval = Evaluator::new();
    eval.set_variable(
        "load-path",
        Value::list(bootstrap_load_path_entries(&lisp_dir)),
    );
    eval.set_variable("dump-mode", Value::symbol("pbootstrap"));
    eval.set_variable("purify-flag", Value::Nil);
    eval.set_variable("max-lisp-eval-depth", Value::Int(1600));
    eval.set_variable("inhibit-load-charset-map", Value::True);

    let etc_dir = project_root.join("etc");
    eval.set_variable(
        "data-directory",
        Value::string(format!("{}/", etc_dir.to_string_lossy())),
    );
    eval.set_variable(
        "source-directory",
        Value::string(format!("{}/", project_root.to_string_lossy())),
    );
    eval.set_variable(
        "installation-directory",
        Value::string(format!("{}/", project_root.to_string_lossy())),
    );

    let path_dirs: Vec<Value> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(|s| Value::string(s.to_string()))
        .collect();
    eval.set_variable("exec-path", Value::list(path_dirs));
    eval.set_variable("exec-suffixes", Value::Nil);
    eval.set_variable("exec-directory", Value::Nil);
    eval.set_variable(
        "menu-bar-final-items",
        Value::list(vec![Value::symbol("help-menu")]),
    );
    eval.set_variable(
        "macroexp--pending-eager-loads",
        Value::list(vec![Value::symbol("skip")]),
    );

    let glyphless_stubs = [
        "(put 'glyphless-char-display 'char-table-extra-slots 1)",
        "(setq glyphless-char-display (make-char-table 'glyphless-char-display nil))",
        "(set-char-table-extra-slot glyphless-char-display 0 'empty-box)",
    ];
    for stub in &glyphless_stubs {
        let forms = crate::emacs_core::parser::parse_forms(stub).expect("parse glyphless stub");
        let _ = eval.eval_forms(&forms);
    }

    let load_path = get_load_path(&eval.obarray());
    for name in BOOTSTRAP_LOAD_SEQUENCE {
        if *name == stop_before {
            break;
        }
        if *name == "!enable-eager-expansion" {
            eval.set_variable("macroexp--pending-eager-loads", Value::Nil);
            continue;
        }
        if *name == "!require-gv" {
            eval.require_value(Value::symbol("gv"), None, None)
                .expect("partial bootstrap require gv");
            continue;
        }
        if *name == "!load-ldefs-boot" {
            let ldefs_path = lisp_dir.join("ldefs-boot.el");
            if ldefs_path.exists() {
                load_file(&mut eval, &ldefs_path).expect("load ldefs-boot");
            }
            continue;
        }
        if name.starts_with('!') {
            continue;
        }

        let path = bootstrap_fixture_path(&load_path, name, prefer_compiled)
            .unwrap_or_else(|| panic!("bootstrap file not found: {name}"));
        load_file(&mut eval, &path).unwrap_or_else(|err| {
            panic!(
                "failed loading {name} from {}: {}",
                path.display(),
                format_eval_error(&eval, &err)
            )
        });
    }

    eval
}

#[test]
fn bootstrap_lambda_parameters_bind_special_symbols_like_gnu_emacs() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).unwrap_or_else(|err| {
        panic!("startup state: {}", format_eval_error(&eval, &err));
    });
    let forms = parse_forms(
        "(progn
            (fset 'vm-bootstrap-shadow-foo (lambda () t))
            (list
              (funcall (lambda (t) t) 7)
              (funcall (lambda (nil) nil) 9)
              (funcall (lambda (t) (vm-bootstrap-shadow-foo)) 7)
              (funcall (lambda (t) (let ((ok t)) ok)) 7)
              (mapcar (lambda (t) t) '(1 2 3))
              (mapcar (lambda (nil) nil) '(4 5 6))
              (let* ((captured 42)
                     (shadow (lambda (t) (list t captured))))
                (funcall shadow 7))
              (funcall (lambda (t) (setq t 10) t) 7)))",
    )
    .expect("parse");
    let result = eval.eval_expr(&forms[0]);
    assert_eq!(
        format_eval_result(&result),
        "OK (7 9 t 7 (1 2 3) (4 5 6) (7 42) 10)",
        "bootstrap evaluator should match GNU's special-symbol parameter binding"
    );
}

#[test]
fn bootstrap_lambda_parameter_named_pi_shadows_obsolete_global_constant() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).unwrap_or_else(|err| {
        panic!("startup state: {}", format_eval_error(&eval, &err));
    });
    let rendered = eval_rendered(
        &mut eval,
        "(list
            (funcall (lambda (pi) pi) 7)
            (funcall (lambda (pi) (let ((shadow pi)) shadow)) 11)
            (let ((fn (lambda (pi) (lambda () pi))))
              (funcall (funcall fn 13))))",
    );
    assert_eq!(
        rendered, "OK (7 11 13)",
        "bootstrap evaluator should let local pi bindings shadow the obsolete global constant"
    );
}

#[test]
fn bootstrap_cconv_closure_keeps_captured_canonical_t_binding() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).unwrap_or_else(|err| {
        panic!("startup state: {}", format_eval_error(&eval, &err));
    });
    let rendered = eval_rendered(
        &mut eval,
        "(funcall (funcall (lambda (h t) (lambda () t)) 1 2))",
    );
    assert_eq!(
        rendered, "OK 2",
        "bootstrap cconv closure should preserve captured lexical binding named t"
    );
}

#[test]
fn bootstrap_church_list_tail_and_to_list_keep_captured_t() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).unwrap_or_else(|err| {
        panic!("startup state: {}", format_eval_error(&eval, &err));
    });
    let rendered = eval_rendered(
        &mut eval,
        r#"(let* ((cnil (lambda (on-cons on-nil) (funcall on-nil)))
                  (ccons (lambda (h t)
                           (lambda (on-cons on-nil)
                             (funcall on-cons h t))))
                  (ctail (lambda (lst)
                           (funcall lst
                                    (lambda (h t) t)
                                    (lambda () cnil)))))
             (fset 'neovm--test-church-to-list
                   (lambda (lst)
                     (funcall lst
                              (lambda (h t)
                                (cons h (funcall 'neovm--test-church-to-list t)))
                              (lambda () nil))))
             (unwind-protect
                 (let* ((l1 (funcall ccons 10
                                     (funcall ccons 20
                                              (funcall ccons 30 cnil)))))
                   (list
                    (funcall 'neovm--test-church-to-list l1)
                    (funcall 'neovm--test-church-to-list (funcall ctail l1))))
               (fmakunbound 'neovm--test-church-to-list)))"#,
    );
    assert_eq!(
        rendered, "OK ((10 20 30) (20 30))",
        "bootstrap recursive church list helpers should preserve captured lexical binding named t"
    );
}

#[test]
fn bootstrap_church_map_keeps_local_t_with_outer_captures() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).unwrap_or_else(|err| {
        panic!("startup state: {}", format_eval_error(&eval, &err));
    });
    let rendered = eval_rendered(
        &mut eval,
        r#"(let* ((cnil (lambda (on-cons on-nil) (funcall on-nil)))
                  (ccons (lambda (h t)
                           (lambda (on-cons on-nil)
                             (funcall on-cons h t))))
                  (to-list nil)
                  (cmap nil))
             (fset 'neovm--test-church-to-list
                   (lambda (lst)
                     (funcall lst
                              (lambda (h t)
                                (cons h (funcall 'neovm--test-church-to-list t)))
                              (lambda () nil))))
             (setq to-list (lambda (lst) (funcall 'neovm--test-church-to-list lst)))
             (fset 'neovm--test-church-map
                   (lambda (f lst)
                     (funcall lst
                              (lambda (h t)
                                (funcall ccons (funcall f h)
                                         (funcall 'neovm--test-church-map f t)))
                              (lambda () cnil))))
             (setq cmap (lambda (f lst) (funcall 'neovm--test-church-map f lst)))
             (unwind-protect
                 (let* ((l1 (funcall ccons 10
                                     (funcall ccons 20
                                              (funcall ccons 30 cnil)))))
                   (funcall to-list (funcall cmap (lambda (x) (* x 2)) l1)))
               (fmakunbound 'neovm--test-church-to-list)
               (fmakunbound 'neovm--test-church-map)))"#,
    );
    assert_eq!(
        rendered, "OK (20 40 60)",
        "bootstrap recursive church map should preserve local t while capturing outer vars"
    );
}

#[test]
fn bootstrap_church_foldr_keeps_local_t_with_outer_captures() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).unwrap_or_else(|err| {
        panic!("startup state: {}", format_eval_error(&eval, &err));
    });
    let rendered = eval_rendered(
        &mut eval,
        r#"(let* ((cnil (lambda (on-cons on-nil) (funcall on-nil)))
                  (ccons (lambda (h t)
                           (lambda (on-cons on-nil)
                             (funcall on-cons h t))))
                  (cfoldr nil))
             (fset 'neovm--test-church-foldr
                   (lambda (f init lst)
                     (funcall lst
                              (lambda (h t)
                                (funcall f h (funcall 'neovm--test-church-foldr f init t)))
                              (lambda () init))))
             (setq cfoldr (lambda (f init lst) (funcall 'neovm--test-church-foldr f init lst)))
             (unwind-protect
                 (let* ((l1 (funcall ccons 10
                                     (funcall ccons 20
                                              (funcall ccons 30 cnil)))))
                   (list
                    (funcall cfoldr (lambda (h acc) (+ h acc)) 0 l1)
                    (funcall cfoldr (lambda (h acc) (1+ acc)) 0 l1)))
               (fmakunbound 'neovm--test-church-foldr)))"#,
    );
    assert_eq!(
        rendered, "OK (60 3)",
        "bootstrap recursive church foldr should preserve local t while capturing outer vars"
    );
}

#[test]
fn bootstrap_church_append_roundtrip_and_map_sum_match_gnu() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).unwrap_or_else(|err| {
        panic!("startup state: {}", format_eval_error(&eval, &err));
    });
    let rendered = eval_rendered(
        &mut eval,
        r#"(let* ((cnil (lambda (on-cons on-nil) (funcall on-nil)))
                  (ccons (lambda (h t)
                           (lambda (on-cons on-nil)
                             (funcall on-cons h t))))
                  (to-list nil)
                  (from-list nil)
                  (cmap nil)
                  (cfoldr nil))
             (fset 'neovm--test-church-to-list
                   (lambda (lst)
                     (funcall lst
                              (lambda (h t)
                                (cons h (funcall 'neovm--test-church-to-list t)))
                              (lambda () nil))))
             (setq to-list (lambda (lst) (funcall 'neovm--test-church-to-list lst)))
             (fset 'neovm--test-church-from-list
                   (lambda (lst)
                     (if (null lst) cnil
                       (funcall ccons (car lst)
                                (funcall 'neovm--test-church-from-list (cdr lst))))))
             (setq from-list (lambda (lst) (funcall 'neovm--test-church-from-list lst)))
             (fset 'neovm--test-church-map
                   (lambda (f lst)
                     (funcall lst
                              (lambda (h t)
                                (funcall ccons (funcall f h)
                                         (funcall 'neovm--test-church-map f t)))
                              (lambda () cnil))))
             (setq cmap (lambda (f lst) (funcall 'neovm--test-church-map f lst)))
             (fset 'neovm--test-church-foldr
                   (lambda (f init lst)
                     (funcall lst
                              (lambda (h t)
                                (funcall f h (funcall 'neovm--test-church-foldr f init t)))
                              (lambda () init))))
             (setq cfoldr (lambda (f init lst) (funcall 'neovm--test-church-foldr f init lst)))
             (unwind-protect
                 (let* ((l1 (funcall ccons 10
                                     (funcall ccons 20
                                              (funcall ccons 30
                                                       (funcall ccons 40 cnil)))))
                        (l2 (funcall from-list '(5 6 7)))
                        (cappend (lambda (l1 l2)
                                   (funcall cfoldr (lambda (h acc) (funcall ccons h acc)) l2 l1)))
                        (csum (lambda (lst)
                                (funcall cfoldr (lambda (h acc) (+ h acc)) 0 lst))))
                   (list
                    (funcall to-list (funcall from-list '(100 200 300)))
                    (funcall to-list (funcall cappend l1 l2))
                    (funcall csum (funcall cmap (lambda (x) (* x x)) l2))))
               (fmakunbound 'neovm--test-church-to-list)
               (fmakunbound 'neovm--test-church-from-list)
               (fmakunbound 'neovm--test-church-map)
               (fmakunbound 'neovm--test-church-foldr)))"#,
    );
    assert_eq!(
        rendered, "OK ((100 200 300) (10 20 30 40 5 6 7) 110)",
        "bootstrap church helper composition should match GNU Emacs"
    );
}

#[test]
fn bootstrap_runtime_does_not_leak_eval_when_compile_cl_lib_side_effects() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).unwrap_or_else(|err| {
        panic!("startup state: {}", format_eval_error(&eval, &err));
    });
    let rendered = eval_rendered(
        &mut eval,
        "(list (featurep 'cl-lib)
               (featurep 'cl-macs)
               (featurep 'cl-extra)
               (featurep 'cl-seq)
               (featurep 'gv)
               (featurep 'seq)
               (featurep 'cl-generic)
               (fboundp 'cl--block-wrapper)
               (fboundp 'cl--block-throw)
               (fboundp 'cl-every)
               (autoloadp (symbol-function 'cl-every))
               (fboundp 'cl-defstruct)
               (autoloadp (symbol-function 'cl-defstruct))
               (fboundp 'cl-reduce)
               (autoloadp (symbol-function 'cl-reduce))
               (fboundp 'cl-subseq)
               (autoloadp (symbol-function 'cl-subseq))
               (fboundp 'gv-get)
               (autoloadp (symbol-function 'gv-get))
               (fboundp 'setf)
               (autoloadp (symbol-function 'setf))
               (fboundp 'emacs-lisp-mode)
               (autoloadp (symbol-function 'emacs-lisp-mode))
               (functionp (symbol-function 'emacs-lisp-mode)))",
    );
    assert_eq!(
        rendered,
        "OK (nil nil nil nil nil t t nil nil nil nil nil nil nil nil nil nil t t t t t nil t)",
        "bootstrap runtime should match GNU -Q startup visibility for cl preload and loaddefs"
    );
}

#[test]
fn bootstrap_runtime_matches_gnu_oclosure_advice_surface() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).unwrap_or_else(|err| {
        panic!("startup state: {}", format_eval_error(&eval, &err));
    });
    let rendered = eval_rendered(
        &mut eval,
        "(list (fboundp 'advice--copy)
               (boundp 'advice--copy)
               (fboundp 'advice--cons)
               (boundp 'advice--cons)
               (fboundp 'advice--p)
               (fboundp 'advice--make)
               (featurep 'nadvice)
               (featurep 'oclosure)
               (and (advice--p (cadr (assq :before advice--how-alist))) t)
               (type-of (cadr (assq :before advice--how-alist)))
               (byte-code-function-p (cadr (assq :before advice--how-alist))))",
    );
    // NeoVM loads nadvice.el from source (no .elc), so advice handlers
    // are interpreted functions rather than byte-code functions.
    assert_eq!(
        rendered, "OK (t nil t nil t t t t t interpreted-function nil)",
        "bootstrap runtime should match GNU -Q oclosure/nadvice surface"
    );
}

const BOOTSTRAP_CACHE_RACE_DUMP_ENV: &str = "NEOVM_BOOTSTRAP_RACE_DUMP_PATH";
const BOOTSTRAP_CACHE_RACE_WORKER_TEST: &str =
    "emacs_core::load::tests::bootstrap_cache_parallel_creation_worker";

#[test]
fn bootstrap_cache_parallel_creation_worker() {
    let Some(dump_path) = std::env::var_os(BOOTSTRAP_CACHE_RACE_DUMP_ENV) else {
        return;
    };

    let dump_path = PathBuf::from(dump_path);
    let mut eval =
        create_bootstrap_evaluator_cached_at_path(&[], &dump_path).expect("worker bootstrap");
    apply_runtime_startup_state(&mut eval).expect("worker runtime startup");

    let rendered = eval_rendered(
        &mut eval,
        "(list (featurep 'cl-lib) (fboundp 'setf) (autoloadp (symbol-function 'setf)))",
    );
    assert_eq!(rendered, "OK (nil t t)");
}

#[test]
fn bootstrap_cache_parallel_creation_is_safe() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dump_path = dir.path().join("parallel-bootstrap.pdump");
    let exe = std::env::current_exe().expect("current test binary");

    let mut children = Vec::new();
    for _ in 0..2 {
        let mut cmd = Command::new(&exe);
        cmd.env(BOOTSTRAP_CACHE_RACE_DUMP_ENV, &dump_path)
            .arg("--exact")
            .arg(BOOTSTRAP_CACHE_RACE_WORKER_TEST)
            .arg("--nocapture");
        children.push(cmd.spawn().expect("spawn bootstrap worker"));
    }

    for mut child in children {
        let status = child.wait().expect("wait for bootstrap worker");
        assert!(status.success(), "bootstrap worker failed: {status}");
    }

    let mut loaded =
        create_bootstrap_evaluator_cached_at_path(&[], &dump_path).expect("reload dump after race");
    apply_runtime_startup_state(&mut loaded).expect("runtime startup after race");
    let rendered = eval_rendered(
        &mut loaded,
        "(list (featurep 'cl-lib) (fboundp 'setf) (autoloadp (symbol-function 'setf)))",
    );
    assert_eq!(rendered, "OK (nil t t)");
}

#[test]
fn bootstrap_runtime_advice_copy_and_add_behavior() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).unwrap_or_else(|err| {
        panic!("startup state: {}", format_eval_error(&eval, &err));
    });
    let rendered = eval_rendered(
        &mut eval,
        r#"(list
             (condition-case err
                 (progn
                   (funcall 'advice--copy
                            (cadr (assq :before advice--how-alist))
                            'ignore nil :before nil)
                   'ok)
               (error (cons 'error err)))
             (condition-case err
                 (progn
                   (advice-add '+ :before (lambda (&rest _args) nil))
                   'ok)
               (error (cons 'error err))))"#,
    );
    assert_eq!(rendered, "OK (ok ok)");
}

#[test]
fn bootstrap_runtime_advice_make_preserves_oclosure_type() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).unwrap_or_else(|err| {
        panic!("startup state: {}", format_eval_error(&eval, &err));
    });
    let rendered = eval_rendered(
        &mut eval,
        r#"(let ((target 'neovm--adv-target)
                 (adv 'neovm--adv-fn))
             (fset target (lambda (x) x))
             (fset adv (lambda (&rest _) nil))
             (unwind-protect
                 (let* ((main (symbol-function target))
                        (made (advice--make :before adv main nil)))
                   (list (and (advice--p made) t)
                         (advice--car made)
                         (advice--how made)
                         (type-of (advice--cdr made))))
               (fmakunbound target)
               (fmakunbound adv)))"#,
    );
    assert_eq!(
        rendered,
        "OK (t neovm--adv-fn :before interpreted-function)"
    );
}

#[test]
fn bootstrap_runtime_loaded_bytecode_preserves_wrong_arity_shape() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).unwrap_or_else(|err| {
        panic!("startup state: {}", format_eval_error(&eval, &err));
    });
    let rendered = eval_rendered(
        &mut eval,
        r#"(list
             (condition-case err (advice-add 'car :before) (error err))
             (condition-case err (advice-remove 'car) (error err))
             (condition-case err (advice-member-p 'ignore) (error err)))"#,
    );
    assert_eq!(
        rendered,
        "OK ((wrong-number-of-arguments advice-add 2) (wrong-number-of-arguments advice-remove 1) (wrong-number-of-arguments advice-member-p 1))"
    );
}

#[test]
fn bootstrap_runtime_keeps_cl_loaddefs_out_of_default_q_surface() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).unwrap_or_else(|err| {
        panic!("runtime startup state: {}", format_eval_error(&eval, &err));
    });
    let rendered = eval_rendered(
        &mut eval,
        r#"(list
             (fboundp 'cl-every)
             (autoloadp (symbol-function 'cl-every))
             (fboundp 'cl-defstruct)
             (autoloadp (symbol-function 'cl-defstruct))
             (fboundp 'cl-reduce)
             (autoloadp (symbol-function 'cl-reduce))
             (fboundp 'cl-subseq)
             (autoloadp (symbol-function 'cl-subseq)))"#,
    );
    assert_eq!(rendered, "OK (nil nil nil nil nil nil nil nil)");
}

#[test]
fn bootstrap_runtime_cl_adjoin_entry_point_works() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(progn
             (require 'cl-lib)
             (condition-case err (cl-adjoin 4 '(1 2 3)) (error err)))"#,
    );
    assert_eq!(rendered, "OK (4 1 2 3)");
}

#[test]
fn bootstrap_runtime_require_cl_lib_works() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(condition-case err
               (progn
                 (require 'cl-lib)
                 (list (featurep 'cl-lib)
                       (autoloadp (symbol-function 'cl-every))
                       (autoloadp (symbol-function 'cl-defstruct))
                       (autoloadp (symbol-function 'cl-reduce))
                       (autoloadp (symbol-function 'cl-subseq))))
             (error err))"#,
    );
    assert_eq!(rendered, "OK (t t t t t)");
}

#[test]
fn bootstrap_runtime_require_icons_restores_cl_loaddefs_under_gui_features() {
    init_test_tracing();
    let mut eval =
        create_bootstrap_evaluator_cached_with_features(&["x", "neomacs"]).expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(condition-case err
               (progn
                 (require 'icons)
                 (list (featurep 'icons)
                       (featurep 'cl-lib)
                       (fboundp 'cl-every)
                       (autoloadp (symbol-function 'cl-every))))
             (error (list 'error err)))"#,
    );
    assert_eq!(rendered, "OK (t t t t)");
}

#[test]
fn bootstrap_runtime_gui_surface_matches_gnu_icons_residency() {
    let mut eval =
        create_bootstrap_evaluator_cached_with_features(&["x", "neomacs"]).expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(list (featurep 'icons)
                 (fboundp 'icon-string)
                 (autoloadp (symbol-function 'icon-string))
                 (boundp 'icon-preference)
                 (facep 'icon)
                 (facep 'icon-button)
                 (fboundp 'describe-icon)
                 (autoloadp (symbol-function 'describe-icon))
                 (featurep 'tab-bar)
                 (fboundp 'tab-bar-mode)
                 (autoloadp (symbol-function 'tab-bar-mode)))"#,
    );
    assert_eq!(rendered, "OK (nil nil nil nil nil nil t t t t nil)");
}

#[test]
fn bootstrap_runtime_require_cl_lib_works_under_gui_features() {
    init_test_tracing();
    let mut eval =
        create_bootstrap_evaluator_cached_with_features(&["x", "neomacs"]).expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(condition-case err
               (progn
                 (require 'cl-lib)
                 (list (featurep 'cl-lib)
                       (autoloadp (symbol-function 'cl-every))
                       (autoloadp (symbol-function 'cl-defstruct))
                       (autoloadp (symbol-function 'cl-reduce))
                       (autoloadp (symbol-function 'cl-subseq))))
             (error err))"#,
    );
    assert_eq!(rendered, "OK (t t t t t)");
}

#[test]
fn bootstrap_runtime_require_uses_live_features_variable_when_internal_cache_is_stale() {
    let mut eval =
        create_bootstrap_evaluator_cached_with_features(&["x", "neomacs"]).expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    eval.features.insert(0, intern("cl-lib"));

    let rendered = eval_rendered(
        &mut eval,
        r#"(condition-case err
               (progn
                 (require 'cl-lib)
                 (list (featurep 'cl-lib)
                       (autoloadp (symbol-function 'cl-every))
                       (autoloadp (symbol-function 'cl-defstruct))
                       (autoloadp (symbol-function 'cl-reduce))
                       (autoloadp (symbol-function 'cl-subseq))))
             (error err))"#,
    );
    assert_eq!(rendered, "OK (t t t t t)");
}

#[test]
fn bootstrap_runtime_require_cl_lib_works_under_fresh_gui_features() {
    init_test_tracing();
    let mut eval =
        create_bootstrap_evaluator_with_features(&["x", "neomacs"]).expect("fresh bootstrap");
    let project_root = compile_time_project_root();
    finalize_cached_bootstrap_eval(&mut eval, &project_root).expect("finalize runtime surface");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(condition-case err
               (progn
                 (require 'cl-lib)
                 (list (featurep 'cl-lib)
                       (autoloadp (symbol-function 'cl-every))
                       (autoloadp (symbol-function 'cl-defstruct))
                       (autoloadp (symbol-function 'cl-reduce))
                       (autoloadp (symbol-function 'cl-subseq))))
             (error err))"#,
    );
    assert_eq!(rendered, "OK (t t t t t)");
}

#[test]
fn icons_v2_cache_preserves_top_level_require_cl_lib() {
    let path = compile_time_project_root().join("lisp/emacs-lisp/icons.el");
    let source = fs::read_to_string(&path).expect("read icons.el");
    let forms =
        maybe_load_expanded_cache(&path, &source, lexical_binding_enabled_for_source(&source))
            .expect("load V2 cache for icons");
    let rendered = forms.iter().map(print_expr).collect::<Vec<_>>();
    assert!(
        rendered.first() == Some(&"(require 'cl-lib)".to_string()),
        "expected cached icons replay to start with require cl-lib, got first forms: {:?}",
        rendered.iter().take(5).collect::<Vec<_>>()
    );
}

#[test]
fn bootstrap_runtime_tab_bar_mode_restores_cl_loaddefs_under_gui_features() {
    init_test_tracing();
    let mut eval =
        create_bootstrap_evaluator_cached_with_features(&["x", "neomacs"]).expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(condition-case err
               (progn
                 (require 'tab-bar)
                 (tab-bar-mode 1)
                 (list (featurep 'tab-bar)
                       (featurep 'icons)
                       (featurep 'cl-lib)
                       (fboundp 'cl-every)
                       (autoloadp (symbol-function 'cl-every))))
             (error (list 'error err)))"#,
    );
    assert_eq!(rendered, "OK (t t t t nil)");
}

#[test]
fn bootstrap_runtime_tab_bar_make_keymap_supports_auto_width_hash_test() {
    let mut eval =
        create_bootstrap_evaluator_cached_with_features(&["x", "neomacs"]).expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(condition-case err
               (progn
                 (require 'tab-bar)
                 (setq tab-bar-show 1)
                 (tab-bar-mode 1)
                 (tab-bar-new-tab)
                 (switch-to-buffer (get-buffer-create "*tb-2*"))
                 (tab-bar-select-tab 1)
                 (and (string-match-p "\\*tb-2\\*" (prin1-to-string (tab-bar-make-keymap-1))) t))
             (error (list 'error err)))"#,
    );
    assert_eq!(rendered, "OK t");
}

#[test]
fn bootstrap_runtime_cached_gui_surface_clears_transient_loader_state() {
    let eval = create_bootstrap_evaluator_cached_with_features(&["x", "neomacs"])
        .expect("bootstrap evaluator");
    assert!(
        eval.require_stack.is_empty(),
        "require_stack leaked from bootstrap"
    );
    assert!(
        eval.loads_in_progress.is_empty(),
        "loads_in_progress leaked from bootstrap"
    );
}

#[test]
fn bootstrap_runtime_cached_gui_surface_restores_window_system_surface() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["x", "neomacs"])
        .expect("bootstrap evaluator");
    let rendered = eval_rendered(
        &mut eval,
        r#"(list (window-system)
                 initial-window-system
                 (frame-parameter nil 'window-system)
                 (frame-parameter nil 'display-type)
                 (display-color-cells)
                 (display-visual-class))"#,
    );
    assert_eq!(
        rendered,
        "OK (neomacs neomacs neomacs color 16777216 true-color)"
    );
}

#[test]
fn bootstrap_runtime_require_eieio_restores_cl_loaddefs_surface() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(condition-case err
               (progn
                 (require 'eieio)
                 (list (featurep 'eieio)
                       (featurep 'eieio-core)
                       (autoloadp (symbol-function 'cl-every))
                       (autoloadp (symbol-function 'cl-defstruct))
                       (autoloadp (symbol-function 'cl-reduce))
                       (autoloadp (symbol-function 'cl-subseq))))
             (error err))"#,
    );
    assert_eq!(rendered, "OK (t t t nil t t)");
}

#[test]
fn bootstrap_runtime_loads_gnu_subr_helpers() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(list
             (always 1 2 3)
             (assq-delete-all 'foo '((foo . 1) ignored (bar . 2) (foo . 3)))
             (butlast '(1 2 3 4) 2)
             (number-sequence 1 4)
             (split-string " a  b " nil t)
             (string-prefix-p "neo" "neovm")
             (string-suffix-p "vm" "neovm")
             (string-trim "  vm  ")
             (string-trim-left "  vm  ")
             (string-trim-right "  vm  ")
             (json-available-p)
             (let ((g1 (gensym))
                   (g2 (gensym [1 2])))
               (list (and (symbolp g1)
                          (string-prefix-p "g" (symbol-name g1)))
                     (and (symbolp g2)
                          (string-prefix-p "[1 2]" (symbol-name g2)))))
             (string-join '("a" "b" "c") "-")
             (eventp ?a)
             (timeout-event-p '(timer-event 1))
             (event-modifiers (event-convert-list '(control meta ?a)))
             (event-basic-type (event-convert-list '(control meta ?a)))
             (equal (single-key-description
                     (event-apply-modifier ?a 'control 26 "C-"))
                    "C-a")
             (equal (last '(1 2 3 4)) '(4))
             (equal (listify-key-sequence "Az") '(65 122))
             (key-valid-p "C-x C-f")
             (substring-no-properties
              (help-key-description (kbd "C-a") (kbd "C-a")))
             (file-size-human-readable 1536)
             (file-size-human-readable 1572864 'iec)
             (condition-case nil
                 (progn (file-size-human-readable 1 nil nil 1) nil)
               (wrong-type-argument t))
             (file-size-human-readable-iec 1536)
             (condition-case nil
                 (progn (file-size-human-readable-iec "x") nil)
               (wrong-type-argument t)))"#,
    );
    assert_eq!(
        rendered,
        "OK (t (ignored (bar . 2)) (1 2) (1 2 3 4) (\"a\" \"b\") t t \"vm\" \"vm  \" \"  vm\" t (t t) \"a-b-c\" t t (control meta) 97 t t t t \"C-a\" \"1.5k\" \"1.5MiB\" t \"1.5 KiB\" t)"
    );
}

#[test]
fn bootstrap_runtime_preserves_gnu_global_prefix_links() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(list
             (lookup-key (current-global-map) "\e")
             (lookup-key esc-map "x")
             (lookup-key (current-global-map) "\C-x")
             (lookup-key ctl-x-map "2")
             (lookup-key ctl-x-map "3")
             (lookup-key (current-global-map) "\e\e\e")
             (lookup-key (current-global-map) "\C-x\C-z"))"#,
    );
    assert_eq!(
        rendered,
        "OK (ESC-prefix execute-extended-command Control-X-prefix split-window-below split-window-right keyboard-escape-quit suspend-emacs)"
    );
}

#[test]
fn bootstrap_runtime_preserves_gnu_minibuffer_completion_bindings() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(list
             (lookup-key minibuffer-local-map "\r")
             (lookup-key minibuffer-local-completion-map (kbd "RET"))
             (lookup-key minibuffer-local-must-match-map (kbd "RET"))
             (lookup-key read-extended-command-mode-map (kbd "M-X")))"#,
    );
    assert_eq!(
        rendered,
        "OK (exit-minibuffer minibuffer-completion-exit minibuffer-complete-and-exit execute-extended-command-cycle)"
    );
}

#[test]
fn bootstrap_runtime_global_obarray_proxy_preserves_completion_semantics() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(progn
             (defun neo-obarray-probe ()
               (interactive))
             (list
               (obarrayp obarray)
               (intern-soft "neo-obarray-probe" obarray)
               (try-completion "neo-obarray-probe" obarray #'commandp)
               (test-completion "neo-obarray-probe" obarray #'commandp)
               (not (null (member "neo-obarray-probe"
                                  (all-completions "neo-obarray"
                                                   obarray
                                                   #'commandp))))))"#,
    );
    assert_eq!(rendered, "OK (t neo-obarray-probe t t t)");
}

#[test]
fn bootstrap_runtime_execute_extended_command_exits_minibuffer_on_ret() {
    init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let setup = parse_forms(
        r#"(progn
             (setq neo-ret-probe-ran nil)
             (defun neo-ret-probe ()
               (interactive)
               (setq neo-ret-probe-ran t)))"#,
    )
    .expect("parse execute-extended-command RET probe");
    let _ = eval.eval_forms(&setup);

    let (tx, rx) = crossbeam_channel::unbounded();
    drop(tx);
    eval.input_rx = Some(rx);
    eval.command_loop.running = true;

    for ch in "neo-ret-probe".chars() {
        eval.command_loop
            .unread_events
            .push_back(crate::keyboard::KeyEvent::char(ch));
    }
    eval.command_loop
        .unread_events
        .push_back(crate::keyboard::KeyEvent::named(
            crate::keyboard::NamedKey::Return,
        ));

    let result = eval
        .apply(Value::symbol("execute-extended-command"), vec![Value::Nil])
        .expect("execute-extended-command should return after RET");
    assert_eq!(result, Value::Nil);
    assert!(
        eval.eval_symbol("neo-ret-probe-ran")
            .expect("probe var should exist")
            .is_truthy(),
        "expected RET to exit the minibuffer and run the command"
    );
}

#[test]
fn bootstrap_runtime_command_loop_executes_meta_x_command_on_ret() {
    init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let setup = parse_forms(
        r#"(progn
             (setq neo-ret-probe-ran nil)
             (defun neo-ret-probe ()
               (interactive)
               (setq neo-ret-probe-ran t)
               (exit-recursive-edit)))"#,
    )
    .expect("parse command-loop M-x RET probe");
    let _ = eval.eval_forms(&setup);

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::KeyPress(
        crate::keyboard::KeyEvent::char_with_mods('x', crate::keyboard::Modifiers::meta()),
    ))
    .expect("queue M-x");
    for ch in "neo-ret-probe".chars() {
        tx.send(crate::keyboard::InputEvent::KeyPress(
            crate::keyboard::KeyEvent::char(ch),
        ))
        .expect("queue command chars");
    }
    tx.send(crate::keyboard::InputEvent::KeyPress(
        crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Return),
    ))
    .expect("queue RET");
    tx.send(crate::keyboard::InputEvent::CloseRequested)
        .expect("queue close request");
    drop(tx);

    eval.input_rx = Some(rx);
    eval.command_loop.running = true;

    let result = eval
        .recursive_edit_inner()
        .expect("command loop should exit normally");
    assert_eq!(result, Value::Nil);
    assert!(
        eval.eval_symbol("neo-ret-probe-ran")
            .expect("probe var should exist")
            .is_truthy(),
        "expected M-x command RET path to run the command before shutdown fallback"
    );
}

#[test]
fn bootstrap_runtime_list_buffers_command_path_matches_gnu() {
    init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"(condition-case err
               (progn
                 (list-buffers)
                 'ok)
             (error err))"#,
    );

    assert_eq!(rendered, "OK ok");
}

#[test]
fn bootstrap_runtime_buffer_file_name_variable_defaults_to_nil() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"(with-current-buffer "*scratch*"
             (condition-case err
                 (list buffer-file-name (buffer-file-name))
               (error err)))"#,
    );

    assert_eq!(rendered, "OK (nil nil)");
}

#[test]
fn bootstrap_runtime_buffer_auto_save_file_name_variable_defaults_to_nil() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"(with-current-buffer "*scratch*"
             (condition-case err
                 buffer-auto-save-file-name
               (error err)))"#,
    );

    assert_eq!(rendered, "OK nil");
}

#[test]
fn bootstrap_runtime_add_to_invisibility_spec_matches_gnu_default_t() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"(with-current-buffer (get-buffer-create "*inv*")
             (condition-case err
                 (progn
                   (add-to-invisibility-spec '(dired . t))
                   buffer-invisibility-spec)
               (error err)))"#,
    );

    assert_eq!(rendered, "OK ((dired . t) t)");
}

#[test]
fn bootstrap_runtime_view_hello_file_command_path_matches_gnu() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"(condition-case err
               (progn
                 (view-hello-file)
                 (list (buffer-name)
                       major-mode
                       buffer-auto-save-file-name
                       (stringp buffer-file-name)))
             (error err))"#,
    );

    assert_eq!(rendered, "OK (\"HELLO\" fundamental-mode nil t)");
}

#[test]
fn bootstrap_runtime_cd_accepts_existing_abbreviated_directory_like_gnu() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"(let* ((dir (abbreviate-file-name default-directory))
                  (expanded (expand-file-name dir)))
             (list (file-directory-p dir)
                   (file-accessible-directory-p dir)
                   (condition-case err
                       (progn
                         (cd dir)
                         (equal default-directory expanded))
                     (error err))))"#,
    );

    assert_eq!(rendered, "OK (t t t)");
}

#[test]
fn bootstrap_runtime_find_file_handles_multibyte_markdown_like_gnu() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let target = project_root.join("docs/rust-display-engine.md");
    let target_str = target.to_string_lossy();

    let rendered = eval_rendered(
        &mut eval,
        &format!(
            r#"(condition-case err
                   (progn
                     (find-file "{}")
                     (list (buffer-name)
                           (> (buffer-size) 0)
                           (integerp
                            (string-match-p "Redesign Opportunities"
                                            (buffer-string)))))
                 (error err))"#,
            target_str
        ),
    );

    assert_eq!(rendered, "OK (\"rust-display-engine.md\" t t)");
}

fn bootstrap_runtime_read_key_sequence_follows_escape_prefix_command() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    eval.command_loop
        .unread_events
        .push_back(crate::keyboard::KeyEvent::named(
            crate::keyboard::NamedKey::Escape,
        ));
    eval.command_loop
        .unread_events
        .push_back(crate::keyboard::KeyEvent::char('x'));

    let (keys, binding) = eval.read_key_sequence().expect("read ESC x sequence");
    assert_eq!(keys, vec![Value::Int(27), Value::Int('x' as i64)]);
    assert_eq!(binding, Value::symbol("execute-extended-command"));
}

#[test]
fn bootstrap_runtime_read_key_sequence_follows_meta_x_command() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    eval.command_loop
        .unread_events
        .push_back(crate::keyboard::KeyEvent::char_with_mods(
            'x',
            crate::keyboard::Modifiers::meta(),
        ));

    let (keys, binding) = eval.read_key_sequence().expect("read M-x sequence");
    assert_eq!(keys, vec![Value::Int(134_217_848)]);
    assert_eq!(binding, Value::symbol("execute-extended-command"));
}

#[test]
fn bootstrap_runtime_loads_gnu_window_split_entry_point() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    let forms = parse_forms(
        "(list (fboundp 'split-window)
               (let ((w (split-window)))
                 (list (window-live-p w)
                       (length (window-list)))))",
    )
    .expect("parse");
    let rendered = format_eval_result(&eval.eval_expr(&forms[0]));
    assert_eq!(rendered, "OK (t (t 2))");
}

#[test]
fn bootstrap_runtime_cl_reduce_entry_point_works() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(progn
             (require 'cl-lib)
             (condition-case err (cl-reduce #'+ '(1 2 3)) (error err)))"#,
    );
    assert_eq!(rendered, "OK 6");
}

#[test]
fn bootstrap_runtime_cl_defstruct_entry_point_works() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(progn
             (require 'cl-lib)
             (condition-case err
                 (progn
                   (cl-defstruct neovm--dbg-point x y)
                   (let ((p (make-neovm--dbg-point :x 1 :y 2)))
                     (list (neovm--dbg-point-p p)
                           (neovm--dbg-point-x p)
                           (neovm--dbg-point-y p))))
               (error err)))"#,
    );
    assert_eq!(rendered, "OK (t 1 2)");
}

#[test]
fn bootstrap_runtime_interpreted_closure_filter_state_matches_gnu_emacs() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(list
             (compiled-function-p (symbol-function 'cconv-fv))
             (compiled-function-p (symbol-function 'cconv-make-interpreted-closure))
             internal-make-interpreted-closure-function)"#,
    );
    // NeoVM loads cconv from .el source (no .elc), so the functions are
    // interpreted, not byte-compiled.  The important invariant is that the
    // closure filter variable is set to cconv-make-interpreted-closure.
    assert_eq!(rendered, "OK (nil nil cconv-make-interpreted-closure)");
}

#[test]
fn bootstrap_runtime_rebound_interpreted_closure_filter_remains_observable() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(progn
             (setq neovm--hook-count 0)
             (fset 'neovm--counting-make-interpreted-closure
                   (lambda (args body env docstring iform)
                     (setq neovm--hook-count (1+ neovm--hook-count))
                     (make-interpreted-closure args body env docstring iform)))
             (let ((internal-make-interpreted-closure-function
                    'neovm--counting-make-interpreted-closure))
               (unwind-protect
                   (list
                    (funcall (let ((x 1)) (lambda () x)))
                    (funcall (let ((x 1)) (lambda () x)))
                    neovm--hook-count)
                 (fmakunbound 'neovm--counting-make-interpreted-closure)
                 (makunbound 'neovm--hook-count))))"#,
    );
    assert_eq!(rendered, "OK (1 1 2)");
}

#[test]
fn bootstrap_runtime_cl_defstruct_macroexpand_all_head_matches_gnu() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(progn
             (require 'cl-lib)
             (condition-case err
                 (car (macroexpand-all '(cl-defstruct neovm--dbg-point x y)))
               (error err)))"#,
    );
    assert_eq!(rendered, "OK progn");
}

#[test]
fn bootstrap_runtime_cl_defstruct_autoload_state_matches_gnu() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(progn
             (require 'cl-lib)
             (let ((before (symbol-function 'cl-defstruct)))
               (list
                 (autoloadp before)
                 (condition-case err
                     (type-of (autoload-do-load before 'cl-defstruct t))
                   (error err))
                 (featurep 'cl-macs)
                 (boundp 'cl--bind-forms)
                 (special-variable-p 'cl--bind-forms)
                 (condition-case err
                     (car (macroexpand '(cl-defstruct neovm--dbg-point x y)))
                   (error err))
                 (boundp 'cl--bind-forms)
                 (special-variable-p 'cl--bind-forms))))"#,
    );
    assert_eq!(rendered, "OK (t cons t nil nil progn nil nil)");
}

#[test]
fn bootstrap_runtime_cl_transform_lambda_matches_gnu() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(progn
             (require 'cl-lib)
             (autoload-do-load (symbol-function 'cl-defstruct) 'cl-defstruct t)
             (condition-case err
                 (cl--transform-lambda '((x) 1) 'vm-foo)
               (error err)))"#,
    );
    assert_eq!(rendered, "OK ((x) (cl-block vm-foo 1))");
}

#[test]
fn bootstrap_runtime_cl_defun_entry_point_matches_gnu() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(progn
             (require 'cl-lib)
             (condition-case err
                 (progn
                   (cl-defun vm-foo () 1)
                   (vm-foo))
               (error err)))"#,
    );
    assert_eq!(rendered, "OK 1");
}

#[test]
fn bootstrap_runtime_cl_defsubst_key_defaults_matches_gnu() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(progn
             (require 'cl-lib)
             (condition-case err
                 (progn
                   (cl-defsubst vm-make (&cl-defs (nil (a) (b)) &key a b)
                     (list a b))
                   (vm-make :a 1 :b 2))
               (error err)))"#,
    );
    assert_eq!(rendered, "OK (1 2)");
}

#[test]
fn bootstrap_runtime_cl_defun_cl_quote_key_defaults_matches_gnu() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(progn
             (require 'cl-lib)
             (condition-case err
                 (progn
                   (cl-defun vm-cmpr (cl-whole &cl-quote &cl-defs (nil (a) (b)) &key a b)
                     (list cl-whole a b))
                   (vm-cmpr 'whole :a 1 :b 2))
               (error err)))"#,
    );
    assert_eq!(rendered, "OK (whole 1 2)");
}

#[test]
fn bootstrap_runtime_cl_transform_lambda_cl_quote_key_defaults_matches_gnu() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(progn
             (require 'cl-lib)
             (autoload-do-load (symbol-function 'cl-defstruct) 'cl-defstruct t)
             (condition-case err
                 (cl--transform-lambda
                  '((cl-whole &cl-quote &cl-defs (nil (a) (b)) &key a b)
                    (list cl-whole a b))
                  'vm-cmpr)
               (error err)))"#,
    );
    assert_eq!(
        rendered,
        "OK ((cl-whole &rest --cl-rest--) \"\n\n(fn CL-WHOLE &cl-quote &key A B)\" (let* ((a (car (cdr (plist-member --cl-rest-- ':a)))) (b (car (cdr (plist-member --cl-rest-- ':b))))) (progn (let ((--cl-keys-- --cl-rest--)) (while --cl-keys-- (cond ((memq (car --cl-keys--) '(:a :b :allow-other-keys)) (unless (cdr --cl-keys--) (error \"Missing argument for %s\" (car --cl-keys--))) (setq --cl-keys-- (cdr (cdr --cl-keys--)))) ((car (cdr (memq ':allow-other-keys --cl-rest--))) (setq --cl-keys-- nil)) (t (error \"Keyword argument %S not one of (:a :b)\" (car --cl-keys--)))))) (cl-block vm-cmpr (list cl-whole a b)))))"
    );
}

fn eval_rendered(eval: &mut Evaluator, form: &str) -> String {
    let parsed = crate::emacs_core::parser::parse_forms(form).expect("parse eval form");
    match eval.eval_expr(&parsed[0]) {
        Ok(value) => format!(
            "OK {}",
            crate::emacs_core::print::print_value_with_buffers(&value, &eval.buffers)
        ),
        Err(err) => format!("ERR {}", format_eval_error(eval, &err)),
    }
}

#[test]
fn bootstrap_condition_case_lexical_handler_binding_restores_outer_let() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"(let ((outer 'original))
             (list
              (condition-case outer
                  (/ 1 0)
                (arith-error
                 (setq outer (list 'caught (car outer)))
                 outer))
              outer))"#,
    );
    assert_eq!(rendered, "OK ((caught arith-error) original)");
}

#[test]
fn bootstrap_runtime_seeds_gnu_per_buffer_frame_display_vars() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"(list left-margin-width
                 right-margin-width
                 left-fringe-width
                 right-fringe-width
                 fringes-outside-margins
                 scroll-bar-width
                 scroll-bar-height
                 vertical-scroll-bar
                 horizontal-scroll-bar)"#,
    );

    assert_eq!(rendered, "OK (nil nil nil nil nil nil nil t t)");
}

#[test]
fn bootstrap_runtime_standard_fontset_spec_creates_named_fontset() {
    let mut eval =
        create_bootstrap_evaluator_cached_with_features(&["neomacs"]).expect("bootstrap evaluator");
    let parsed = parse_forms(
        r#"(let ((name (create-fontset-from-fontset-spec standard-fontset-spec t)))
             (list name (query-fontset "fontset-standard")))"#,
    )
    .expect("parse fontset creation form");
    let result = eval
        .eval_expr(&parsed[0])
        .expect("standard fontset creation should evaluate");
    assert_eq!(
        list_to_vec(&result),
        Some(vec![
            Value::string("-*-fixed-medium-r-normal-*-16-*-*-*-*-*-fontset-standard"),
            Value::string("-*-fixed-medium-r-normal-*-16-*-*-*-*-*-fontset-standard"),
        ])
    );
}

#[test]
fn bootstrap_runtime_setup_default_fontset_preserves_gnu_han_order() {
    let mut eval =
        create_bootstrap_evaluator_with_features(&["neomacs"]).expect("fresh bootstrap evaluator");
    let rendered = eval_rendered(
        &mut eval,
        r#"(list (charsetp 'devanagari-cdac)
                 (aref char-script-table ?好))"#,
    );
    assert_eq!(rendered, "OK (t han)");

    let forms = parse_forms("(setup-default-fontset)").expect("parse default fontset setup");
    eval.eval_expr(&forms[0])
        .expect("setup-default-fontset should evaluate");

    let entries = matching_entries_for_fontset(DEFAULT_FONTSET_NAME, '好');
    let registries: Vec<Option<String>> = entries
        .iter()
        .take(23)
        .map(|entry| match entry {
            FontSpecEntry::Font(spec) => spec.registry.clone(),
            FontSpecEntry::ExplicitNone => None,
        })
        .collect();
    // GNU Emacs 31.1 returns a shorter Han sequence here than older
    // assumptions suggested. Normalize GNU's wildcard-heavy registry
    // strings to Neomacs' stored registry form before comparing.
    assert_eq!(
        registries,
        vec![
            Some("gb2312.1980-0".to_string()),
            Some("jisx0208*".to_string()),
            Some("big5*".to_string()),
            Some("ksc5601.1987*".to_string()),
            Some("cns11643.1992-1".to_string()),
            Some("gbk-0".to_string()),
            Some("gb18030".to_string()),
            Some("jisx0213.2000-1".to_string()),
            Some("jisx0213.2004-1".to_string()),
            Some("iso10646-1".to_string()),
            Some("iso10646-1".to_string()),
            Some("iso10646-1".to_string()),
            Some("iso10646-1".to_string()),
            Some("iso10646-1".to_string()),
            Some("gb2312.1980".to_string()),
            Some("gbk-0".to_string()),
            Some("gb18030".to_string()),
            Some("jisx0208".to_string()),
            Some("ksc5601.1987".to_string()),
            Some("cns11643.1992-1".to_string()),
            Some("big5".to_string()),
            Some("jisx0213.2000-1".to_string()),
            Some("jisx0213.2004-1".to_string()),
        ]
    );
}

#[test]
fn bootstrap_runtime_fontset_font_for_han_matches_gnu_order() {
    let mut eval =
        create_bootstrap_evaluator_with_features(&["neomacs"]).expect("fresh bootstrap evaluator");

    let rendered = eval_rendered(
        &mut eval,
        r#"(progn
             (setup-default-fontset)
             (fontset-font t ?好 t))"#,
    );

    assert!(
        rendered.starts_with(
            "OK ((nil . \"gb2312.1980-0\") \
             (nil . \"jisx0208*\") \
             (nil . \"big5*\") \
             (nil . \"ksc5601.1987*\") \
             (nil . \"cns11643.1992-1\") \
             (nil . \"gbk-0\") \
             (nil . \"gb18030\") \
             (nil . \"jisx0213.2000-1\") \
             (nil . \"jisx0213.2004-1\")"
        ),
        "unexpected fontset-font order: {rendered}"
    );
}

#[test]
fn bootstrap_runtime_fontset_font_accepts_multibyte_character_ints() {
    let mut eval =
        create_bootstrap_evaluator_with_features(&["neomacs"]).expect("fresh bootstrap evaluator");

    let rendered = eval_rendered(
        &mut eval,
        r#"(progn
             (setup-default-fontset)
             (let ((ch (string-to-char "好")))
               (list ch (fontset-font t ch t))))"#,
    );

    assert!(
        rendered.starts_with(
            "OK (22909 ((nil . \"gb2312.1980-0\") \
             (nil . \"jisx0208*\") \
             (nil . \"big5*\") \
             (nil . \"ksc5601.1987*\") \
             (nil . \"cns11643.1992-1\") \
             (nil . \"gbk-0\") \
             (nil . \"gb18030\") \
             (nil . \"jisx0213.2000-1\") \
             (nil . \"jisx0213.2004-1\")"
        ),
        "unexpected fontset-font result for multibyte int character: {rendered}"
    );
}

#[test]
fn bootstrap_x_runtime_prebinds_gnu_x_globals_before_x_win_initialization() {
    let mut eval = create_bootstrap_evaluator_with_features(&["x"]).expect("x bootstrap evaluator");
    let rendered = eval_rendered(
        &mut eval,
        r#"(list (hash-table-p x-keysym-table)
                 (hash-table-test x-keysym-table)
                 (gethash 160 x-keysym-table)
                 x-selection-timeout
                 x-session-id
                 x-session-previous-id
                 x-ctrl-keysym
                 x-alt-keysym
                 x-hyper-keysym
                 x-meta-keysym
                 x-super-keysym)"#,
    );
    assert_eq!(rendered, "OK (t eql 160 0 nil nil nil nil nil nil nil)");
}

#[test]
fn bootstrap_runtime_match_data_returns_marker_handles_for_buffer_search() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"(with-temp-buffer
             (insert "foobar")
             (goto-char (point-min))
             (looking-at "\\(foo\\)\\(bar\\)")
             (match-data))"#,
    );
    assert_eq!(
        rendered,
        "OK (#<marker in no buffer> #<marker in no buffer> #<marker in no buffer> #<marker in no buffer> #<marker in no buffer> #<marker in no buffer>)"
    );
}

#[test]
fn bootstrap_neomacs_runtime_loads_neomacs_term_layer() {
    let mut eval = create_bootstrap_evaluator_with_features(&["neomacs"])
        .expect("neomacs bootstrap evaluator");
    assert!(eval.feature_present("neomacs"));
    assert!(eval.feature_present("neomacs-win"));
    assert!(!eval.feature_present("x-win"));
}

#[test]
fn bootstrap_neomacs_gui_runtime_prefers_neomacs_term_layer_over_x_term() {
    let mut eval = create_bootstrap_evaluator_with_features(&["neomacs", "x"])
        .expect("neomacs+x bootstrap evaluator");
    assert!(eval.feature_present("neomacs"));
    assert!(eval.feature_present("x"));
    assert!(eval.feature_present("neomacs-win"));
    assert!(!eval.feature_present("x-win"));
}

#[test]
fn bootstrap_help_fns_loads_and_preserves_hook_depth_metadata() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let help_fns = project_root.join("lisp/help-fns.el");

    let rendered = fresh_bootstrap_eval_with_loaded_file(
        &help_fns,
        r#"
(let* ((depth-sym (get 'help-fns-describe-function-functions 'hook--depth-alist))
       (depth-alist (default-value depth-sym)))
  (list
   (symbolp depth-sym)
   (not (eq depth-sym 'depth-alist))
   (equal (symbol-name depth-sym) "depth-alist")
   (eq (alist-get 'help-fns--compiler-macro depth-alist nil nil #'eq) 100)
   (memq 'help-fns--compiler-macro help-fns-describe-function-functions)))
"#,
    );

    assert_eq!(rendered, "OK (t t t t (help-fns--compiler-macro))");
}

#[test]
fn bootstrap_help_fns_describe_function_writes_help_buffer() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let help_fns = project_root.join("lisp/help-fns.el");

    let rendered = fresh_bootstrap_eval_with_loaded_file(
        &help_fns,
        r#"
(let ((result (funcall (symbol-function 'describe-function) 'car)))
  (list
   (stringp result)
   (bufferp (get-buffer "*Help*"))
   (with-current-buffer (get-buffer "*Help*")
     (> (length (buffer-string)) 0))))
"#,
    );

    assert_eq!(rendered, "OK (t t t)");
}

#[test]
fn bootstrap_help_fns_describe_variable_writes_help_buffer() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let help_fns = project_root.join("lisp/help-fns.el");

    let rendered = fresh_bootstrap_eval_with_loaded_file(
        &help_fns,
        r#"
(let ((result (funcall (symbol-function 'describe-variable) 'load-path)))
  (list
   (stringp result)
   (bufferp (get-buffer "*Help*"))
   (with-current-buffer (get-buffer "*Help*")
     (> (length (buffer-string)) 0))))
"#,
    );

    assert_eq!(rendered, "OK (t t t)");
}

#[test]
fn bootstrap_runtime_describe_function_autoloads_help_fns() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"(let ((before (symbol-function 'describe-function)))
             (list
              (autoloadp before)
              (stringp (describe-function 'car))
              (autoloadp (symbol-function 'describe-function))
              (bufferp (get-buffer "*Help*"))))"#,
    );

    assert_eq!(rendered, "OK (t t nil t)");
}

#[test]
fn bootstrap_runtime_describe_variable_autoloads_help_fns() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"(let ((before (symbol-function 'describe-variable)))
             (list
              (autoloadp before)
              (stringp (describe-variable 'load-path))
              (autoloadp (symbol-function 'describe-variable))
              (bufferp (get-buffer "*Help*"))))"#,
    );

    assert_eq!(rendered, "OK (t t nil t)");
}

#[test]
fn bootstrap_runtime_eieio_core_starts_as_gnu_autoload_state() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");

    let rendered = eval_rendered(
        &mut eval,
        r#"(list
             (featurep 'eieio-core)
             (autoloadp (symbol-function 'eieio-defclass-autoload)))"#,
    );

    assert_eq!(rendered, "OK (nil t)");
}

#[test]
fn runtime_startup_state_preserves_gui_frame_metrics() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    let scratch = eval.buffers.create_buffer("*scratch*");
    let fid = eval.frames.create_frame("F1", 960, 640, scratch);
    let frame_before = eval.frames.get(fid).expect("bootstrap frame should exist");
    let expected_char_width = frame_before.char_width;
    let expected_char_height = frame_before.char_height;
    let expected_font_pixel_size = frame_before.font_pixel_size;

    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let frame_after = eval.frames.get(fid).expect("runtime frame should exist");
    assert_eq!(frame_after.char_width, expected_char_width);
    assert_eq!(frame_after.char_height, expected_char_height);
    assert_eq!(frame_after.font_pixel_size, expected_font_pixel_size);
}

#[test]
fn bootstrap_misc_upcase_char_preserves_point_and_uppercases_region() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let misc = project_root.join("lisp/misc.el");

    let rendered = fresh_bootstrap_eval_with_loaded_file(
        &misc,
        r#"
(with-temp-buffer
  (insert "abCd")
  (goto-char (point-min))
  (funcall (symbol-function 'upcase-char) 2)
  (list (buffer-string) (point)))
"#,
    );

    assert_eq!(rendered, r#"OK ("ABCd" 1)"#);
}

#[test]
fn bootstrap_runtime_upcase_char_autoloads_misc() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"(with-temp-buffer
             (insert "ab")
             (goto-char (point-min))
             (let ((before (symbol-function 'upcase-char)))
               (list
                (autoloadp before)
                (null (upcase-char 1))
                (buffer-string)
                (autoloadp (symbol-function 'upcase-char))
                (point))))"#,
    );

    assert_eq!(rendered, r#"OK (t t "Ab" nil 1)"#);
}

fn cached_bootstrap_eval_with_loaded_file(path: &std::path::Path, form: &str) -> String {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    load_file(&mut eval, path).unwrap_or_else(|err| {
        panic!(
            "failed loading {}: {}",
            path.display(),
            format_eval_error(&eval, &err)
        )
    });
    eval_rendered(&mut eval, form)
}

fn cached_bootstrap_with_loaded_source(source: &str, form: &str) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("vm-gv-load.el");
    std::fs::write(&path, source).expect("write temp elisp source");
    cached_bootstrap_eval_with_loaded_file(&path, form)
}

fn fresh_bootstrap_eval_with_loaded_file(path: &std::path::Path, form: &str) -> String {
    let mut eval = create_bootstrap_evaluator().expect("bootstrap evaluator");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    load_file(&mut eval, path).unwrap_or_else(|err| {
        panic!(
            "failed loading {}: {}",
            path.display(),
            format_eval_error(&eval, &err)
        )
    });
    eval_rendered(&mut eval, form)
}

#[test]
fn profile_single_bootstrap_file_load() {
    if std::env::var("NEOVM_PROFILE_BOOTSTRAP_FILE").is_err() {
        return;
    }

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();

    let target = std::env::var("NEOVM_PROFILE_BOOTSTRAP_FILE").expect("profile target");
    let stop_before =
        std::env::var("NEOVM_PROFILE_BOOTSTRAP_STOP_BEFORE").unwrap_or_else(|_| target.clone());
    let prefer_compiled =
        std::env::var("NEOVM_PROFILE_BOOTSTRAP_PREFER_COMPILED").as_deref() == Ok("1");

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let lisp_dir = project_root.join("lisp");

    let mut eval = partial_bootstrap_eval_until(&stop_before, prefer_compiled);
    let load_path = get_load_path(&eval.obarray());
    let path = bootstrap_fixture_path(&load_path, &target, prefer_compiled)
        .unwrap_or_else(|| panic!("bootstrap file not found: {target}"));

    let start = std::time::Instant::now();
    load_file(&mut eval, &path).unwrap_or_else(|err| {
        panic!(
            "failed loading {target} from {}: {}",
            path.display(),
            format_eval_error(&eval, &err)
        )
    });
    tracing::info!(
        "PROFILE target={} compiled={} path={} elapsed={:.2?}",
        target,
        prefer_compiled,
        path.display(),
        start.elapsed()
    );

    let _ = lisp_dir;
}

#[test]
fn cache_write_disable_env_value_matrix() {
    for value in ["1", "true", "TRUE", " yes ", "On", "\tyEs\n"] {
        assert!(
            cache_write_disabled_env_value(value),
            "expected '{value}' to disable load cache writes",
        );
    }

    for value in ["0", "false", "FALSE", "no", "off", "", "   ", "maybe"] {
        assert!(
            !cache_write_disabled_env_value(value),
            "expected '{value}' to leave load cache writes enabled",
        );
    }
}

#[test]
fn strip_reader_prefix_handles_bom_and_shebang() {
    let source = "#!/usr/bin/env emacs --script\n(setq vm-shebang-strip 1)\n";
    assert_eq!(
        strip_reader_prefix(source),
        ("(setq vm-shebang-strip 1)\n", false),
        "shebang-prefixed source should drop the first line before parsing",
    );
    assert_eq!(
        strip_reader_prefix("#!/usr/bin/env emacs --script"),
        ("", true),
        "single-line shebang files should preserve end-of-file signaling",
    );
    assert_eq!(
        strip_reader_prefix("(setq vm-shebang-strip 2)\n"),
        ("(setq vm-shebang-strip 2)\n", false),
        "non-shebang source should remain unchanged",
    );
    assert_eq!(
        strip_reader_prefix("\u{feff}(setq vm-bom-strip 3)\n"),
        ("(setq vm-bom-strip 3)\n", false),
        "utf-8 bom should be removed before parsing",
    );
    assert_eq!(
        strip_reader_prefix("\u{feff}#!/usr/bin/env emacs --script\n(setq vm-bom-shebang 4)\n"),
        ("(setq vm-bom-shebang 4)\n", false),
        "utf-8 bom should not block shebang stripping",
    );
}

#[test]
fn lexical_binding_detects_second_line_cookie_after_shebang() {
    assert!(
        lexical_binding_enabled_in_file_local_cookie_line(
            ";; -*- mode: emacs-lisp; lexical-binding: t; -*-",
        ),
        "lexical-binding cookie should be parsed from -*- metadata block",
    );
    assert!(
        !lexical_binding_enabled_in_file_local_cookie_line(
            "(setq vm-lb-false \"lexical-binding: t\")",
        ),
        "plain source text must not be treated as file-local cookie metadata",
    );
    assert!(
        !lexical_binding_enabled_in_file_local_cookie_line(";; -*- Lexical-Binding: t; -*-",),
        "cookie keys are case-sensitive in oracle behavior",
    );
    assert!(
        lexical_binding_enabled_for_source(
            "#!/usr/bin/env emacs --script\n;; -*- lexical-binding: t; -*-\n(setq vm-lb 1)\n",
        ),
        "second-line lexical-binding cookie should be honored for shebang scripts",
    );
    assert!(
        !lexical_binding_enabled_for_source(
            ";; no cookie on first line\n;; -*- lexical-binding: t; -*-\n",
        ),
        "second-line cookie should not activate lexical binding without shebang",
    );
}

#[test]
fn find_file_nonexistent() {
    assert!(find_file_in_load_path("nonexistent", &[]).is_none());
}

#[test]
fn load_path_extraction() {
    let mut ob = super::super::symbol::Obarray::new();
    ob.set_symbol_value("default-directory", Value::string("/tmp/project"));
    ob.set_symbol_value(
        "load-path",
        Value::list(vec![
            Value::string("/usr/share/emacs/lisp"),
            Value::Nil,
            Value::string("/home/user/.emacs.d"),
        ]),
    );
    let paths = get_load_path(&ob);
    assert_eq!(
        paths,
        vec![
            "/usr/share/emacs/lisp",
            "/tmp/project",
            "/home/user/.emacs.d"
        ]
    );
}

#[test]
fn find_file_with_suffix_flags() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-flags-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");

    let plain = dir.join("choice");
    let el = dir.join("choice.el");
    let elc = dir.join("choice.elc");
    fs::write(&plain, "plain").expect("write plain fixture");
    fs::write(&el, "el").expect("write el fixture");
    fs::write(&elc, "elc").expect("write elc fixture");

    let load_path = vec![dir.to_string_lossy().to_string()];

    // GNU `load` prefers compiled Elisp over source by default.
    assert_eq!(
        find_file_in_load_path_with_flags("choice", &load_path, false, false, false),
        Some(elc.clone())
    );
    // no-suffix mode only tries exact name.
    assert_eq!(
        find_file_in_load_path_with_flags("choice", &load_path, true, false, false),
        Some(plain.clone())
    );
    // must-suffix mode rejects plain file and requires suffixed one.
    assert_eq!(
        find_file_in_load_path_with_flags("choice", &load_path, false, true, false),
        Some(elc)
    );
    // no-suffix takes precedence if both flags are set.
    assert_eq!(
        find_file_in_load_path_with_flags("choice", &load_path, true, true, false),
        Some(plain)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn find_file_prefers_earlier_load_path_directory() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("neovm-load-path-order-{unique}"));
    let d1 = root.join("d1");
    let d2 = root.join("d2");
    fs::create_dir_all(&d1).expect("create d1");
    fs::create_dir_all(&d2).expect("create d2");

    let plain = d1.join("choice");
    let el = d2.join("choice.el");
    fs::write(&plain, "plain").expect("write plain fixture");
    fs::write(&el, "el").expect("write el fixture");

    let load_path = vec![
        d1.to_string_lossy().to_string(),
        d2.to_string_lossy().to_string(),
    ];
    assert_eq!(
        find_file_in_load_path_with_flags("choice", &load_path, false, false, false),
        Some(plain)
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn find_file_prefers_newer_source_when_enabled() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-prefer-newer-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");

    let elc = dir.join("choice.elc");
    let el = dir.join("choice.el");
    fs::write(&elc, "compiled").expect("write compiled fixture");
    std::thread::sleep(std::time::Duration::from_secs(1));
    fs::write(&el, "source").expect("write source fixture");

    let load_path = vec![dir.to_string_lossy().to_string()];
    assert_eq!(
        find_file_in_load_path_with_flags("choice", &load_path, false, false, false),
        Some(elc.clone())
    );
    assert_eq!(
        find_file_in_load_path_with_flags("choice", &load_path, false, false, true),
        Some(el)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_file_records_load_history() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-history-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    fs::write(&file, "(setq vm-load-history-probe t)\n").expect("write fixture");

    let mut eval = super::super::eval::Evaluator::new();
    let loaded = load_file(&mut eval, &file).expect("load file");
    assert_eq!(loaded, Value::True);

    let history = eval
        .obarray()
        .symbol_value("load-history")
        .cloned()
        .unwrap_or(Value::Nil);
    let entries = super::super::value::list_to_vec(&history).expect("load-history is a list");
    assert!(
        !entries.is_empty(),
        "load-history should have at least one entry"
    );
    let first = super::super::value::list_to_vec(&entries[0]).expect("entry is a list");
    let path_str = file.to_string_lossy().to_string();
    assert_eq!(
        first.first().and_then(Value::as_str),
        Some(path_str.as_str())
    );
    assert_eq!(
        eval.obarray().symbol_value("load-file-name").cloned(),
        Some(Value::Nil)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ensure_startup_compat_variables_backfills_xfaces_bootstrap_state() {
    let mut eval = super::super::eval::Evaluator::new();
    for name in [
        "face-filters-always-match",
        "face--new-frame-defaults",
        "face-default-stipple",
        "scalable-fonts-allowed",
        "face-ignored-fonts",
        "face-remapping-alist",
        "face-font-rescale-alist",
        "face-near-same-color-threshold",
        "face-font-lax-matched-attributes",
        "system-configuration",
        "system-configuration-options",
        "system-configuration-features",
        "operating-system-release",
        "delayed-warnings-list",
    ] {
        eval.obarray_mut().makunbound(name);
    }

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    ensure_startup_compat_variables(&mut eval, project_root);

    assert_eq!(
        eval.obarray().symbol_value("face-default-stipple").copied(),
        Some(Value::string("gray3"))
    );
    assert_eq!(
        eval.obarray()
            .symbol_value("face-near-same-color-threshold")
            .copied(),
        Some(Value::Int(30_000))
    );
    assert_eq!(
        eval.obarray()
            .symbol_value("face-font-lax-matched-attributes")
            .copied(),
        Some(Value::True)
    );
    assert!(
        eval.obarray()
            .symbol_value("system-configuration")
            .is_some_and(Value::is_string),
        "system-configuration should be backfilled to a string"
    );
    assert!(
        eval.obarray()
            .symbol_value("system-configuration-options")
            .is_some_and(Value::is_string),
        "system-configuration-options should be backfilled to a string"
    );
    assert!(
        eval.obarray()
            .symbol_value("system-configuration-features")
            .is_some_and(Value::is_string),
        "system-configuration-features should be backfilled to a string"
    );
    assert!(
        eval.obarray()
            .symbol_value("operating-system-release")
            .is_some_and(|value| value.is_nil() || value.is_string()),
        "operating-system-release should be backfilled to nil or a string"
    );
    assert_eq!(
        eval.obarray()
            .symbol_value("delayed-warnings-list")
            .copied(),
        Some(Value::Nil)
    );

    let table = eval
        .obarray()
        .symbol_value("face--new-frame-defaults")
        .copied()
        .expect("face hash table backfilled");
    let Value::HashTable(id) = table else {
        panic!("face--new-frame-defaults must be a hash table");
    };
    let test = with_heap(|heap| heap.get_hash_table(id).test.clone());
    assert_eq!(test, HashTableTest::Eq);
}

#[test]
fn nested_load_restores_parent_load_file_name() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-file-name-nested-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let parent = dir.join("parent.el");
    let child = dir.join("child.el");

    fs::write(
        &parent,
        "(setq vm-parent-seen load-file-name)\n\
         (load (expand-file-name \"child\" (file-name-directory load-file-name)) nil 'nomessage)\n\
         (setq vm-parent-after-child load-file-name)\n",
    )
    .expect("write parent fixture");
    fs::write(&child, "(setq vm-child-seen load-file-name)\n").expect("write child fixture");

    let mut eval = super::super::eval::Evaluator::new();
    let loaded = load_file(&mut eval, &parent).expect("load parent fixture");
    assert_eq!(loaded, Value::True);

    let parent_str = parent.to_string_lossy().to_string();
    let child_str = child.to_string_lossy().to_string();
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-parent-seen")
            .and_then(Value::as_str),
        Some(parent_str.as_str())
    );
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-child-seen")
            .and_then(Value::as_str),
        Some(child_str.as_str())
    );
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-parent-after-child")
            .and_then(Value::as_str),
        Some(parent_str.as_str())
    );
    assert_eq!(
        eval.obarray().symbol_value("load-file-name").cloned(),
        Some(Value::Nil),
        "load-file-name should be restored after top-level load",
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_file_accepts_shebang_and_honors_second_line_lexical_binding_cookie() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-shebang-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    fs::write(
        &file,
        "#!/usr/bin/env emacs --script\n\
         ;; -*- lexical-binding: t; -*-\n\
         (setq vm-load-shebang-probe lexical-binding)\n\
         (setq vm-load-shebang-fn (let ((x 41)) (lambda () (+ x 1))))\n",
    )
    .expect("write shebang fixture");

    let mut eval = super::super::eval::Evaluator::new();
    let loaded = load_file(&mut eval, &file).expect("load shebang fixture");
    assert_eq!(loaded, Value::True);
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-load-shebang-probe")
            .cloned(),
        Some(Value::True),
        "second-line lexical-binding cookie should set lexical-binding to t during load",
    );

    let call = super::super::parser::parse_forms(
        "(let ((lexical-binding nil)) (funcall vm-load-shebang-fn))",
    )
    .expect("parse call fixture");
    let value = eval.eval_expr(&call[0]).expect("evaluate closure");
    assert_eq!(
        value.as_int(),
        Some(42),
        "closure should capture lexical scope from loaded file",
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_file_does_not_enable_lexical_binding_from_non_cookie_second_line_text() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-shebang-noncookie-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    fs::write(
        &file,
        "#!/usr/bin/env emacs --script\n\
         (setq vm-load-shebang-false-string \"lexical-binding: t\")\n\
         (setq vm-load-shebang-false-probe lexical-binding)\n\
         (setq vm-load-shebang-false-fn (let ((x 41)) (lambda () (+ x 1))))\n",
    )
    .expect("write shebang non-cookie fixture");

    let mut eval = super::super::eval::Evaluator::new();
    let loaded = load_file(&mut eval, &file).expect("load shebang non-cookie fixture");
    assert_eq!(loaded, Value::True);
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-load-shebang-false-probe")
            .cloned(),
        Some(Value::Nil),
        "non-cookie second-line text must not flip lexical-binding to t",
    );

    let call = super::super::parser::parse_forms(
        "(condition-case err (let ((lexical-binding nil)) (funcall vm-load-shebang-false-fn)) (error (list 'error (car err))))",
    )
    .expect("parse call fixture");
    let value = eval
        .eval_expr(&call[0])
        .expect("evaluate closure failure probe");
    let payload = super::super::value::list_to_vec(&value).expect("expected error payload list");
    assert_eq!(
        payload,
        vec![Value::symbol("error"), Value::symbol("void-variable")],
        "without lexical-binding cookie, closure must not capture lexical locals",
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_file_accepts_utf8_bom_prefixed_source() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-bom-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    fs::write(
        &file,
        "\u{feff}(setq vm-load-bom-probe 'ok)\n(setq vm-load-bom-flag t)\n",
    )
    .expect("write bom fixture");

    let mut eval = super::super::eval::Evaluator::new();
    let loaded = load_file(&mut eval, &file).expect("load bom fixture");
    assert_eq!(loaded, Value::True);
    assert_eq!(
        eval.obarray().symbol_value("vm-load-bom-probe").cloned(),
        Some(Value::symbol("ok")),
        "utf-8 bom should be ignored by reader before first form",
    );
    assert_eq!(
        eval.obarray().symbol_value("vm-load-bom-flag").cloned(),
        Some(Value::True)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_file_single_line_shebang_signals_end_of_file() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-shebang-eof-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    fs::write(&file, "#!/usr/bin/env emacs --script").expect("write shebang-only fixture");

    let mut eval = super::super::eval::Evaluator::new();
    let err = load_file(&mut eval, &file).expect_err("shebang-only source should signal EOF");
    match err {
        EvalError::Signal { symbol, data } => {
            assert_eq!(resolve_sym(symbol), "end-of-file");
            assert!(data.is_empty());
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_file_writes_and_invalidates_neoc_cache() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-neoc-cache-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    let source_v1 = "(setq vm-load-cache-probe 'v1)\n";
    fs::write(&file, source_v1).expect("write source fixture");

    let mut eval = super::super::eval::Evaluator::new();
    let loaded = load_file(&mut eval, &file).expect("load source file");
    assert_eq!(loaded, Value::True);
    assert_eq!(
        eval.obarray().symbol_value("vm-load-cache-probe").cloned(),
        Some(Value::symbol("v1"))
    );

    let cache = cache_sidecar_path(&file);
    assert!(
        cache.exists(),
        "source load should create .neoc sidecar cache"
    );
    let cache_v1 = fs::read_to_string(&cache).expect("read cache v1");
    assert!(
        cache_v1.contains(&format!("key={}", cache_key(false))),
        "cache key should include lexical-binding dimension",
    );
    assert!(
        cache_v1.contains(&format!("source-hash={:016x}", source_hash(source_v1))),
        "cache should carry source hash invalidation key",
    );

    let source_v2 = ";;; -*- lexical-binding: t; -*-\n(setq vm-load-cache-probe 'v2)\n";
    fs::write(&file, source_v2).expect("write source fixture v2");

    let loaded = load_file(&mut eval, &file).expect("reload source file");
    assert_eq!(loaded, Value::True);
    assert_eq!(
        eval.obarray().symbol_value("vm-load-cache-probe").cloned(),
        Some(Value::symbol("v2"))
    );
    let cache_v2 = fs::read_to_string(&cache).expect("read cache v2");
    assert_ne!(cache_v1, cache_v2, "cache must refresh when source changes");
    assert!(
        cache_v2.contains(&format!("key={}", cache_key(true))),
        "cache key should update when lexical-binding dimension changes",
    );
    assert!(
        cache_v2.contains(&format!("source-hash={:016x}", source_hash(source_v2))),
        "cache hash should update when source text changes",
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_file_ignores_corrupt_neoc_cache_and_loads_source() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-neoc-corrupt-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    fs::write(&file, "(setq vm-load-corrupt-neoc 'ok)\n").expect("write source fixture");
    let cache = cache_sidecar_path(&file);
    fs::write(&cache, "corrupt-neoc-cache").expect("write corrupt cache");

    let mut eval = super::super::eval::Evaluator::new();
    let loaded = load_file(&mut eval, &file).expect("load should ignore corrupt cache");
    assert_eq!(loaded, Value::True);
    assert_eq!(
        eval.obarray().symbol_value("vm-load-corrupt-neoc").cloned(),
        Some(Value::symbol("ok"))
    );
    let rewritten = fs::read_to_string(&cache).expect("cache should be rewritten");
    assert!(
        rewritten.starts_with(ELISP_CACHE_MAGIC),
        "rewritten cache should have expected header",
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_file_ignores_cache_write_failures_before_write() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-neoc-write-fail-pre-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    fs::write(&file, "(setq vm-load-neoc-write-fail-pre 'ok)\n").expect("write source fixture");

    let _guard = CacheWriteFailGuard::set(CACHE_WRITE_PHASE_BEFORE_WRITE);
    let mut eval = super::super::eval::Evaluator::new();
    let loaded =
        load_file(&mut eval, &file).expect("load should succeed despite cache write failure");
    assert_eq!(loaded, Value::True);
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-load-neoc-write-fail-pre")
            .cloned(),
        Some(Value::symbol("ok"))
    );
    assert!(
        !cache_sidecar_path(&file).exists(),
        "cache should be absent when write fails before cache file creation",
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_file_cleans_tmp_after_cache_write_failure_before_rename() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-neoc-write-fail-post-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    fs::write(&file, "(setq vm-load-neoc-write-fail-post 'ok)\n").expect("write source fixture");

    let _guard = CacheWriteFailGuard::set(CACHE_WRITE_PHASE_AFTER_WRITE);
    let mut eval = super::super::eval::Evaluator::new();
    let loaded =
        load_file(&mut eval, &file).expect("load should succeed despite cache rename failure");
    assert_eq!(loaded, Value::True);
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-load-neoc-write-fail-post")
            .cloned(),
        Some(Value::symbol("ok"))
    );
    assert!(
        !cache_sidecar_path(&file).exists(),
        "cache should be absent when failure happens before rename",
    );
    assert!(
        !cache_temp_path(&file).exists(),
        "temporary cache file should be cleaned after write failure",
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_elc_is_supported() {
    // .elc files are now supported. A valid .elc with a simple setq should work.
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-elc-supported-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let compiled = dir.join("probe.elc");
    // Write a minimal .elc with valid Elisp content (no magic header — just a setq).
    fs::write(&compiled, "(setq vm-elc-loaded t)\n").expect("write compiled fixture");

    let mut eval = super::super::eval::Evaluator::new();
    let result = load_file(&mut eval, &compiled);
    assert!(
        result.is_ok(),
        "load should accept .elc: {:?}",
        result.err()
    );
    assert_eq!(
        eval.obarray().symbol_value("vm-elc-loaded").cloned(),
        Some(Value::True),
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_elc_gz_is_rejected() {
    // .elc.gz files are still unsupported.
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-elcgz-rejected-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let compiled = dir.join("probe.elc.gz");
    fs::write(&compiled, "gzipped-data").expect("write compiled fixture");

    let mut eval = super::super::eval::Evaluator::new();
    let err = load_file(&mut eval, &compiled).expect_err("load should reject .elc.gz");
    match err {
        EvalError::Signal { symbol, .. } => assert_eq!(resolve_sym(symbol), "error"),
        other => panic!("unexpected error: {other:?}"),
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn find_file_surfaces_elc_only_artifact_as_explicit_unsupported_load_target() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-elc-only-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");

    let compiled = dir.join("module.elc");
    fs::write(&compiled, "compiled").expect("write compiled fixture");

    let load_path = vec![dir.to_string_lossy().to_string()];
    let found = find_file_in_load_path_with_flags("module", &load_path, false, false, false);
    assert_eq!(found, Some(compiled));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_elc_gz_is_explicitly_unsupported() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-elc-gz-unsupported-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let compiled = dir.join("probe.elc.gz");
    fs::write(&compiled, "compiled-data").expect("write compiled fixture");

    let mut eval = super::super::eval::Evaluator::new();
    let err = load_file(&mut eval, &compiled).expect_err("load should reject .elc.gz");
    match err {
        EvalError::Signal { symbol, .. } => assert_eq!(resolve_sym(symbol), "error"),
        other => panic!("unexpected error: {other:?}"),
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn precompile_source_file_writes_deterministic_cache() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-precompile-deterministic-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let source = dir.join("probe.el");
    fs::write(
        &source,
        ";;; -*- lexical-binding: t; -*-\n(setq vm-precompile-probe '(1 2 3))\n",
    )
    .expect("write source fixture");

    let cache_path_1 = precompile_source_file(&source).expect("first precompile should succeed");
    let cache_v1 = fs::read_to_string(&cache_path_1).expect("read cache v1");
    let cache_path_2 = precompile_source_file(&source).expect("second precompile should succeed");
    let cache_v2 = fs::read_to_string(&cache_path_2).expect("read cache v2");

    assert_eq!(cache_path_1, cache_path_2, "cache path should be stable");
    assert_eq!(
        cache_v1, cache_v2,
        "precompile output should be deterministic"
    );
    assert!(
        cache_v1.contains("lexical=1"),
        "lexical-binding should be reflected in cache key",
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn precompile_source_file_rejects_compiled_inputs() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-precompile-reject-elc-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let compiled = dir.join("probe.elc");
    fs::write(&compiled, "compiled").expect("write compiled fixture");

    let err = precompile_source_file(&compiled).expect_err("elc input should be rejected");
    match err {
        EvalError::Signal { symbol, .. } => assert_eq!(resolve_sym(symbol), "file-error"),
        other => panic!("unexpected error: {other:?}"),
    }

    let _ = fs::remove_dir_all(&dir);
}

/// Try loading the full loadup.el file sequence through the NeoVM
/// evaluator.  This test runs by default.  Set
/// NEOVM_LOADUP_TEST_SKIP=1 to skip it.
#[test]
fn neovm_loadup_bootstrap() {
    if std::env::var("NEOVM_LOADUP_TEST_SKIP").as_deref() == Ok("1") {
        tracing::info!("skipping neovm_loadup_bootstrap (NEOVM_LOADUP_TEST_SKIP=1)");
        return;
    }

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();

    let mut eval = create_bootstrap_evaluator().expect("loadup bootstrap should succeed");
    let form = crate::emacs_core::parser::parse_forms(
        "(list (not (null (cl--find-class 'float))) (not (null (cl--find-class 'integer))))",
    )
    .expect("parse cl class probe");
    let result = eval.eval_expr(&form[0]).expect("evaluate cl class probe");
    let items = crate::emacs_core::value::list_to_vec(&result).expect("result list");
    assert_eq!(
        items,
        vec![Value::True, Value::True],
        "expected float/integer CL classes to be registered, got {result}"
    );

    let float_pred = eval
        .obarray()
        .get_property("float", "cl-deftype-satisfies")
        .copied();
    let integer_pred = eval
        .obarray()
        .get_property("integer", "cl-deftype-satisfies")
        .copied();
    assert!(
        float_pred.is_some_and(|v| !v.is_nil()),
        "expected float cl-deftype-satisfies property to be non-nil, got {float_pred:?}"
    );
    assert!(
        integer_pred.is_some_and(|v| !v.is_nil()),
        "expected integer cl-deftype-satisfies property to be non-nil, got {integer_pred:?}"
    );

    let compat_probe = crate::emacs_core::parser::parse_forms(
        "(list (coding-system-p 'iso-8859-15) (stringp system-configuration-features))",
    )
    .expect("parse startup compatibility probe");
    let compat_result = eval
        .eval_expr(&compat_probe[0])
        .expect("evaluate startup compatibility probe");
    let compat_items =
        crate::emacs_core::value::list_to_vec(&compat_result).expect("compat probe result list");
    assert_eq!(
        compat_items,
        vec![Value::True, Value::True],
        "expected iso-8859-15 and system-configuration-features to be available, got {compat_result}"
    );
}

#[test]
fn compiled_bootstrap_cl_preload_stubs_work_after_faces() {
    let mut eval = partial_bootstrap_eval_until("!bootstrap-cl-preloaded-stubs", true);
    let stubs = [
        "(defmacro cl--find-class (type) `(get ,type 'cl--class))",
        "(defun cl--builtin-type-p (name) nil)",
        "(defun cl--struct-name-p (name) (and name (symbolp name) (not (keywordp name))))",
        "(defvar cl-struct-cl-structure-object-tags nil)",
        "(defvar cl--struct-default-parent nil)",
        "(defun cl-struct-define (name docstring parent type named slots children-sym tag print) (when children-sym (if (boundp children-sym) (add-to-list children-sym tag) (set children-sym (list tag)))))",
        "(defun cl--define-derived-type (name expander predicate &optional parents) nil)",
        "(defmacro cl-function (func) `(function ,func))",
    ];

    let mut failures = Vec::new();
    for stub in stubs {
        let forms = crate::emacs_core::parser::parse_forms(stub).expect("parse stub");
        for result in eval.eval_forms(&forms) {
            if let Err(err) = result {
                failures.push(format!("{stub} => {}", format_eval_error(&eval, &err)));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "compiled bootstrap should accept cl preload stubs after faces: {failures:#?}"
    );
}

#[test]
fn source_cl_lib_loads_after_early_gv_without_bootstrap_gv_stubs() {
    let mut eval = partial_bootstrap_eval_until("!bootstrap-cl-preloaded-stubs", false);
    let rendered = eval_rendered(
        &mut eval,
        r#"(condition-case err
               (progn
                 (list (featurep 'gv)
                       (macrop 'gv-define-expander)
                       (macrop 'gv-define-setter)
                       (macrop 'gv-define-simple-setter)
                       (require 'cl-lib)
                       (featurep 'cl-lib)
                       (autoloadp (symbol-function 'cl-subseq))
                       (macrop 'setf)))
             (error err))"#,
    );
    assert_eq!(rendered, "OK (t t t t cl-lib t t t)");
}

#[test]
fn compiled_cl_preloaded_loads_after_faces() {
    let mut eval = partial_bootstrap_eval_until("emacs-lisp/cl-preloaded", true);
    let load_path = get_load_path(&eval.obarray());
    let path = bootstrap_fixture_path(&load_path, "emacs-lisp/cl-preloaded", true)
        .expect("compiled cl-preloaded fixture path");

    load_file(&mut eval, &path).unwrap_or_else(|err| {
        panic!(
            "failed loading emacs-lisp/cl-preloaded from {}: {}",
            path.display(),
            format_eval_error(&eval, &err)
        )
    });

    let probe = crate::emacs_core::parser::parse_forms("(fboundp 'built-in-class--make)")
        .expect("parse built-in-class probe");
    let result = eval
        .eval_expr(&probe[0])
        .expect("evaluate built-in-class constructor probe");
    assert_eq!(result, Value::True);
}

#[test]
fn source_cycle_spacing_form_loads_after_bootstrap_prefix() {
    let mut eval = partial_bootstrap_eval_until("simple", false);
    let load_path = get_load_path(&eval.obarray());
    let path = bootstrap_fixture_path(&load_path, "simple", false).expect("simple.el path");
    let content = std::fs::read_to_string(&path).expect("read simple.el");
    let forms = parse_source_forms(&path, &content).expect("parse simple.el");

    let cycle_spacing_form = forms
        .get(89)
        .expect("cycle-spacing source bootstrap form")
        .clone();
    assert!(
        print_expr(&cycle_spacing_form).starts_with("(defun cycle-spacing"),
        "unexpected simple.el FORM[89]: {}",
        print_expr(&cycle_spacing_form)
    );

    let subset_source = format!(
        ";;; cycle-spacing-subset.el --- focused bootstrap slice -*- lexical-binding: t; -*-\n\n{}\n\n{}\n\n{}\n",
        print_expr(&forms[87]),
        print_expr(&forms[88]),
        print_expr(&forms[89]),
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let subset_path = dir.path().join("cycle-spacing-subset.el");
    std::fs::write(&subset_path, subset_source).expect("write cycle-spacing subset");

    load_file(&mut eval, &subset_path).unwrap_or_else(|err| {
        panic!(
            "failed loading focused cycle-spacing subset from {}: {}",
            subset_path.display(),
            format_eval_error(&eval, &err)
        )
    });

    let probe = crate::emacs_core::parser::parse_forms(
        "(list (boundp 'cycle-spacing--context) (fboundp 'cycle-spacing))",
    )
    .expect("parse cycle-spacing probe");
    let result = eval
        .eval_expr(&probe[0])
        .expect("evaluate cycle-spacing probe");
    assert_eq!(result, Value::list(vec![Value::True, Value::True]));
}

#[test]
fn compiled_characters_loads_after_case_table() {
    let mut eval = partial_bootstrap_eval_until("international/characters", true);
    let load_path = get_load_path(&eval.obarray());
    let path = bootstrap_fixture_path(&load_path, "international/characters", true)
        .expect("compiled international/characters fixture path");

    load_file(&mut eval, &path).unwrap_or_else(|err| {
        panic!(
            "failed loading international/characters from {}: {}",
            path.display(),
            format_eval_error(&eval, &err)
        )
    });
}

#[test]
fn source_chinese_loads_after_composite() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();

    let mut eval = partial_bootstrap_eval_until("language/chinese", false);
    let load_path = get_load_path(&eval.obarray());
    let path = bootstrap_fixture_path(&load_path, "language/chinese", false)
        .expect("source language/chinese fixture path");

    load_file(&mut eval, &path).unwrap_or_else(|err| {
        panic!(
            "failed loading language/chinese from {}: {}",
            path.display(),
            format_eval_error(&eval, &err)
        )
    });
}

#[test]
fn define_prefix_command_sets_symbol_value_and_function() {
    let mut eval = partial_bootstrap_eval_until("keymap", false);
    let probe = crate::emacs_core::parser::parse_forms(
        r#"(let ((cmd 'neovm--test-prefix-map))
             (define-prefix-command cmd nil "Test Prefix")
             (list (eq cmd 'neovm--test-prefix-map)
                   (keymapp (symbol-function cmd))
                   (keymapp (symbol-value cmd))))"#,
    )
    .expect("parse define-prefix-command probe");
    let result = eval
        .eval_expr(&probe[0])
        .expect("evaluate define-prefix-command probe");
    assert_eq!(
        crate::emacs_core::value::list_to_vec(&result).expect("probe result list"),
        vec![Value::True, Value::True, Value::True]
    );
}

#[test]
fn lookup_key_returned_submenu_symbol_has_bound_value() {
    let mut eval = partial_bootstrap_eval_until("keymap", false);
    let probe = crate::emacs_core::parser::parse_forms(
        r#"(let* ((root (make-sparse-keymap))
                  (submenu 'describe-chinese-environment-map))
             (define-prefix-command submenu nil "Chinese Environment")
             (define-key-after root (vector 'Chinese) (cons "Chinese" submenu))
             (let ((found (lookup-key root [Chinese])))
               (list (eq found submenu)
                     (keymapp (symbol-value found)))))"#,
    )
    .expect("parse lookup-key submenu probe");
    let result = eval
        .eval_expr(&probe[0])
        .expect("evaluate lookup-key submenu probe");
    assert_eq!(
        crate::emacs_core::value::list_to_vec(&result).expect("probe result list"),
        vec![Value::True, Value::True]
    );
}

#[test]
fn set_language_info_alist_reuses_chinese_submenu_like_gnu_emacs() {
    let mut eval = partial_bootstrap_eval_until("language/chinese", false);
    let probe = crate::emacs_core::parser::parse_forms(
        r#"(progn
             (set-language-info-alist
              "Chinese-GB"
              '((documentation . "GB"))
              '("Chinese"))
             (set-language-info-alist
              "Chinese-BIG5"
              '((documentation . "BIG5"))
              '("Chinese"))
             (keymapp describe-chinese-environment-map))"#,
    )
    .expect("parse set-language-info-alist submenu probe");
    let result = eval
        .eval_expr(&probe[0])
        .expect("evaluate set-language-info-alist submenu probe");
    assert_eq!(result, Value::True);
}

#[test]
fn bootstrap_load_sequence_includes_gnu_x_term_layer_after_tool_bar() {
    let tool_bar_idx = BOOTSTRAP_LOAD_SEQUENCE
        .iter()
        .position(|name| *name == "tool-bar")
        .expect("tool-bar bootstrap entry");
    let touch_screen_idx = BOOTSTRAP_LOAD_SEQUENCE
        .iter()
        .position(|name| *name == "touch-screen")
        .expect("touch-screen bootstrap entry");
    let x_dnd_idx = BOOTSTRAP_LOAD_SEQUENCE
        .iter()
        .position(|name| *name == "x-dnd")
        .expect("x-dnd bootstrap entry");
    let x_idx = BOOTSTRAP_LOAD_SEQUENCE
        .iter()
        .position(|name| *name == "!load-x-win")
        .expect("x bootstrap sentinel");
    assert_eq!(touch_screen_idx, tool_bar_idx + 1);
    assert_eq!(x_dnd_idx, touch_screen_idx + 1);
    assert_eq!(x_idx, x_dnd_idx + 1);
}

#[test]
fn partial_bootstrap_fill_delete_newlines_matches_gnu_trailing_space_behavior() {
    let mut eval = partial_bootstrap_eval_until("tool-bar", false);
    let load_path = get_load_path(&eval.obarray());
    let fill_path =
        bootstrap_fixture_path(&load_path, "textmodes/fill", false).expect("fill fixture path");
    load_file(&mut eval, &fill_path).unwrap_or_else(|err| {
        panic!(
            "failed loading fill.el from {}: {}",
            fill_path.display(),
            format_eval_error(&eval, &err)
        )
    });

    let forms = parse_forms(
        r#"(with-temp-buffer
             (insert "Enable the mode if ARG is nil, omitted, or is a positive number.\n")
             (insert "Disable the mode if ARG is a negative number.\n")
             (let ((to (copy-marker (point) t)))
               (fill-delete-newlines (point-min) to 'left t nil)
               (buffer-string)))"#,
    )
    .expect("parse fill-delete-newlines regression");
    let result = eval
        .eval_forms(&forms)
        .into_iter()
        .last()
        .expect("one form")
        .expect("evaluation succeeds");

    assert_eq!(
        format_eval_result(&Ok(result)),
        r#"OK "Enable the mode if ARG is nil, omitted, or is a positive number.  Disable the mode if ARG is a negative number. ""#
    );
}

#[test]
fn bootstrap_tool_bar_mode_comes_from_gnu_mode_macro_path() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();

    tracing::info!("tool-bar probe: begin partial bootstrap");
    let mut eval = partial_bootstrap_eval_until("tool-bar", false);
    tracing::info!("tool-bar probe: partial bootstrap complete");
    let load_path = get_load_path(&eval.obarray());
    let easy_mmode_path = bootstrap_fixture_path(&load_path, "emacs-lisp/easy-mmode", false)
        .expect("easy-mmode fixture path");
    tracing::info!("tool-bar probe: loading {}", easy_mmode_path.display());
    load_file(&mut eval, &easy_mmode_path).unwrap_or_else(|err| {
        panic!(
            "failed loading easy-mmode from {}: {}",
            easy_mmode_path.display(),
            format_eval_error(&eval, &err)
        )
    });
    tracing::info!("tool-bar probe: easy-mmode load complete");
    let tool_bar_path =
        bootstrap_fixture_path(&load_path, "tool-bar", false).expect("tool-bar fixture path");
    tracing::info!("tool-bar probe: loading {}", tool_bar_path.display());
    let source = fs::read_to_string(&tool_bar_path).expect("read tool-bar source");
    let top_level_forms =
        crate::emacs_core::parser::parse_forms(&source).expect("parse tool-bar source");
    for (label, src) in [
        (
            "pretty-name",
            r#"(easy-mmode-pretty-mode-name 'tool-bar-mode nil)"#,
        ),
        (
            "docstring-arg-check",
            r#"(string-match-p
                 "\\bARG\\b"
                 "Toggle the tool bar in all graphical frames (Tool Bar mode).\n\nSee `tool-bar-add-item' and `tool-bar-add-item-from-menu' for\nconveniently adding tool bar items.")"#,
        ),
        (
            "argdoc-format",
            r#"(let* ((mode-pretty-name "Tool-Bar mode")
                      (getter 'tool-bar-mode)
                      (global t)
                      (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                      (fill-column (if (integerp docs-fc) docs-fc 65))
                      (argdoc (format
                               easy-mmode--arg-docstring
                               (if global "global " "")
                               mode-pretty-name
                               (concat
                                (if (symbolp getter) "the variable ")
                                (format "`%s'"
                                        (string-replace "'" "\\='" (format "%S" getter)))))))
                 argdoc)"#,
        ),
        (
            "ensure-empty-lines-basic",
            r#"(with-temp-buffer
                 (insert "Toggle the tool bar in all graphical frames (Tool Bar mode).")
                 (ensure-empty-lines)
                 (buffer-string))"#,
        ),
        (
            "forward-paragraph-basic",
            r#"(with-temp-buffer
                 (insert "Enable the mode if ARG is nil, omitted, or is a positive number.\n")
                 (insert "Disable the mode if ARG is a negative number.\n")
                 (goto-char (point-min))
                 (forward-paragraph 1)
                 (point))"#,
        ),
        (
            "fill-delete-newlines-basic",
            r#"(with-temp-buffer
                 (insert "Enable the mode if ARG is nil, omitted, or is a positive number.\n")
                 (insert "Disable the mode if ARG is a negative number.\n")
                 (let ((to (copy-marker (point) t)))
                   (fill-delete-newlines (point-min) to 'left t nil)
                   (buffer-string)))"#,
        ),
        (
            "fill-move-to-break-point-basic",
            r#"(with-temp-buffer
                 (insert "Enable the mode if ARG is nil, omitted, or is a positive number.\n")
                 (insert "Disable the mode if ARG is a negative number.\n")
                 (let ((to (copy-marker (point) t)))
                   (fill-delete-newlines (point-min) to 'left t nil)
                   (goto-char (point-min))
                   (let ((linebeg (point)))
                     (move-to-column (current-fill-column))
                     (unless (> (current-column) (current-fill-column))
                       (forward-char 1))
                     (fill-move-to-break-point linebeg)
                     (list (point) (current-column) (buffer-string)))))"#,
        ),
        (
            "fill-newline-basic",
            r#"(with-temp-buffer
                 (insert "Enable the mode if ARG is nil, omitted, or is a positive number.\n")
                 (insert "Disable the mode if ARG is a negative number.\n")
                 (let ((to (copy-marker (point) t)))
                   (fill-delete-newlines (point-min) to 'left t nil)
                   (goto-char (point-min))
                   (let ((linebeg (point)))
                     (move-to-column (current-fill-column))
                     (unless (> (current-column) (current-fill-column))
                       (forward-char 1))
                     (fill-move-to-break-point linebeg)
                     (fill-newline)
                     (list (point) (current-column) (buffer-string)))))"#,
        ),
        (
            "fill-second-iteration-setup",
            r#"(with-temp-buffer
                 (insert "Enable the mode if ARG is nil, omitted, or is a positive number.\n")
                 (insert "Disable the mode if ARG is a negative number.\n")
                 (goto-char (point-min))
                 (let* ((from (point))
                        (to (progn
                              (goto-char (point-max))
                              (copy-marker (point) t))))
                   (fill-delete-newlines from to 'left t nil)
                   (goto-char from)
                   (let ((linebeg (point)))
                     (move-to-column (current-fill-column))
                     (unless (> (current-column) (current-fill-column))
                       (forward-char 1))
                     (fill-move-to-break-point linebeg)
                     (skip-chars-forward " \t")
                     (fill-newline))
                   (let ((linebeg (point)))
                     (move-to-column (current-fill-column))
                     (format "%S"
                             (list :point (point)
                                   :column (current-column)
                                   :to (marker-position to)
                                   :linebeg linebeg
                                   :text (buffer-string))))))"#,
        ),
        (
            "fill-region-as-paragraph-basic",
            r#"(with-temp-buffer
                 (let ((start (point)))
                   (insert "Enable the mode if ARG is nil, omitted, or is a positive number.\n")
                   (insert "Disable the mode if ARG is a negative number.\n")
                   (fill-region-as-paragraph start (point) 'left t)
                   (buffer-string)))"#,
        ),
        (
            "fill-region-basic",
            r#"(with-temp-buffer
                 (let ((start (point)))
                   (insert "Enable the mode if ARG is nil, omitted, or is a positive number.\n")
                   (insert "Disable the mode if ARG is a negative number.\n")
                   (fill-region start (point) 'left t))
                 (buffer-string))"#,
        ),
        (
            "docstring-forward-paragraph-boundary",
            r#"(with-temp-buffer
                 (let* ((fill-prefix nil)
                        (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                        (fill-column (if (integerp docs-fc) docs-fc 65))
                        (argdoc (format
                                 easy-mmode--arg-docstring
                                 "global "
                                 "Tool-Bar mode"
                                 "the variable `tool-bar-mode'")))
                   (insert argdoc)
                   (goto-char (point-min))
                   (let ((initial (point))
                         (max (copy-marker (point-max) t)))
                     (fill-forward-paragraph 1)
                     (let ((end (min max (point)))
                           (after-forward (point)))
                       (fill-forward-paragraph -1)
                       (list :initial initial
                             :after-forward after-forward
                             :end end
                             :beg (point))))))"#,
        ),
        (
            "docstring-first-paragraph-fill",
            r#"(with-temp-buffer
                 (let* ((fill-prefix nil)
                        (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                        (fill-column (if (integerp docs-fc) docs-fc 65))
                        (argdoc (format
                                 easy-mmode--arg-docstring
                                 "global "
                                 "Tool-Bar mode"
                                 "the variable `tool-bar-mode'")))
                   (insert argdoc)
                   (goto-char (point-min))
                   (let ((end (save-excursion
                                (fill-forward-paragraph 1)
                                (point))))
                     (fill-region-as-paragraph (point) end 'left t)
                     (list :point (point)
                           :end end
                           :text (buffer-string)))))"#,
        ),
        (
            "docstring-second-paragraph-boundary",
            r#"(with-temp-buffer
                 (let* ((fill-prefix nil)
                        (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                        (fill-column (if (integerp docs-fc) docs-fc 65))
                        (argdoc (format
                                 easy-mmode--arg-docstring
                                 "global "
                                 "Tool-Bar mode"
                                 "the variable `tool-bar-mode'")))
                   (insert argdoc)
                   (goto-char (point-min))
                   (let* ((max (copy-marker (point-max) t))
                          (first-end (save-excursion
                                       (fill-forward-paragraph 1)
                                       (point))))
                     (fill-region-as-paragraph (point) first-end 'left t)
                     (let ((initial (point)))
                       (fill-forward-paragraph 1)
                       (let ((second-end (min max (point)))
                             (after-forward (point)))
                         (fill-forward-paragraph -1)
                         (list :initial initial
                               :after-forward after-forward
                               :second-end second-end
                               :beg (point)
                               :max (marker-position max)
                               :text (buffer-string)))))))"#,
        ),
        (
            "docstring-second-paragraph-post-delete",
            r#"(with-temp-buffer
                 (let* ((fill-prefix nil)
                        (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                        (fill-column (if (integerp docs-fc) docs-fc 65))
                        (argdoc (format
                                 easy-mmode--arg-docstring
                                 "global "
                                 "Tool-Bar mode"
                                 "the variable `tool-bar-mode'")))
                   (insert argdoc)
                   (goto-char (point-min))
                   (let* ((first-end (save-excursion
                                       (fill-forward-paragraph 1)
                                       (point))))
                     (fill-region-as-paragraph (point) first-end 'left t)
                     (let ((from (point))
                           (to (save-excursion
                                 (fill-forward-paragraph 1)
                                 (copy-marker (point) t))))
                       (fill-delete-newlines from to 'left t nil)
                       (list :point (point)
                             :from from
                             :to (marker-position to)
                             :text (buffer-string))))))"#,
        ),
        (
            "docstring-second-paragraph-first-iteration",
            r#"(with-temp-buffer
                 (let* ((fill-prefix nil)
                        (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                        (fill-column (if (integerp docs-fc) docs-fc 65))
                        (argdoc (format
                                 easy-mmode--arg-docstring
                                 "global "
                                 "Tool-Bar mode"
                                 "the variable `tool-bar-mode'")))
                   (insert argdoc)
                   (goto-char (point-min))
                   (let* ((first-end (save-excursion
                                       (fill-forward-paragraph 1)
                                       (point))))
                     (fill-region-as-paragraph (point) first-end 'left t)
                     (let ((from (point))
                           (to (save-excursion
                                 (fill-forward-paragraph 1)
                                 (copy-marker (point) t))))
                       (fill-delete-newlines from to 'left t nil)
                       (goto-char from)
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (let ((after-move (point))
                               (after-move-col (current-column)))
                           (unless (> (current-column) (current-fill-column))
                             (forward-char 1))
                           (let ((after-forward (point))
                                 (after-forward-col (current-column)))
                             (fill-move-to-break-point linebeg)
                             (let ((after-break (point))
                                   (after-break-col (current-column)))
                               (skip-chars-forward " \t")
                               (list :linebeg linebeg
                                     :to (marker-position to)
                                     :after-move after-move
                                     :after-move-col after-move-col
                                     :after-forward after-forward
                                     :after-forward-col after-forward-col
                                     :after-break after-break
                                     :after-break-col after-break-col
                                     :after-skip (point)
                                     :after-skip-col (current-column)
                                     :before-end (< (point) to)
                                     :text (buffer-string))))))))))"#,
        ),
        (
            "docstring-second-paragraph-first-cut",
            r#"(with-temp-buffer
                 (let* ((fill-prefix nil)
                        (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                        (fill-column (if (integerp docs-fc) docs-fc 65))
                        (argdoc (format
                                 easy-mmode--arg-docstring
                                 "global "
                                 "Tool-Bar mode"
                                 "the variable `tool-bar-mode'")))
                   (insert argdoc)
                   (goto-char (point-min))
                   (let* ((first-end (save-excursion
                                       (fill-forward-paragraph 1)
                                       (point))))
                     (fill-region-as-paragraph (point) first-end 'left t)
                     (let ((from (point))
                           (to (save-excursion
                                 (fill-forward-paragraph 1)
                                 (copy-marker (point) t))))
                       (fill-delete-newlines from to 'left t nil)
                       (goto-char from)
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (unless (> (current-column) (current-fill-column))
                           (forward-char 1))
                         (fill-move-to-break-point linebeg)
                         (skip-chars-forward " \t")
                         (fill-newline)
                         (list :point (point)
                               :to (marker-position to)
                               :linebeg linebeg
                               :text (buffer-string)))))))"#,
        ),
        (
            "docstring-second-paragraph-second-iteration-setup",
            r#"(with-temp-buffer
                 (let* ((fill-prefix nil)
                        (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                        (fill-column (if (integerp docs-fc) docs-fc 65))
                        (argdoc (format
                                 easy-mmode--arg-docstring
                                 "global "
                                 "Tool-Bar mode"
                                 "the variable `tool-bar-mode'")))
                   (insert argdoc)
                   (goto-char (point-min))
                   (let* ((first-end (save-excursion
                                       (fill-forward-paragraph 1)
                                       (point))))
                     (fill-region-as-paragraph (point) first-end 'left t)
                     (let ((from (point))
                           (to (save-excursion
                                 (fill-forward-paragraph 1)
                                 (copy-marker (point) t))))
                       (fill-delete-newlines from to 'left t nil)
                       (goto-char from)
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (unless (> (current-column) (current-fill-column))
                           (forward-char 1))
                         (fill-move-to-break-point linebeg)
                         (skip-chars-forward " \t")
                         (fill-newline))
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (list :point (point)
                               :column (current-column)
                               :to (marker-position to)
                               :linebeg linebeg
                               :text (buffer-string)))))))"#,
        ),
        (
            "docstring-second-paragraph-second-iteration-break",
            r#"(with-temp-buffer
                 (let* ((fill-prefix nil)
                        (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                        (fill-column (if (integerp docs-fc) docs-fc 65))
                        (argdoc (format
                                 easy-mmode--arg-docstring
                                 "global "
                                 "Tool-Bar mode"
                                 "the variable `tool-bar-mode'")))
                   (insert argdoc)
                   (goto-char (point-min))
                   (let* ((first-end (save-excursion
                                       (fill-forward-paragraph 1)
                                       (point))))
                     (fill-region-as-paragraph (point) first-end 'left t)
                     (let ((from (point))
                           (to (save-excursion
                                 (fill-forward-paragraph 1)
                                 (copy-marker (point) t))))
                       (fill-delete-newlines from to 'left t nil)
                       (goto-char from)
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (unless (> (current-column) (current-fill-column))
                           (forward-char 1))
                         (fill-move-to-break-point linebeg)
                         (skip-chars-forward " \t")
                         (fill-newline))
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (let ((after-move (point))
                               (after-move-col (current-column)))
                           (unless (> (current-column) (current-fill-column))
                             (forward-char 1))
                           (let ((after-forward (point))
                                 (after-forward-col (current-column)))
                             (fill-move-to-break-point linebeg)
                             (let ((after-break (point))
                                   (after-break-col (current-column)))
                               (skip-chars-forward " \t")
                               (list :linebeg linebeg
                                     :to (marker-position to)
                                     :after-move after-move
                                     :after-move-col after-move-col
                                     :after-forward after-forward
                                     :after-forward-col after-forward-col
                                     :after-break after-break
                                     :after-break-col after-break-col
                                     :after-skip (point)
                                     :after-skip-col (current-column)
                                     :before-end (< (point) to)
                                     :text (buffer-string))))))))))"#,
        ),
        (
            "docstring-second-paragraph-second-cut",
            r#"(with-temp-buffer
                 (let* ((fill-prefix nil)
                        (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                        (fill-column (if (integerp docs-fc) docs-fc 65))
                        (argdoc (format
                                 easy-mmode--arg-docstring
                                 "global "
                                 "Tool-Bar mode"
                                 "the variable `tool-bar-mode'")))
                   (insert argdoc)
                   (goto-char (point-min))
                   (let* ((first-end (save-excursion
                                       (fill-forward-paragraph 1)
                                       (point))))
                     (fill-region-as-paragraph (point) first-end 'left t)
                     (let ((from (point))
                           (to (save-excursion
                                 (fill-forward-paragraph 1)
                                 (copy-marker (point) t))))
                       (fill-delete-newlines from to 'left t nil)
                       (goto-char from)
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (unless (> (current-column) (current-fill-column))
                           (forward-char 1))
                         (fill-move-to-break-point linebeg)
                         (skip-chars-forward " \t")
                         (fill-newline))
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (unless (> (current-column) (current-fill-column))
                           (forward-char 1))
                         (fill-move-to-break-point linebeg)
                         (skip-chars-forward " \t")
                         (fill-newline)
                         (list :point (point)
                               :to (marker-position to)
                               :linebeg linebeg
                               :text (buffer-string)))))))"#,
        ),
        (
            "docstring-second-paragraph-post-second-cut",
            r#"(with-temp-buffer
                 (let* ((fill-prefix nil)
                        (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                        (fill-column (if (integerp docs-fc) docs-fc 65))
                        (argdoc (format
                                 easy-mmode--arg-docstring
                                 "global "
                                 "Tool-Bar mode"
                                 "the variable `tool-bar-mode'")))
                   (insert argdoc)
                   (goto-char (point-min))
                   (let* ((first-end (save-excursion
                                       (fill-forward-paragraph 1)
                                       (point))))
                     (fill-region-as-paragraph (point) first-end 'left t)
                     (let ((from (point))
                           (to (save-excursion
                                 (fill-forward-paragraph 1)
                                 (copy-marker (point) t))))
                       (fill-delete-newlines from to 'left t nil)
                       (goto-char from)
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (unless (> (current-column) (current-fill-column))
                           (forward-char 1))
                         (fill-move-to-break-point linebeg)
                         (skip-chars-forward " \t")
                         (fill-newline))
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (unless (> (current-column) (current-fill-column))
                           (forward-char 1))
                         (fill-move-to-break-point linebeg)
                         (skip-chars-forward " \t")
                         (fill-newline))
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (list :point (point)
                               :column (current-column)
                               :to (marker-position to)
                               :linebeg linebeg
                               :text (buffer-string)))))))"#,
        ),
        (
            "docstring-second-paragraph-first-justify",
            r#"(with-temp-buffer
                 (let* ((fill-prefix nil)
                        (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                        (fill-column (if (integerp docs-fc) docs-fc 65))
                        (argdoc (format
                                 easy-mmode--arg-docstring
                                 "global "
                                 "Tool-Bar mode"
                                 "the variable `tool-bar-mode'")))
                   (insert argdoc)
                   (goto-char (point-min))
                   (let* ((first-end (save-excursion
                                       (fill-forward-paragraph 1)
                                       (point))))
                     (fill-region-as-paragraph (point) first-end 'left t)
                     (let ((from (point))
                           (to (save-excursion
                                 (fill-forward-paragraph 1)
                                 (copy-marker (point) t))))
                       (fill-delete-newlines from to 'left t nil)
                       (goto-char from)
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (unless (> (current-column) (current-fill-column))
                           (forward-char 1))
                         (fill-move-to-break-point linebeg)
                         (skip-chars-forward " \t")
                         (fill-newline)
                         (save-excursion
                           (forward-line -1)
                           (justify-current-line 'left nil t))
                         (list :point (point)
                               :to (marker-position to)
                               :text (buffer-string)))))))"#,
        ),
        (
            "docstring-second-paragraph-second-justify",
            r#"(with-temp-buffer
                 (let* ((fill-prefix nil)
                        (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                        (fill-column (if (integerp docs-fc) docs-fc 65))
                        (argdoc (format
                                 easy-mmode--arg-docstring
                                 "global "
                                 "Tool-Bar mode"
                                 "the variable `tool-bar-mode'")))
                   (insert argdoc)
                   (goto-char (point-min))
                   (let* ((first-end (save-excursion
                                       (fill-forward-paragraph 1)
                                       (point))))
                     (fill-region-as-paragraph (point) first-end 'left t)
                     (let ((from (point))
                           (to (save-excursion
                                 (fill-forward-paragraph 1)
                                 (copy-marker (point) t))))
                       (fill-delete-newlines from to 'left t nil)
                       (goto-char from)
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (unless (> (current-column) (current-fill-column))
                           (forward-char 1))
                         (fill-move-to-break-point linebeg)
                         (skip-chars-forward " \t")
                         (fill-newline)
                         (save-excursion
                           (forward-line -1)
                           (justify-current-line 'left nil t)))
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (unless (> (current-column) (current-fill-column))
                           (forward-char 1))
                         (fill-move-to-break-point linebeg)
                         (skip-chars-forward " \t")
                         (fill-newline)
                         (save-excursion
                           (forward-line -1)
                           (justify-current-line 'left nil t))
                         (list :point (point)
                               :to (marker-position to)
                               :text (buffer-string)))))))"#,
        ),
        (
            "docstring-second-paragraph-final-justify",
            r#"(with-temp-buffer
                 (let* ((fill-prefix nil)
                        (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                        (fill-column (if (integerp docs-fc) docs-fc 65))
                        (argdoc (format
                                 easy-mmode--arg-docstring
                                 "global "
                                 "Tool-Bar mode"
                                 "the variable `tool-bar-mode'")))
                   (insert argdoc)
                   (goto-char (point-min))
                   (let* ((first-end (save-excursion
                                       (fill-forward-paragraph 1)
                                       (point))))
                     (fill-region-as-paragraph (point) first-end 'left t)
                     (let ((from (point))
                           (to (save-excursion
                                 (fill-forward-paragraph 1)
                                 (copy-marker (point) t))))
                       (fill-delete-newlines from to 'left t nil)
                       (goto-char from)
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (unless (> (current-column) (current-fill-column))
                           (forward-char 1))
                         (fill-move-to-break-point linebeg)
                         (skip-chars-forward " \t")
                         (fill-newline)
                         (save-excursion
                           (forward-line -1)
                           (justify-current-line 'left nil t)))
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (unless (> (current-column) (current-fill-column))
                           (forward-char 1))
                         (fill-move-to-break-point linebeg)
                         (skip-chars-forward " \t")
                         (fill-newline)
                         (save-excursion
                           (forward-line -1)
                           (justify-current-line 'left nil t)))
                       (goto-char to)
                       (justify-current-line 'left t t)
                       (list :point (point)
                             :to (marker-position to)
                             :text (buffer-string))))))"#,
        ),
        (
            "docstring-second-paragraph-finalize",
            r#"(with-temp-buffer
                 (let* ((fill-prefix nil)
                        (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                        (fill-column (if (integerp docs-fc) docs-fc 65))
                        (argdoc (format
                                 easy-mmode--arg-docstring
                                 "global "
                                 "Tool-Bar mode"
                                 "the variable `tool-bar-mode'")))
                   (insert argdoc)
                   (goto-char (point-min))
                   (let* ((first-end (save-excursion
                                       (fill-forward-paragraph 1)
                                       (point))))
                     (fill-region-as-paragraph (point) first-end 'left t)
                     (let ((from (point))
                           (to (save-excursion
                                 (fill-forward-paragraph 1)
                                 (copy-marker (point) t))))
                       (fill-delete-newlines from to 'left t nil)
                       (goto-char from)
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (unless (> (current-column) (current-fill-column))
                           (forward-char 1))
                         (fill-move-to-break-point linebeg)
                         (skip-chars-forward " \t")
                         (fill-newline)
                         (save-excursion
                           (forward-line -1)
                           (justify-current-line 'left nil t)))
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (unless (> (current-column) (current-fill-column))
                           (forward-char 1))
                         (fill-move-to-break-point linebeg)
                         (skip-chars-forward " \t")
                         (fill-newline)
                         (save-excursion
                           (forward-line -1)
                           (justify-current-line 'left nil t)))
                       (goto-char to)
                       (justify-current-line 'left t t)
                       (goto-char to)
                       (unless (eobp) (forward-char 1))
                       (set-marker to nil)
                       (list :point (point)
                             :text (buffer-string))))))"#,
        ),
        (
            "docstring-second-paragraph-third-iteration-setup",
            r#"(with-temp-buffer
                 (let* ((fill-prefix nil)
                        (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                        (fill-column (if (integerp docs-fc) docs-fc 65))
                        (argdoc (format
                                 easy-mmode--arg-docstring
                                 "global "
                                 "Tool-Bar mode"
                                 "the variable `tool-bar-mode'")))
                   (insert argdoc)
                   (goto-char (point-min))
                   (let* ((first-end (save-excursion
                                       (fill-forward-paragraph 1)
                                       (point))))
                     (fill-region-as-paragraph (point) first-end 'left t)
                     (let ((from (point))
                           (to (save-excursion
                                 (fill-forward-paragraph 1)
                                 (copy-marker (point) t))))
                       (fill-delete-newlines from to 'left t nil)
                       (goto-char from)
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (unless (> (current-column) (current-fill-column))
                           (forward-char 1))
                         (fill-move-to-break-point linebeg)
                         (skip-chars-forward " \t")
                         (fill-newline)
                         (save-excursion
                           (forward-line -1)
                           (justify-current-line 'left nil t)))
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (unless (> (current-column) (current-fill-column))
                           (forward-char 1))
                         (fill-move-to-break-point linebeg)
                         (skip-chars-forward " \t")
                         (fill-newline)
                         (save-excursion
                           (forward-line -1)
                           (justify-current-line 'left nil t)))
                       (let ((linebeg (point))
                             (before (point))
                             (before-col (current-column)))
                         (move-to-column (current-fill-column))
                         (list :linebeg linebeg
                               :to (marker-position to)
                               :before before
                               :before-col before-col
                               :after-move (point)
                               :after-move-col (current-column)
                               :text (buffer-string)))))))"#,
        ),
        (
            "docstring-second-paragraph-third-iteration-break",
            r#"(with-temp-buffer
                 (let* ((fill-prefix nil)
                        (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                        (fill-column (if (integerp docs-fc) docs-fc 65))
                        (argdoc (format
                                 easy-mmode--arg-docstring
                                 "global "
                                 "Tool-Bar mode"
                                 "the variable `tool-bar-mode'")))
                   (insert argdoc)
                   (goto-char (point-min))
                   (let* ((first-end (save-excursion
                                       (fill-forward-paragraph 1)
                                       (point))))
                     (fill-region-as-paragraph (point) first-end 'left t)
                     (let ((from (point))
                           (to (save-excursion
                                 (fill-forward-paragraph 1)
                                 (copy-marker (point) t))))
                       (fill-delete-newlines from to 'left t nil)
                       (goto-char from)
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (unless (> (current-column) (current-fill-column))
                           (forward-char 1))
                         (fill-move-to-break-point linebeg)
                         (skip-chars-forward " \t")
                         (fill-newline)
                         (save-excursion
                           (forward-line -1)
                           (justify-current-line 'left nil t)))
                       (let ((linebeg (point)))
                         (move-to-column (current-fill-column))
                         (unless (> (current-column) (current-fill-column))
                           (forward-char 1))
                         (fill-move-to-break-point linebeg)
                         (skip-chars-forward " \t")
                         (fill-newline)
                         (save-excursion
                           (forward-line -1)
                           (justify-current-line 'left nil t)))
                       (let ((linebeg (point))
                             (before (point))
                             (before-col (current-column)))
                         (move-to-column (current-fill-column))
                         (let ((after-move (point))
                               (after-move-col (current-column)))
                           (unless (> (current-column) (current-fill-column))
                             (forward-char 1))
                           (let ((after-forward (point))
                                 (after-forward-col (current-column)))
                             (fill-move-to-break-point linebeg)
                             (let ((after-break (point))
                                   (after-break-col (current-column)))
                               (skip-chars-forward " \t")
                               (list :linebeg linebeg
                                     :to (marker-position to)
                                     :before before
                                     :before-col before-col
                                     :after-move after-move
                                     :after-move-col after-move-col
                                     :after-forward after-forward
                                     :after-forward-col after-forward-col
                                     :after-break after-break
                                     :after-break-col after-break-col
                                     :after-skip (point)
                                     :after-skip-col (current-column)
                                     :before-end (< (point) to)
                                     :text (buffer-string))))))))))"#,
        ),
        (
            "docstring-second-paragraph-fill-return",
            r#"(with-temp-buffer
                 (let* ((fill-prefix nil)
                        (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                        (fill-column (if (integerp docs-fc) docs-fc 65))
                        (argdoc (format
                                 easy-mmode--arg-docstring
                                 "global "
                                 "Tool-Bar mode"
                                 "the variable `tool-bar-mode'")))
                   (insert argdoc)
                   (goto-char (point-min))
                   (let* ((first-end (save-excursion
                                       (fill-forward-paragraph 1)
                                       (point))))
                     (fill-region-as-paragraph (point) first-end 'left t)
                     (let ((second-end (save-excursion
                                         (fill-forward-paragraph 1)
                                         (point))))
                       (fill-region-as-paragraph (point) second-end 'left t)
                       'ok))))"#,
        ),
        (
            "docstring-second-paragraph-fill",
            r#"(with-temp-buffer
                 (let* ((fill-prefix nil)
                        (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                        (fill-column (if (integerp docs-fc) docs-fc 65))
                        (argdoc (format
                                 easy-mmode--arg-docstring
                                 "global "
                                 "Tool-Bar mode"
                                 "the variable `tool-bar-mode'")))
                   (insert argdoc)
                   (goto-char (point-min))
                   (let* ((max (copy-marker (point-max) t))
                          (first-end (save-excursion
                                       (fill-forward-paragraph 1)
                                       (point))))
                     (fill-region-as-paragraph (point) first-end 'left t)
                     (let ((second-end (save-excursion
                                         (fill-forward-paragraph 1)
                                         (point))))
                       (fill-region-as-paragraph (point) second-end 'left t)
                       (list :point (point)
                             :max (marker-position max)
                             :second-end second-end
                             :text (buffer-string))))))"#,
        ),
        (
            "docstring-boilerplate-fill",
            r#"(with-temp-buffer
                 (let* ((fill-prefix nil)
                        (docs-fc (bound-and-true-p emacs-lisp-docstring-fill-column))
                        (fill-column (if (integerp docs-fc) docs-fc 65))
                        (argdoc (format
                                 easy-mmode--arg-docstring
                                 "global "
                                 "Tool-Bar mode"
                                 "the variable `tool-bar-mode'")))
                   (let ((start (point)))
                     (insert argdoc)
                     (fill-region start (point) 'left t))
                   (buffer-string)))"#,
        ),
        (
            "docstring",
            r#"(easy-mmode--mode-docstring
                 "Toggle the tool bar in all graphical frames (Tool Bar mode).

See `tool-bar-add-item' and `tool-bar-add-item-from-menu' for
conveniently adding tool bar items."
                 "Tool-Bar mode"
                 'tool-bar-map
                 'tool-bar-mode
                 t)"#,
        ),
        (
            "pcase-modevar",
            r#"(let ((getter 'tool-bar-mode))
                 (pcase getter
                   (`(default-value ',v) v)
                   (_ getter)))"#,
        ),
    ] {
        let forms = crate::emacs_core::parser::parse_forms(src).expect("parse easy-mmode probe");
        tracing::info!("tool-bar probe: helper {}", label);
        let value = eval.eval_expr(&forms[0]).unwrap_or_else(|err| {
            panic!(
                "failed evaluating tool-bar helper {label} from {}: {}",
                tool_bar_path.display(),
                format_eval_error(&eval, &err)
            )
        });
        let rendered = crate::emacs_core::print::print_value_with_buffers(&value, &eval.buffers);
        tracing::info!("tool-bar probe: helper {} => {}", label, rendered);
    }
    let macroexpand_probe = crate::emacs_core::parser::parse_forms(
        r#"(macroexpand
             '(define-minor-mode tool-bar-mode
                "Toggle the tool bar in all graphical frames (Tool Bar mode).

See `tool-bar-add-item' and `tool-bar-add-item-from-menu' for
conveniently adding tool bar items."
                :init-value t
                :global t
                :variable tool-bar-mode
                (let ((val (if tool-bar-mode 1 0)))
                  (dolist (frame (frame-list))
                    (set-frame-parameter frame 'tool-bar-lines val))
                  (if (assq 'tool-bar-lines default-frame-alist)
                      (setq default-frame-alist
                            (cons (cons 'tool-bar-lines val)
                                  (assq-delete-all 'tool-bar-lines
                                                   default-frame-alist)))))
                (and tool-bar-mode
                     (= 1 (length (default-value 'tool-bar-map)))
                     (tool-bar-setup))))"#,
    )
    .expect("parse macroexpand probe");
    tracing::info!("tool-bar probe: macroexpand form 1");
    let expanded = eval
        .eval_expr(&macroexpand_probe[0])
        .expect("macroexpand tool-bar define-minor-mode");
    tracing::info!("tool-bar probe: macroexpand complete");
    if let Some(forms) = list_to_vec(&expanded) {
        if matches!(forms.first(), Some(Value::Symbol(id)) if resolve_sym(*id) == "progn") {
            for (idx, form) in forms.iter().enumerate().skip(1) {
                tracing::info!("tool-bar probe: eval expanded subform {}", idx);
                let expr = value_to_expr(form);
                eval.eval_expr(&expr).unwrap_or_else(|err| {
                    panic!(
                        "failed evaluating tool-bar expanded subform {} from {}: {}",
                        idx,
                        tool_bar_path.display(),
                        format_eval_error(&eval, &err)
                    )
                });
            }
        } else {
            panic!("unexpected macroexpand output for tool-bar define-minor-mode: {expanded:?}");
        }
    } else {
        panic!("macroexpand did not return a list for tool-bar define-minor-mode: {expanded:?}");
    }
    for (idx, form) in top_level_forms.iter().enumerate().skip(1) {
        tracing::info!("tool-bar probe: eval top-level form {}", idx + 1);
        eval.eval_expr(form).unwrap_or_else(|err| {
            panic!(
                "failed evaluating tool-bar form {} from {}: {}",
                idx + 1,
                tool_bar_path.display(),
                format_eval_error(&eval, &err)
            )
        });
    }
    tracing::info!("tool-bar probe: load complete");
    let forms = crate::emacs_core::parser::parse_forms(
        r#"(list
             (special-form-p 'define-minor-mode)
             (commandp 'tool-bar-mode)
             (not (and (consp (symbol-function 'tool-bar-mode))
                       (eq (car (symbol-function 'tool-bar-mode)) 'autoload)))
             (keymapp tool-bar-map))"#,
    )
    .expect("parse tool-bar bootstrap probe");
    let result = eval
        .eval_expr(&forms[0])
        .expect("evaluate tool-bar bootstrap probe");
    assert_eq!(
        result,
        Value::list(vec![Value::Nil, Value::True, Value::True, Value::True])
    );
}

#[test]
fn evaluator_bootstrap_binds_default_frame_scroll_bars_like_gnu_frame_c() {
    let eval = Evaluator::new();
    assert_eq!(
        eval.obarray.symbol_value("default-frame-scroll-bars"),
        Some(&Value::symbol("right"))
    );
}

#[test]
fn auth_source_backend_exposes_type_slot() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();

    let mut eval =
        create_bootstrap_evaluator_cached_with_features(&["neomacs"]).expect("bootstrap evaluator");
    let runtime_load_path = crate::emacs_core::parser::parse_forms("(load \"subdirs\" nil t)")
        .expect("parse runtime load-path expansion");
    eval.eval_expr(&runtime_load_path[0])
        .expect("load runtime subdirs.el");
    let require_error = eval
        .require_value(Value::symbol("auth-source"), None, None)
        .err()
        .map(|err| match err {
            crate::emacs_core::error::Flow::Signal(sig) => {
                let rendered = sig
                    .data
                    .iter()
                    .map(|value| format!("{value}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("({} {})", sig.symbol_name(), rendered)
            }
            other => format!("{other:?}"),
        });

    let form = crate::emacs_core::parser::parse_forms(
        "(let ((backend (make-instance 'auth-source-backend :type 'netrc :source \"test\")))\n\
           (list (slot-value backend 'type)\n\
                 (slot-value backend 'source)\n\
                 (mapcar #'cl--slot-descriptor-name\n\
                         (eieio-class-slots (eieio-object-class backend)))))",
    )
    .expect("parse auth-source backend slot probe");
    let result = eval.eval_expr(&form[0]).unwrap_or_else(|err| {
        panic!(
            "evaluate auth-source backend slot probe failed after require_error={require_error:?}: {err:?}"
        )
    });
    let items = crate::emacs_core::value::list_to_vec(&result).expect("probe result list");
    assert_eq!(items.first().copied(), Some(Value::symbol("netrc")));
    assert_eq!(items.get(1).and_then(Value::as_str), Some("test"));

    let slot_names = crate::emacs_core::value::list_to_vec(&items[2]).expect("slot names list");
    assert!(
        slot_names
            .iter()
            .any(|value| value.as_symbol_name() == Some("type")),
        "expected auth-source-backend slots to include `type`, got {:?}, require_error={require_error:?}",
        slot_names,
    );
}

fn expect_vector_ints(value: Value) -> Vec<i64> {
    match value {
        Value::Vector(v) => with_heap(|h| h.get_vector(v).clone())
            .iter()
            .map(|item| match item {
                Value::Int(n) => *n,
                other => panic!("expected int in vector, got {other:?}"),
            })
            .collect(),
        other => panic!("expected vector, got {other:?}"),
    }
}

#[test]
fn cl_callf_updates_variable_place() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    let form = crate::emacs_core::parser::parse_forms(
        "(let ((a '(3 2 1)))
           (cl-callf (lambda (slots) (apply #'vector (nreverse slots))) a)
           a)",
    )
    .expect("parse cl-callf variable probe");
    let result = eval
        .eval_expr(&form[0])
        .expect("evaluate cl-callf variable probe");
    assert_eq!(expect_vector_ints(result), vec![1, 2, 3]);
}

#[test]
fn direct_setq_funcall_updates_variable_place() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    let form = crate::emacs_core::parser::parse_forms(
        "(let ((a '(3 2 1)))
           (setq a (funcall #'(lambda (slots) (apply #'vector (nreverse slots))) a))
           a)",
    )
    .expect("parse direct funcall probe");
    let result = eval
        .eval_expr(&form[0])
        .expect("evaluate direct funcall probe");
    assert_eq!(expect_vector_ints(result), vec![1, 2, 3]);
}

#[test]
fn pdump_roundtrip_preserves_advice_remove_member_lifecycle() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");

    let mut eval = create_bootstrap_evaluator().expect("bootstrap evaluator");
    ensure_startup_compat_variables(&mut eval, project_root);

    let dir = tempfile::tempdir().expect("tempdir");
    let dump_path = dir.path().join("advice-lifecycle.pdump");
    crate::emacs_core::pdump::dump_to_file(&eval, &dump_path).expect("dump should succeed");
    drop(eval);

    let mut loaded =
        crate::emacs_core::pdump::load_from_dump(&dump_path).expect("load should succeed");
    ensure_startup_compat_variables(&mut loaded, project_root);
    apply_runtime_startup_state(&mut loaded).expect("runtime startup after load");

    let steps = [
        (
            "setup-target",
            "(fset 'neovm--adv-tgt3 (lambda (x) x))",
            None,
        ),
        (
            "setup-before",
            "(fset 'neovm--adv-fn3a (lambda (&rest _) nil))",
            None,
        ),
        (
            "setup-after",
            "(fset 'neovm--adv-fn3b (lambda (&rest _) nil))",
            None,
        ),
        (
            "member-initial",
            "(not (null (advice-member-p 'neovm--adv-fn3a 'neovm--adv-tgt3)))",
            Some("nil"),
        ),
        (
            "add-before",
            "(advice-add 'neovm--adv-tgt3 :before 'neovm--adv-fn3a)",
            None,
        ),
        (
            "add-after",
            "(advice-add 'neovm--adv-tgt3 :after 'neovm--adv-fn3b)",
            None,
        ),
        (
            "member-before-present",
            "(not (null (advice-member-p 'neovm--adv-fn3a 'neovm--adv-tgt3)))",
            Some("t"),
        ),
        (
            "member-after-present",
            "(not (null (advice-member-p 'neovm--adv-fn3b 'neovm--adv-tgt3)))",
            Some("t"),
        ),
        (
            "remove-before",
            "(advice-remove 'neovm--adv-tgt3 'neovm--adv-fn3a)",
            None,
        ),
        (
            "member-before-absent",
            "(not (null (advice-member-p 'neovm--adv-fn3a 'neovm--adv-tgt3)))",
            Some("nil"),
        ),
        (
            "member-after-still-present",
            "(not (null (advice-member-p 'neovm--adv-fn3b 'neovm--adv-tgt3)))",
            Some("t"),
        ),
        (
            "remove-after",
            "(advice-remove 'neovm--adv-tgt3 'neovm--adv-fn3b)",
            None,
        ),
        (
            "member-after-absent",
            "(not (null (advice-member-p 'neovm--adv-fn3b 'neovm--adv-tgt3)))",
            Some("nil"),
        ),
    ];

    for (label, form, expected) in steps {
        let parsed = crate::emacs_core::parser::parse_forms(form).expect("parse step");
        let value = loaded.eval_expr(&parsed[0]).expect("evaluate step");
        if let Some(expected) = expected {
            let rendered =
                crate::emacs_core::print::print_value_with_buffers(&value, &loaded.buffers);
            assert_eq!(rendered, expected, "unexpected result at step {label}");
        }
    }
}

#[test]
fn pdump_roundtrip_evaluates_full_advice_remove_member_form() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");

    let mut eval = create_bootstrap_evaluator().expect("bootstrap evaluator");
    ensure_startup_compat_variables(&mut eval, project_root);

    let dir = tempfile::tempdir().expect("tempdir");
    let dump_path = dir.path().join("advice-lifecycle-full.pdump");
    crate::emacs_core::pdump::dump_to_file(&eval, &dump_path).expect("dump should succeed");
    drop(eval);

    let mut loaded =
        crate::emacs_core::pdump::load_from_dump(&dump_path).expect("load should succeed");
    ensure_startup_compat_variables(&mut loaded, project_root);
    apply_runtime_startup_state(&mut loaded).expect("runtime startup after load");

    let form = crate::emacs_core::parser::parse_forms(
        r#"(progn
      (fset 'neovm--adv-tgt3 (lambda (x) x))
      (fset 'neovm--adv-fn3a (lambda (&rest _) nil))
      (fset 'neovm--adv-fn3b (lambda (&rest _) nil))
      (unwind-protect
          (let (results)
            (setq results (cons (not (null (advice-member-p 'neovm--adv-fn3a 'neovm--adv-tgt3))) results))
            (advice-add 'neovm--adv-tgt3 :before 'neovm--adv-fn3a)
            (advice-add 'neovm--adv-tgt3 :after 'neovm--adv-fn3b)
            (setq results (cons (not (null (advice-member-p 'neovm--adv-fn3a 'neovm--adv-tgt3))) results))
            (setq results (cons (not (null (advice-member-p 'neovm--adv-fn3b 'neovm--adv-tgt3))) results))
            (advice-remove 'neovm--adv-tgt3 'neovm--adv-fn3a)
            (setq results (cons (not (null (advice-member-p 'neovm--adv-fn3a 'neovm--adv-tgt3))) results))
            (setq results (cons (not (null (advice-member-p 'neovm--adv-fn3b 'neovm--adv-tgt3))) results))
            (advice-remove 'neovm--adv-tgt3 'neovm--adv-fn3b)
            (setq results (cons (not (null (advice-member-p 'neovm--adv-fn3b 'neovm--adv-tgt3))) results))
            (nreverse results))
        (fmakunbound 'neovm--adv-tgt3)
        (fmakunbound 'neovm--adv-fn3a)
        (fmakunbound 'neovm--adv-fn3b)))"#,
    )
    .expect("parse form");

    let value = loaded.eval_expr(&form[0]).expect("evaluate full form");
    let rendered = crate::emacs_core::print::print_value_with_buffers(&value, &loaded.buffers);
    assert_eq!(rendered, "(nil t t nil t nil)");
}

#[test]
fn cached_bootstrap_reload_evaluates_full_advice_remove_member_form() {
    let form_source = r#"(progn
      (fset 'neovm--adv-tgt3 (lambda (x) x))
      (fset 'neovm--adv-fn3a (lambda (&rest _) nil))
      (fset 'neovm--adv-fn3b (lambda (&rest _) nil))
      (unwind-protect
          (let (results)
            (setq results (cons (not (null (advice-member-p 'neovm--adv-fn3a 'neovm--adv-tgt3))) results))
            (advice-add 'neovm--adv-tgt3 :before 'neovm--adv-fn3a)
            (advice-add 'neovm--adv-tgt3 :after 'neovm--adv-fn3b)
            (setq results (cons (not (null (advice-member-p 'neovm--adv-fn3a 'neovm--adv-tgt3))) results))
            (setq results (cons (not (null (advice-member-p 'neovm--adv-fn3b 'neovm--adv-tgt3))) results))
            (advice-remove 'neovm--adv-tgt3 'neovm--adv-fn3a)
            (setq results (cons (not (null (advice-member-p 'neovm--adv-fn3a 'neovm--adv-tgt3))) results))
            (setq results (cons (not (null (advice-member-p 'neovm--adv-fn3b 'neovm--adv-tgt3))) results))
            (advice-remove 'neovm--adv-tgt3 'neovm--adv-fn3b)
            (setq results (cons (not (null (advice-member-p 'neovm--adv-fn3b 'neovm--adv-tgt3))) results))
            (nreverse results))
        (fmakunbound 'neovm--adv-tgt3)
        (fmakunbound 'neovm--adv-fn3a)
        (fmakunbound 'neovm--adv-fn3b)))"#;

    let dir = tempfile::tempdir().expect("tempdir");
    let dump_path = dir.path().join("cached-advice-lifecycle.pdump");

    let mut fresh =
        create_bootstrap_evaluator_cached_at_path(&[], &dump_path).expect("fresh cached bootstrap");
    apply_runtime_startup_state(&mut fresh).expect("fresh runtime startup");
    let fresh_form = crate::emacs_core::parser::parse_forms(form_source).expect("parse fresh form");
    let fresh_value = fresh.eval_expr(&fresh_form[0]).expect("fresh form eval");
    assert_eq!(
        crate::emacs_core::print::print_value_with_buffers(&fresh_value, &fresh.buffers),
        "(nil t t nil t nil)"
    );
    drop(fresh);

    let mut loaded = create_bootstrap_evaluator_cached_at_path(&[], &dump_path)
        .expect("loaded cached bootstrap");
    apply_runtime_startup_state(&mut loaded).expect("loaded runtime startup");
    let loaded_form =
        crate::emacs_core::parser::parse_forms(form_source).expect("parse loaded form");
    let loaded_value = loaded.eval_expr(&loaded_form[0]).expect("loaded form eval");
    assert_eq!(
        crate::emacs_core::print::print_value_with_buffers(&loaded_value, &loaded.buffers),
        "(nil t t nil t nil)"
    );
}

#[test]
fn bootstrap_source_eval_honors_advised_subr_function_cell() {
    let rendered = crate::emacs_core::oracle_test::common::run_neovm_eval_with_bootstrap(
        r#"(progn
           (let ((log nil))
             (fset 'neovm--combo-plus-before
                   (lambda (&rest args)
                     (setq log (cons args log))))
             (unwind-protect
                 (list
                   (progn
                     (advice-add '+ :before 'neovm--combo-plus-before)
                     (setq log nil)
                     (list
                       (+ 4 7)
                       (funcall '+ 4 7)
                       (apply '+ (list 4 7))
                       (nreverse log)
                       (if (advice-member-p 'neovm--combo-plus-before '+) t nil)))
                   (progn
                     (advice-remove '+ 'neovm--combo-plus-before)
                     (setq log nil)
                     (list
                       (+ 4 7)
                       (funcall '+ 4 7)
                       (apply '+ (list 4 7))
                       (nreverse log)
                       (if (advice-member-p 'neovm--combo-plus-before '+) t nil))))
               (condition-case nil
                   (advice-remove '+ 'neovm--combo-plus-before)
                 (error nil))
               (fmakunbound 'neovm--combo-plus-before))))"#,
    )
    .expect("evaluate plus advice shape");
    assert_eq!(
        rendered,
        "OK ((11 11 11 ((4 7) (4 7) (4 7)) t) (11 11 11 nil nil))"
    );
}

#[test]
fn bootstrap_source_eval_honors_advised_callbuiltin_function_cell() {
    let rendered = crate::emacs_core::oracle_test::common::run_neovm_eval_with_bootstrap(
        r#"(progn
           (let ((log nil))
             (fset 'neovm--substring-before
                   (lambda (&rest args)
                     (setq log (cons args log))))
             (unwind-protect
                 (list
                   (progn
                     (advice-add 'substring :before 'neovm--substring-before)
                     (setq log nil)
                     (list
                       (substring "abcdef" 1 4)
                       (funcall 'substring "abcdef" 1 4)
                       (apply 'substring (list "abcdef" 1 4))
                       (nreverse log)
                       (if (advice-member-p 'neovm--substring-before 'substring) t nil)))
                   (progn
                     (advice-remove 'substring 'neovm--substring-before)
                     (setq log nil)
                     (list
                       (substring "abcdef" 1 4)
                       (funcall 'substring "abcdef" 1 4)
                       (apply 'substring (list "abcdef" 1 4))
                       (nreverse log)
                       (if (advice-member-p 'neovm--substring-before 'substring) t nil))))
               (condition-case nil
                   (advice-remove 'substring 'neovm--substring-before)
                 (error nil))
               (fmakunbound 'neovm--substring-before))))"#,
    )
    .expect("evaluate substring advice shape");
    assert_eq!(
        rendered,
        "OK ((\"bcd\" \"bcd\" \"bcd\" ((\"abcdef\" 1 4) (\"abcdef\" 1 4) (\"abcdef\" 1 4)) t) (\"bcd\" \"bcd\" \"bcd\" nil nil))"
    );
}

#[test]
fn runtime_startup_state_matches_char_syntax_comprehensive_form() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let form = crate::emacs_core::parser::parse_forms(
        r#"
(list
 ;; Standard syntax table entries
 (char-syntax ?a)
 (char-syntax ?Z)
 (char-syntax ?0)
 (char-syntax ?9)
 (char-syntax ?_)
 (char-syntax ?\ )
 (char-syntax ?\t)
 (char-syntax ?\n)
 (char-syntax ?\()
 (char-syntax ?\))
 (char-syntax ?\[)
 (char-syntax ?\])
 (char-syntax ?{)
 (char-syntax ?})
 (char-syntax ?.)
 (char-syntax ?,)
 (char-syntax ?;)
 (char-syntax ?\")
 (char-syntax ?+)
 (char-syntax ?-)
 (char-syntax ?*)
 (char-syntax ?/)
 (char-syntax ?')
   (with-syntax-table (copy-syntax-table)
     (modify-syntax-entry ?_ "w")
     (modify-syntax-entry ?- "w")
     (list (char-syntax ?_)
           (char-syntax ?-)
           (char-syntax ?a)
           (char-syntax ?\())))
"#,
    )
    .expect("parse char syntax comprehensive probe");
    let result = eval
        .eval_expr(&form[0])
        .expect("evaluate char syntax comprehensive probe");
    assert_eq!(
        crate::emacs_core::print::print_value_with_buffers(&result, &eval.buffers),
        "(119 119 119 119 95 32 32 62 40 41 40 41 95 95 95 39 60 34 95 95 95 95 39 (119 119 119 40))"
    );
}

#[test]
fn oracle_bootstrap_helper_matches_char_syntax_comprehensive_form() {
    let rendered = crate::emacs_core::oracle_test::common::run_neovm_eval_with_bootstrap(
        r#"
(list
 ;; Standard syntax table entries
 (char-syntax ?a)       ;; ?w (word)
 (char-syntax ?Z)       ;; ?w
 (char-syntax ?0)       ;; ?w
 (char-syntax ?9)       ;; ?w
 (char-syntax ?_)       ;; ?_ (symbol) in standard table
 (char-syntax ?\ )      ;; ?\  (whitespace)
 (char-syntax ?\t)      ;; ?\  (whitespace)
 (char-syntax ?\n)      ;; ?\  (whitespace or comment-end)
 (char-syntax ?\()      ;; ?\( (open paren)
 (char-syntax ?\))      ;; ?\) (close paren)
 (char-syntax ?\[)      ;; ?\( (open paren in standard)
 (char-syntax ?\])      ;; ?\) (close paren in standard)
 (char-syntax ?{)
 (char-syntax ?})
 (char-syntax ?.)       ;; ?. (punctuation)
 (char-syntax ?,)       ;; ?. (punctuation)
 (char-syntax ?;)
 (char-syntax ?\")      ;; ?\" (string delimiter)
 (char-syntax ?+)       ;; ?. (punctuation)
 (char-syntax ?-)       ;; ?. (punctuation)
 (char-syntax ?*)
 (char-syntax ?/)
 (char-syntax ?')       ;; ?' (expression prefix) or ?w
 ;; With a custom syntax table
 (with-syntax-table (copy-syntax-table)
   ;; Make _ a word constituent
   (modify-syntax-entry ?_ "w")
   ;; Make - a word constituent
   (modify-syntax-entry ?- "w")
   (list (char-syntax ?_)
         (char-syntax ?-)
         ;; Other entries unchanged
         (char-syntax ?a)
         (char-syntax ?\())))
"#,
    )
    .expect("oracle bootstrap helper should evaluate form");
    assert_eq!(
        rendered,
        "OK (119 119 119 119 95 32 32 62 40 41 40 41 95 95 95 39 60 34 95 95 95 95 39 (119 119 119 40))"
    );
}

#[test]
fn bootstrap_cl_subseq_setf_updates_vector() {
    let rendered = crate::emacs_core::oracle_test::common::run_neovm_eval_with_bootstrap(
        r#"
(progn
  (require 'cl-lib)
  (let ((v (vector 1 2 3 4 5)))
    (setf (cl-subseq v 1 3) '(20 30))
    (append v nil)))
"#,
    )
    .expect("bootstrapped cl-subseq setf evaluation");
    assert_eq!(rendered, "OK (1 20 30 4 5)");
}

#[test]
fn bootstrap_function_put_gv_expander_round_trip() {
    let rendered = crate::emacs_core::oracle_test::common::run_neovm_eval_with_bootstrap(
        r#"
(progn
  (require 'gv)
  (function-put
   'vm-direct-gv
   'gv-expander
   (lambda (do &rest args)
     (gv--defsetter
      'vm-direct-gv
      (lambda (new seq start &optional end)
        (macroexp-let2 nil new new
          `(progn
             (list ,new ,seq ,start ,end)
             ,new)))
      do args)))
  (funcall
   (function-get 'vm-direct-gv 'gv-expander)
   (lambda (_getter setter) (funcall setter '(20 30)))
   'v 1 3))
"#,
    )
    .expect("bootstrapped direct gv expander evaluation");
    assert_eq!(
        rendered,
        "OK (let* ((v v) (new (20 30))) (progn (list new v 1 3) new))"
    );
}

#[test]
fn bootstrap_gv_define_setter_round_trip() {
    let rendered = crate::emacs_core::oracle_test::common::run_neovm_eval_with_bootstrap(
        r#"
(progn
  (require 'gv)
  (gv-define-setter vm-gv-defined (new seq start &optional end)
    (macroexp-let2 nil new new
      `(progn
         (list ,new ,seq ,start ,end)
         ,new)))
  (funcall
   (function-get 'vm-gv-defined 'gv-expander)
   (lambda (_getter setter) (funcall setter '(20 30)))
   'v 1 3))
"#,
    )
    .expect("bootstrapped gv-define-setter evaluation");
    assert_eq!(
        rendered,
        "OK (let* ((v v) (new (20 30))) (progn (list new v 1 3) new))"
    );
}

#[test]
fn bootstrap_defun_gv_setter_declaration_round_trip() {
    let rendered = crate::emacs_core::oracle_test::common::run_neovm_eval_with_bootstrap(
        r#"
(progn
  (defun vm-decl-gv (seq start &optional end)
    (declare
     (gv-setter
      (lambda (new)
        (macroexp-let2 nil new new
          `(progn
             (list ,seq ,new ,start ,end)
             ,new)))))
    (list seq start end))
  (funcall
   (function-get 'vm-decl-gv 'gv-expander)
   (lambda (_getter setter) (funcall setter '(20 30)))
   'v 1 3))
"#,
    )
    .expect("bootstrapped defun gv-setter declaration evaluation");
    assert_eq!(
        rendered,
        "OK (let* ((v v) (new (20 30))) (progn (list v new 1 3) new))"
    );
}

#[test]
fn bootstrap_defun_gv_setter_declaration_evaluates_generated_form() {
    let rendered = crate::emacs_core::oracle_test::common::run_neovm_eval_with_bootstrap(
        r#"
(progn
  (defun vm-decl-gv-subseq (seq start &optional end)
    (declare
     (gv-setter
      (lambda (new)
        (macroexp-let2 nil new new
          `(progn
             (cl-replace ,seq ,new :start1 ,start :end1 ,end)
             ,new)))))
    (seq-subseq seq start end))
  (let ((v (vector 1 2 3 4 5)))
    (eval
     (funcall
      (function-get 'vm-decl-gv-subseq 'gv-expander)
      (lambda (_getter setter) (funcall setter ''(20 30)))
      'v 1 3)
     t)
    (append v nil)))
"#,
    )
    .expect("bootstrapped defun gv-setter declaration setter-eval");
    assert_eq!(rendered, "OK (1 20 30 4 5)");
}

#[test]
fn bootstrap_eieio_core_preserves_accessor_compiler_macro() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"
(progn
  (require 'eieio-core)
  (let* ((cm (function-get 'eieio--class-index-table 'compiler-macro))
         (class (eieio--class-make 'foo))
         (idx (make-hash-table :test 'eq)))
    (puthash 'x 1 idx)
    (setf (eieio--class-index-table class) idx)
    (list (symbolp cm)
          (eq cm 'eieio--class-index-table--inliner)
          (gethash 'x (cl--class-index-table class)))))
"#,
    );

    assert_eq!(rendered, "OK (t t 1)");
}

#[test]
fn bootstrap_defun_compiler_macro_declaration_sets_properties() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"
(progn
  (defun vm--cmacro-probe (x)
    (declare (compiler-macro vm--cmacro-probe--cm))
    x)
  (defun vm--cmacro-probe--cm (_form x) x)
  (list (get 'vm--cmacro-probe 'compiler-macro)
        (function-get 'vm--cmacro-probe 'compiler-macro)))
"#,
    );

    assert_eq!(rendered, "OK (vm--cmacro-probe--cm vm--cmacro-probe--cm)");
}

#[test]
fn bootstrap_define_inline_sets_compiler_macro_properties() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"
(progn
  (require 'inline)
  (define-inline vm--inline-probe (x) x)
  (list (get 'vm--inline-probe 'compiler-macro)
        (function-get 'vm--inline-probe 'compiler-macro)))
"#,
    );

    assert_eq!(
        rendered,
        "OK (vm--inline-probe--inliner vm--inline-probe--inliner)"
    );
}

#[test]
fn expanded_cache_replay_preserves_define_inline_compiler_macro() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("vm-inline-cache.el");
    std::fs::write(
        &path,
        r#"
(require 'inline)
(define-inline vm--inline-cache-probe (x) x)
"#,
    )
    .expect("write inline fixture");

    let form = r#"
(list (get 'vm--inline-cache-probe 'compiler-macro)
      (function-get 'vm--inline-cache-probe 'compiler-macro))
"#;

    let first = cached_bootstrap_eval_with_loaded_file(&path, form);
    let second = cached_bootstrap_eval_with_loaded_file(&path, form);

    assert_eq!(
        first,
        "OK (vm--inline-cache-probe--inliner vm--inline-cache-probe--inliner)"
    );
    assert_eq!(
        second,
        "OK (vm--inline-cache-probe--inliner vm--inline-cache-probe--inliner)"
    );
}

#[test]
fn expanded_cache_replay_preserves_oclosure_define_class_registration() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("vm-oclosure-cache.el");
    std::fs::write(
        &path,
        r#"
(oclosure-define advice)
(cl-defmethod oclosure-interactive-form ((ad advice) &optional _)
  ad)
"#,
    )
    .expect("write oclosure fixture");

    let form = r#"
(let ((class (cl--find-class 'advice)))
  (list (and class t)
        (ignore-errors (and (cl-generic-generalizers 'advice) t))))
"#;

    let load_with_partial_bootstrap = || {
        let mut eval = partial_bootstrap_eval_until("emacs-lisp/nadvice", false);
        load_file(&mut eval, &path).unwrap_or_else(|err| {
            panic!(
                "failed loading {}: {}",
                path.display(),
                format_eval_error(&eval, &err)
            )
        });
        eval_rendered(&mut eval, form)
    };

    let first = load_with_partial_bootstrap();
    let second = load_with_partial_bootstrap();

    assert_eq!(first, "OK (t t)");
    assert_eq!(second, "OK (t t)");
}

#[test]
fn expanded_cache_replay_preserves_nadvice_eval_and_compile_helpers() {
    let load_with_partial_bootstrap = || {
        std::thread::Builder::new()
            .name("nadvice-cache-replay".into())
            .stack_size(64 * 1024 * 1024)
            .spawn(|| {
                let mut eval = partial_bootstrap_eval_until("mouse", false);
                eval_rendered(
                    &mut eval,
                    r#"
(list (fboundp 'advice--normalize-place)
      (fboundp 'add-function))
"#,
                )
            })
            .expect("spawn nadvice bootstrap thread")
            .join()
            .expect("nadvice bootstrap thread should succeed")
    };

    let first = load_with_partial_bootstrap();
    let second = load_with_partial_bootstrap();

    assert_eq!(first, "OK (t t)");
    assert_eq!(second, "OK (t t)");
}

#[test]
fn bootstrap_eieio_core_accessor_macroexpand_matches_gnu_source_shape() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"
(progn
  (require 'eieio-core)
  (list (symbolp (get 'eieio--class-index-table 'compiler-macro))
        (eq (get 'eieio--class-index-table 'compiler-macro)
            'eieio--class-index-table--inliner)
        (eq (get 'eieio--class-index-table 'compiler-macro)
            (function-get 'eieio--class-index-table 'compiler-macro))
        (macroexpand '(setf (eieio--class-index-table class) idx))))
"#,
    );

    assert_eq!(
        rendered,
        "OK (t t t (progn (or (eieio--class-p class) (signal 'wrong-type-argument (list 'eieio--class class))) (let* ((v class)) (aset v 5 idx))))"
    );
}

#[test]
fn bootstrap_eieio_core_accessor_compiler_macro_properties_visible() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"
(progn
  (require 'eieio-core)
  (list (symbolp (get 'eieio--class-index-table 'compiler-macro))
        (eq (get 'eieio--class-index-table 'compiler-macro)
            'eieio--class-index-table--inliner)
        (eq (get 'eieio--class-index-table 'compiler-macro)
            (function-get 'eieio--class-index-table 'compiler-macro))))
"#,
    );

    assert_eq!(rendered, "OK (t t t)");
}

#[test]
fn bootstrap_eieio_core_accessor_compiler_macro_call_matches_gnu_source_shape() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"
(progn
  (require 'eieio-core)
  (apply (get 'eieio--class-index-table 'compiler-macro)
         '(eieio--class-index-table class)
         '(class)))
"#,
    );

    assert_eq!(
        rendered,
        "OK (progn (or (eieio--class-p class) (signal 'wrong-type-argument (list 'eieio--class class))) (aref class 5))"
    );
}

#[test]
fn bootstrap_eieio_runtime_defclass_metadata_matches_oracle() {
    let form = r#"
(progn
  (require 'eieio)
  (defclass neovm--dbg-point ()
    ((x :initarg :x :initform 0)
     (y :initarg :y :initform 0)))
  (unwind-protect
      (let* ((class (cl--find-class 'neovm--dbg-point))
             (obj (make-instance 'neovm--dbg-point :x 3 :y 4))
             (slots (eieio-class-slots class))
             (idx (cl--class-index-table class)))
        (list
         (mapcar #'cl--slot-descriptor-name slots)
         (symbolp (aref class 0))
         (eq (aref class 0) 'eieio--class)
         (gethash 'x idx)
         (gethash 'y idx)
         (symbolp (aref obj 0))
         (eq (aref obj 0) 'neovm--dbg-point)
         (aref obj 1)
         (aref obj 2)))
    (fmakunbound 'neovm--dbg-point)))
"#;
    crate::emacs_core::oracle_test::common::assert_oracle_parity_with_bootstrap(form);
}

#[test]
fn bootstrap_cl_extra_source_vs_compiled_cl_subseq_setf() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let cl_extra_base = project_root.join("lisp/emacs-lisp/cl-extra");
    let source_path = source_suffixed_path(&cl_extra_base);
    let compiled_path = compiled_suffixed_path(&cl_extra_base);

    let form = r#"
(let ((v (vector 1 2 3 4 5)))
  (setf (cl-subseq v 1 3) '(20 30))
  (append v nil))
"#;

    let source_rendered = cached_bootstrap_eval_with_loaded_file(&source_path, form);
    assert_eq!(source_rendered, "OK (1 20 30 4 5)");

    // Skip .elc test when compiled files are not available.
    if compiled_path.exists() {
        let compiled_rendered = cached_bootstrap_eval_with_loaded_file(&compiled_path, form);
        assert_eq!(compiled_rendered, "OK (1 20 30 4 5)");
    }
}

#[test]
fn bootstrap_cl_extra_gv_expander_requires_eval_in_source_and_compiled_paths() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let cl_extra_base = project_root.join("lisp/emacs-lisp/cl-extra");
    let source_path = source_suffixed_path(&cl_extra_base);
    let compiled_path = compiled_suffixed_path(&cl_extra_base);

    let form = r#"
(let* ((expander (function-get 'cl-subseq 'gv-expander))
       (setter-form (funcall expander (lambda (_getter setter) setter) 'v 1 3)))
  (let* ((direct
          (condition-case err
              (funcall setter-form ''(20 30))
            (invalid-function 'invalid-function)
            (error (car err))))
         (setter
          (let ((v 'placeholder-seq))
            (eval setter-form t))))
    (list direct
          (functionp setter)
          (closurep setter))))
"#;

    let source_rendered = cached_bootstrap_eval_with_loaded_file(&source_path, form);
    assert_eq!(source_rendered, "OK (invalid-function t t)");

    // Skip .elc test when compiled files are not available (NeoVM
    // loads .el source only).
    if compiled_path.exists() {
        let compiled_rendered = cached_bootstrap_eval_with_loaded_file(&compiled_path, form);
        assert_eq!(compiled_rendered, "OK (invalid-function t t)");
    }
}

#[test]
fn bootstrap_load_file_defun_gv_setter_declaration_evaluates_generated_form() {
    let source = r#"
(defun vm-loaded-gv-subseq (seq start &optional end)
  (declare
   (gv-setter
    (lambda (new)
      (macroexp-let2 nil new new
        `(progn
           (cl-replace ,seq ,new :start1 ,start :end1 ,end)
           ,new)))))
  (seq-subseq seq start end))
"#;
    let form = r#"
(let ((v (vector 1 2 3 4 5)))
  (setf (vm-loaded-gv-subseq v 1 3) '(20 30))
  (append v nil))
"#;
    let rendered = cached_bootstrap_with_loaded_source(source, form);
    assert_eq!(rendered, "OK (1 20 30 4 5)");
}

#[test]
fn bootstrap_load_file_exact_cl_subseq_shape_evaluates_generated_form() {
    let source = r#"
(defun vm-loaded-cl-subseq-shape (seq start &optional end)
  "Return the subsequence of SEQ from START to END.
If END is omitted, it defaults to the length of the sequence.
If START or END is negative, it counts from the end.
Signal an error if START or END are outside of the sequence (i.e
too large if positive or too small if negative)."
  (declare (side-effect-free t)
           (gv-setter
            (lambda (new)
              (macroexp-let2 nil new new
                `(progn (cl-replace ,seq ,new :start1 ,start :end1 ,end)
                        ,new)))))
  (seq-subseq seq start end))
"#;
    let form = r#"
(let ((v (vector 1 2 3 4 5)))
  (setf (vm-loaded-cl-subseq-shape v 1 3) '(20 30))
  (append v nil))
"#;
    let rendered = cached_bootstrap_with_loaded_source(source, form);
    assert_eq!(rendered, "OK (1 20 30 4 5)");
}

#[test]
fn cl_callf_updates_generalized_place() {
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    let form = crate::emacs_core::parser::parse_forms(
        "(let ((box (list '(3 2 1))))
           (cl-callf (lambda (slots) (apply #'vector (nreverse slots))) (car box))
           (car box))",
    )
    .expect("parse cl-callf generalized place probe");
    let result = eval
        .eval_expr(&form[0])
        .expect("evaluate cl-callf generalized place probe");
    assert_eq!(expect_vector_ints(result), vec![1, 2, 3]);
}

/// Minimal test: load enough files to get macroexpand-all + pcase working,
/// then try (macroexpand-all '(pcase x (1 "one") (2 "two"))) and see
/// if it terminates.
#[test]
fn macroexpand_all_pcase_terminates() {
    if std::env::var("NEOVM_LOADUP_TEST").as_deref() != Ok("1") {
        tracing::info!("skipping (set NEOVM_LOADUP_TEST=1)");
        return;
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("root");
    let lisp_dir = project_root.join("lisp");
    assert!(lisp_dir.is_dir());
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let subdirs = ["", "emacs-lisp"];
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
    eval.set_variable("dump-mode", Value::symbol("pbootstrap"));
    eval.set_variable("purify-flag", Value::Nil);
    eval.set_variable("max-lisp-eval-depth", Value::Int(1600));

    let load_path = get_load_path(&eval.obarray());
    let load_and_report =
        |eval: &mut crate::emacs_core::eval::Evaluator, name: &str, load_path: &[String]| {
            let path = find_file_in_load_path(name, load_path).expect(name);
            load_file(eval, &path).unwrap_or_else(|e| {
                let msg = match &e {
                    EvalError::Signal { symbol, data } => {
                        let sym = crate::emacs_core::intern::resolve_sym(*symbol);
                        let data_strs: Vec<String> = data.iter().map(|v| format!("{v}")).collect();
                        format!("({sym} {})", data_strs.join(" "))
                    }
                    other => format!("{other:?}"),
                };
                panic!("Failed to load {name}: {msg}");
            });
            tracing::info!("  loaded: {name}");
        };
    // Load minimum set: debug-early, byte-run, backquote, subr, macroexp, pcase
    for name in &[
        "emacs-lisp/debug-early",
        "emacs-lisp/byte-run",
        "emacs-lisp/backquote",
        "subr",
    ] {
        load_and_report(&mut eval, name, &load_path);
    }
    // macroexp + pcase: loaded without eager expansion since
    // get_eager_macroexpand_fn requires both internal-macroexpand-for-load
    // AND `--pcase-macroexpander to be defined.
    load_and_report(&mut eval, "emacs-lisp/macroexp", &load_path);
    load_and_report(&mut eval, "emacs-lisp/pcase", &load_path);

    // Test eager expansion with a simple defun containing pcase
    tracing::debug!("Testing eager expansion on a simple defun with cond...");
    let test_form =
        "(defun test-eager (x) (cond ((= x 1) \"one\") ((= x 2) \"two\") (t \"other\")))";
    let form_expr = &crate::emacs_core::parser::parse_forms(test_form).unwrap()[0];
    let form_value = quote_to_value(form_expr);
    let mexp_fn = eval
        .obarray()
        .symbol_function("internal-macroexpand-for-load")
        .cloned();
    match mexp_fn {
        Some(mfn) => {
            tracing::debug!("  internal-macroexpand-for-load found: {mfn}");
            match eager_expand_eval(&mut eval, form_value, mfn) {
                Ok(v) => tracing::debug!("  eager expand+eval OK: {v}"),
                Err(e) => tracing::debug!("  eager expand+eval ERR: {e:?}"),
            }
        }
        None => tracing::debug!("  internal-macroexpand-for-load NOT FOUND"),
    }

    // Test with backquote pattern (like macroexp--expand-all uses)
    tracing::debug!("Testing eager expansion on pcase with backquote pattern...");
    let test_form2 = "(pcase '(cond (t 1)) (`(cond . ,clauses) clauses) (_ nil))";
    let form_expr2 = &crate::emacs_core::parser::parse_forms(test_form2).unwrap()[0];
    match eval.eval_expr(form_expr2) {
        Ok(v) => tracing::debug!("  pcase backquote OK: {v}"),
        Err(e) => tracing::debug!("  pcase backquote ERR: {e:?}"),
    }

    tracing::debug!("All macroexpand-all pcase tests completed");
}

#[test]
fn macroexp_eager_reload_preserves_symbol_identity() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("root");
    let lisp_dir = project_root.join("lisp");
    assert!(lisp_dir.is_dir());

    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let subdirs = ["", "emacs-lisp"];
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
    eval.set_variable("dump-mode", Value::symbol("pbootstrap"));
    eval.set_variable("purify-flag", Value::Nil);
    eval.set_variable(
        "macroexp--pending-eager-loads",
        Value::list(vec![Value::symbol("skip")]),
    );

    let load_path = get_load_path(&eval.obarray());
    let load = |eval: &mut crate::emacs_core::eval::Evaluator, name: &str| {
        let path = find_file_in_load_path(name, &load_path).expect(name);
        load_file(eval, &path).unwrap_or_else(|e| panic!("failed to load {name}: {e:?}"));
    };

    for name in &[
        "emacs-lisp/debug-early",
        "emacs-lisp/byte-run",
        "emacs-lisp/backquote",
        "subr",
    ] {
        load(&mut eval, name);
    }

    let bootstrap_prefix = [
        "keymap",
        "version",
        "widget",
        "custom",
        "emacs-lisp/map-ynp",
        "international/mule",
        "international/mule-conf",
        "env",
        "format",
        "bindings",
        "window",
        "files",
    ];
    let prefix_count = std::env::var("NEOVM_MACROEXP_PREFIX_COUNT")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0)
        .min(bootstrap_prefix.len());
    for name in &bootstrap_prefix[..prefix_count] {
        load(&mut eval, name);
    }

    for name in &["emacs-lisp/macroexp", "emacs-lisp/pcase"] {
        load(&mut eval, name);
    }

    let probe = crate::emacs_core::parser::parse_forms(
        r#"(let* ((s-if (make-symbol "if"))
                  (s-message (make-symbol "message"))
                  (s-when (make-symbol "when"))
                  (s-cadr (make-symbol "cadr"))
                  (form (list s-cadr 'y)))
             (list (special-form-p s-if)
                   (functionp s-message)
                   (macrop s-when)
                   (equal (macroexpand form) form)))"#,
    )
    .expect("parse symbol identity probe");
    let probe_result = eval
        .eval_expr(&probe[0])
        .expect("evaluate symbol identity probe");
    let values =
        crate::emacs_core::value::list_to_vec(&probe_result).expect("probe should return list");
    assert_eq!(
        values,
        vec![Value::Nil, Value::Nil, Value::Nil, Value::True]
    );

    eval.set_variable("macroexp--pending-eager-loads", Value::Nil);
    load(&mut eval, "emacs-lisp/macroexp");
}

#[test]
fn function_get_only_exposes_cxxr_compiler_macro_on_cxxr_symbols() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("root");
    let lisp_dir = project_root.join("lisp");
    assert!(lisp_dir.is_dir());

    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let mut load_path_entries = Vec::new();
    for sub in ["", "emacs-lisp"] {
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

    let load_path = get_load_path(&eval.obarray());
    for name in &[
        "emacs-lisp/debug-early",
        "emacs-lisp/byte-run",
        "emacs-lisp/backquote",
        "subr",
    ] {
        let path = find_file_in_load_path(name, &load_path).expect(name);
        load_file(&mut eval, &path).unwrap_or_else(|e| panic!("failed to load {name}: {e:?}"));
    }

    let probe = crate::emacs_core::parser::parse_forms(
        r#"(list (if (function-get 'car 'compiler-macro) t nil)
                 (if (function-get 'cdr 'compiler-macro) t nil)
                 (if (function-get 'cadr 'compiler-macro) t nil))"#,
    )
    .expect("parse function-get probe");
    let result = eval
        .eval_expr(&probe[0])
        .expect("evaluate function-get probe");
    assert_eq!(
        crate::emacs_core::value::list_to_vec(&result).expect("probe should return list"),
        vec![Value::Nil, Value::Nil, Value::True]
    );
}

/// Test pcase with integer literal patterns — reproduces the
/// "Unknown pattern '32'" error from rx.el line 1284.
#[test]
fn pcase_integer_literal_pattern() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug")),
        )
        .with_test_writer()
        .try_init();
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("root");
    let lisp_dir = project_root.join("lisp");
    assert!(lisp_dir.is_dir());
    let mut eval = crate::emacs_core::eval::Evaluator::new();
    let subdirs = ["", "emacs-lisp"];
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
    eval.set_variable("dump-mode", Value::symbol("pbootstrap"));
    eval.set_variable("purify-flag", Value::Nil);
    eval.set_variable("max-lisp-eval-depth", Value::Int(1600));

    let load_path = get_load_path(&eval.obarray());
    let load_and_report =
        |eval: &mut crate::emacs_core::eval::Evaluator, name: &str, load_path: &[String]| {
            let path = find_file_in_load_path(name, load_path).expect(name);
            load_file(eval, &path).unwrap_or_else(|e| {
                let msg = match &e {
                    EvalError::Signal { symbol, data } => {
                        let sym = crate::emacs_core::intern::resolve_sym(*symbol);
                        let data_strs: Vec<String> = data.iter().map(|v| format!("{v}")).collect();
                        format!("({sym} {})", data_strs.join(" "))
                    }
                    other => format!("{other:?}"),
                };
                panic!("Failed to load {name}: {msg}");
            });
            tracing::info!("  loaded: {name}");
        };
    for name in &[
        "emacs-lisp/debug-early",
        "emacs-lisp/byte-run",
        "emacs-lisp/backquote",
        "subr",
        "emacs-lisp/macroexp",
        "emacs-lisp/pcase",
    ] {
        load_and_report(&mut eval, name, &load_path);
    }

    // Test 1: basic integer pattern
    tracing::info!("Test 1: pcase with integer literal 32");
    let form1 = r#"(pcase 32 (32 "matched") (_ "no-match"))"#;
    let expr1 = &crate::emacs_core::parser::parse_forms(form1).unwrap()[0];
    match eval.eval_expr(expr1) {
        Ok(v) => tracing::info!("  Test 1 OK: {v}"),
        Err(e) => tracing::error!("  Test 1 FAILED: {e:?}"),
    }

    // Test 2: (or 'sym int) pattern — exact pattern from rx.el:1284
    tracing::info!("Test 2: pcase with (or 'sym int) — rx.el pattern");
    let form2 = r#"(pcase ?\s ((or '\? ?\s) "matched") (_ "no-match"))"#;
    let expr2 = &crate::emacs_core::parser::parse_forms(form2).unwrap()[0];
    match eval.eval_expr(expr2) {
        Ok(v) => tracing::info!("  Test 2 OK: {v}"),
        Err(e) => tracing::error!("  Test 2 FAILED: {e:?}"),
    }

    // Test 3: (or int int) pattern
    tracing::info!("Test 3: pcase with (or int int)");
    let form3 = r#"(pcase 32 ((or 32 63) "matched") (_ "no-match"))"#;
    let expr3 = &crate::emacs_core::parser::parse_forms(form3).unwrap()[0];
    match eval.eval_expr(expr3) {
        Ok(v) => tracing::info!("  Test 3 OK: {v}"),
        Err(e) => tracing::error!("  Test 3 FAILED: {e:?}"),
    }

    // Test 4: pcase inside a defun then call it (simulates rx--translate-form)
    tracing::info!("Test 4: pcase inside defun");
    let form4 = r#"(progn
      (defun test-pcase-int (x)
        (pcase x
          ((or '\? ?\s) "question-or-space")
          ('seq "seq")
          (_ "other")))
      (list (test-pcase-int 'seq)
            (test-pcase-int ?\s)
            (test-pcase-int '\?)
            (test-pcase-int 'foo)))"#;
    let expr4 = &crate::emacs_core::parser::parse_forms(form4).unwrap()[0];
    match eval.eval_expr(expr4) {
        Ok(v) => tracing::info!("  Test 4 OK: {v}"),
        Err(e) => tracing::error!("  Test 4 FAILED: {e:?}"),
    }

    // Test 5: get the actual error message
    tracing::info!("Test 5: capture error message from (or 'sym int)");
    let form5 = r#"(condition-case err
        (pcase ?\s ((or '\? ?\s) "matched") (_ "no-match"))
      (error (error-message-string err)))"#;
    let expr5 = &crate::emacs_core::parser::parse_forms(form5).unwrap()[0];
    match eval.eval_expr(expr5) {
        Ok(v) => tracing::info!("  Test 5 result: {v}"),
        Err(e) => tracing::error!("  Test 5 FAILED: {e:?}"),
    }

    // Test 6: (or 'sym 'sym) — should work fine
    tracing::info!("Test 6: (or 'sym 'sym)");
    let form6 = r#"(pcase 'foo ((or 'foo 'bar) "matched") (_ "no"))"#;
    let expr6 = &crate::emacs_core::parser::parse_forms(form6).unwrap()[0];
    match eval.eval_expr(expr6) {
        Ok(v) => tracing::info!("  Test 6 OK: {v}"),
        Err(e) => tracing::error!("  Test 6 FAILED: {e:?}"),
    }

    // Test 7: (or int 'sym) — reversed order
    tracing::info!("Test 7: (or int 'sym) — reversed");
    let form7 = r#"(pcase 32 ((or 32 'foo) "matched") (_ "no"))"#;
    let expr7 = &crate::emacs_core::parser::parse_forms(form7).unwrap()[0];
    match eval.eval_expr(expr7) {
        Ok(v) => tracing::info!("  Test 7 OK: {v}"),
        Err(e) => tracing::error!("  Test 7 FAILED: {e:?}"),
    }

    // Test 8: just macroexpand the problematic form
    tracing::info!("Test 8: macroexpand-1 the (or 'sym int) pcase");
    let form8 = r#"(macroexpand '(pcase x ((or '\? 32) "yes") (_ "no")))"#;
    let expr8 = &crate::emacs_core::parser::parse_forms(form8).unwrap()[0];
    match eval.eval_expr(expr8) {
        Ok(v) => tracing::info!("  Test 8 expansion: {v}"),
        Err(e) => tracing::error!("  Test 8 FAILED: {e:?}"),
    }

    // Test 9: check what pcase--macroexpand does with integer
    tracing::info!("Test 9: pcase--macroexpand on raw integer");
    let form9 = r#"(pcase--macroexpand 32)"#;
    let expr9 = &crate::emacs_core::parser::parse_forms(form9).unwrap()[0];
    match eval.eval_expr(expr9) {
        Ok(v) => tracing::info!("  Test 9 result: {v}"),
        Err(e) => tracing::error!("  Test 9 FAILED: {e:?}"),
    }

    // Test 10: check pcase--self-quoting-p
    tracing::info!("Test 10: pcase--self-quoting-p 32");
    let form10 = r#"(pcase--self-quoting-p 32)"#;
    let expr10 = &crate::emacs_core::parser::parse_forms(form10).unwrap()[0];
    match eval.eval_expr(expr10) {
        Ok(v) => tracing::info!("  Test 10 result: {v}"),
        Err(e) => tracing::error!("  Test 10 FAILED: {e:?}"),
    }

    tracing::info!("pcase integer literal tests completed");
}

#[test]
fn key_parse_modifier_bits() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();

    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let lisp_dir = project_root.join("lisp");
    if !lisp_dir.is_dir() {
        tracing::info!("skipping key_parse_modifier_bits: no lisp/ directory");
        return;
    }

    let mut eval = crate::emacs_core::eval::Evaluator::new();

    // Set up load-path
    let subdirs = ["", "emacs-lisp"];
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
    eval.set_variable("dump-mode", Value::symbol("pbootstrap"));
    eval.set_variable("purify-flag", Value::Nil);

    // Load the minimum bootstrap: debug-early, byte-run, backquote, subr, keymap
    let load_path = get_load_path(&eval.obarray());
    for name in &[
        "emacs-lisp/debug-early",
        "emacs-lisp/byte-run",
        "emacs-lisp/backquote",
        "subr",
        "keymap",
    ] {
        let path = find_file_in_load_path(name, &load_path)
            .unwrap_or_else(|| panic!("cannot find {name} in load-path"));
        load_file(&mut eval, &path).unwrap_or_else(|e| panic!("failed to load {name}: {e:?}"));
    }

    // Test key-parse with various modifier keys
    let test_cases = [
        // key-parse tests
        ("(key-parse \"C-M-q\")", "key-parse C-M-q"),
        // keymap-set with key string
        (
            "(let ((map (make-sparse-keymap))) (keymap-set map \"C-M-q\" #'ignore) map)",
            "keymap-set C-M-q",
        ),
        // defvar-keymap
        (
            "(defvar-keymap test-prog-mode-map :doc \"test\" \"C-M-q\" #'ignore \"M-q\" #'ignore)",
            "defvar-keymap",
        ),
    ];

    for (expr_str, desc) in &test_cases {
        let forms = super::super::parser::parse_forms(expr_str)
            .unwrap_or_else(|e| panic!("parse error for {expr_str}: {e:?}"));
        match eval.eval_expr(&forms[0]) {
            Ok(val) => tracing::debug!("  OK: {desc}: {expr_str} => {val}"),
            Err(e) => {
                let msg = match &e {
                    EvalError::Signal { symbol, data } => {
                        let sym = super::super::intern::resolve_sym(*symbol);
                        let data_strs: Vec<String> = data.iter().map(|v| format!("{v}")).collect();
                        format!("({sym} {})", data_strs.join(" "))
                    }
                    EvalError::UncaughtThrow { tag, value } => {
                        format!("(throw {tag} {value})")
                    }
                };
                tracing::error!("FAIL: {desc}: {expr_str} => {msg}");
            }
        }
    }

    // The critical test: key-parse "C-x" should succeed (not error)
    let forms =
        super::super::parser::parse_forms("(key-parse \"C-x\")").expect("parse key-parse call");
    let result = eval.eval_expr(&forms[0]);
    match &result {
        Err(EvalError::Signal { symbol, data }) => {
            let sym = super::super::intern::resolve_sym(*symbol);
            let data_strs: Vec<String> = data.iter().map(|v| format!("{v}")).collect();
            panic!("key-parse \"C-x\" failed: ({sym} {})", data_strs.join(" "));
        }
        Err(e) => panic!("key-parse \"C-x\" failed: {e:?}"),
        Ok(val) => tracing::debug!("key-parse \"C-x\" => {val}"),
    }
}

#[test]
fn char_literal_roundtrip() {
    use crate::emacs_core::expr::print_expr;

    let cases: Vec<(char, &str)> = vec![
        ('A', "?A"),
        ('z', "?z"),
        ('0', "?0"),
        (' ', "?\\ "),
        ('\\', "?\\\\"),
        ('\n', "?\\n"),
        ('\t', "?\\t"),
        ('\r', "?\\r"),
        ('\x07', "?\\a"),
        ('\x08', "?\\b"),
        ('\x0C', "?\\f"),
        ('\x1B', "?\\e"),
        ('\x7F', "?\\d"),
        ('(', "?\\("),
        (')', "?\\)"),
        ('[', "?\\["),
        (']', "?\\]"),
        ('"', "?\\\""),
        (';', "?\\;"),
        ('#', "?\\#"),
        ('\'', "?\\'"),
        ('`', "?\\`"),
        (',', "?\\,"),
    ];

    for (ch, expected_print) in &cases {
        let expr = Expr::Char(*ch);
        let printed = print_expr(&expr);
        assert_eq!(
            &printed, expected_print,
            "print_expr(Char({:?})) should be {}",
            ch, expected_print
        );

        // Round-trip: print → parse → should get Char back
        let parsed = super::super::parser::parse_forms(&printed).expect(&format!(
            "parse should succeed for printed char literal: {}",
            printed
        ));
        assert_eq!(
            parsed.len(),
            1,
            "should parse exactly one form from {printed}"
        );
        match &parsed[0] {
            Expr::Char(c) => assert_eq!(
                *c, *ch,
                "round-trip Char({:?}) → print → parse should yield same char",
                ch
            ),
            other => panic!(
                "round-trip Char({:?}) → print '{printed}' → parse yielded {:?}, expected Char",
                ch, other
            ),
        }
    }
}

#[test]
fn expanded_cache_round_trip() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-v2-cache-rt-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    let source = "(defun foo () 42)\n(setq bar 'baz)\n";
    fs::write(&file, source).expect("write fixture");

    let forms = vec![
        Expr::List(vec![
            Expr::Symbol(intern("defun")),
            Expr::Symbol(intern("foo")),
            Expr::List(vec![]),
            Expr::Int(42),
        ]),
        Expr::List(vec![
            Expr::Symbol(intern("setq")),
            Expr::Symbol(intern("bar")),
            Expr::List(vec![
                Expr::Symbol(intern("quote")),
                Expr::Symbol(intern("baz")),
            ]),
        ]),
    ];

    write_expanded_cache(&file, source, true, &forms).expect("write V2 cache");
    let loaded = maybe_load_expanded_cache(&file, source, true);
    assert!(loaded.is_some(), "V2 cache should load successfully");
    let loaded_forms = loaded.unwrap();
    assert_eq!(
        loaded_forms.len(),
        forms.len(),
        "should have same number of forms"
    );

    // Verify structural equality via print_expr round-trip
    for (i, (orig, loaded)) in forms.iter().zip(loaded_forms.iter()).enumerate() {
        assert_eq!(
            print_expr(orig),
            print_expr(loaded),
            "form {i} should round-trip through V2 cache"
        );
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn expanded_cache_preserves_uninterned_symbol_identity() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-v2-cache-uninterned-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    let source = "(let* ((#:exp 1) (x #:exp)) x)\n";
    fs::write(&file, source).expect("write fixture");

    let mut eval = Evaluator::new();
    let exp = crate::emacs_core::intern::intern_uninterned("exp");
    let forms = vec![Expr::List(vec![
        Expr::Symbol(intern("let*")),
        Expr::List(vec![
            Expr::List(vec![Expr::Symbol(exp), Expr::Int(1)]),
            Expr::List(vec![Expr::Symbol(intern("x")), Expr::Symbol(exp)]),
        ]),
        Expr::Symbol(intern("x")),
    ])];

    write_expanded_cache(&file, source, true, &forms).expect("write V2 cache");
    let loaded = maybe_load_expanded_cache(&file, source, true).expect("load V2 cache");
    let value = eval.eval_expr(&loaded[0]).expect("evaluate cached form");
    assert_eq!(
        crate::emacs_core::print::print_value_with_buffers(&value, &eval.buffers),
        "1"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn expanded_cache_invalidated_by_source_change() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-v2-cache-inv-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");

    let source_v1 = "(setq x 1)\n";
    fs::write(&file, source_v1).expect("write fixture v1");

    let forms = vec![Expr::List(vec![
        Expr::Symbol(intern("setq")),
        Expr::Symbol(intern("x")),
        Expr::Int(1),
    ])];

    write_expanded_cache(&file, source_v1, true, &forms).expect("write V2 cache");

    // Modify source — cache should be invalidated
    let source_v2 = "(setq x 2)\n";
    let loaded = maybe_load_expanded_cache(&file, source_v2, true);
    assert!(
        loaded.is_none(),
        "V2 cache should be invalidated when source changes"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn v3_cache_overwrites_v1() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-v2-overwrites-v1-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    let source = "(setq x 1)\n";
    fs::write(&file, source).expect("write fixture");

    let forms = vec![Expr::List(vec![
        Expr::Symbol(intern("setq")),
        Expr::Symbol(intern("x")),
        Expr::Int(1),
    ])];

    // Write V1 cache first
    write_forms_cache(&file, source, true, &forms).expect("write V1 cache");
    assert!(
        maybe_load_cached_forms(&file, source, true).is_some(),
        "V1 cache should be readable"
    );

    // Write V3 cache — overwrites the same .neoc file
    write_expanded_cache(&file, source, true, &forms).expect("write V2 cache");

    // V1 reader should NOT match V3 cache (different magic)
    assert!(
        maybe_load_cached_forms(&file, source, true).is_none(),
        "V1 reader should return None after V3 overwrites the cache file"
    );

    // V3 reader should still work
    assert!(
        maybe_load_expanded_cache(&file, source, true).is_some(),
        "V3 reader should still work after overwrite"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn v3_reader_rejects_legacy_textual_v2_cache() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-v3-rejects-v2-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    let source = "(let* ((#:exp 1) (x #:exp)) x)\n";
    fs::write(&file, source).expect("write fixture");

    let legacy_payload = format!(
        "NEOVM-ELISP-CACHE-V2\nkey=schema=2;vm={};lexical=1\nsource-hash={:016x}\n\n{}",
        ELISP_CACHE_VM_VERSION,
        source_hash(source),
        source.trim_end()
    );
    fs::write(cache_sidecar_path(&file), legacy_payload).expect("write legacy V2 cache");

    assert!(
        maybe_load_expanded_cache(&file, source, true).is_none(),
        "V3 reader must reject legacy textual V2 caches"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn v3_reader_accepts_lossless_legacy_textual_v2_cache() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-v3-accepts-v2-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    let source = "(setq x '(1 2 3))\n";
    fs::write(&file, source).expect("write fixture");

    let legacy_payload = format!(
        "{ELISP_EXPANDED_CACHE_LEGACY_MAGIC}\nkey={}\nsource-hash={:016x}\n\n{}",
        legacy_expanded_cache_key(true),
        source_hash(source),
        source.trim_end()
    );
    fs::write(cache_sidecar_path(&file), legacy_payload).expect("write legacy V2 cache");

    let loaded = maybe_load_expanded_cache(&file, source, true).expect("load legacy V2 cache");
    assert_eq!(
        loaded,
        crate::emacs_core::parser::parse_forms(source).expect("parse source"),
        "lossless legacy V2 cache should still load"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn generated_loaddefs_replays_metadata_forms_without_generic_eval_overhead() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-generated-loaddefs-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("generated-loaddefs.el");
    let source = r#";;; loaddefs.el --- automatically extracted autoloads (do not edit)   -*- lexical-binding: t -*-
;; Generated by the `loaddefs-generate' function.

(autoload 'vm-generated-fn "vm-generated" "Doc." t)
(register-definition-prefixes "vm-generated" '("vm-generated-"))
(defvar vm-generated-option nil "Generated option.")
(custom-autoload 'vm-generated-option "vm-generated" t)
(put 'vm-generated-option 'safe-local-variable #'symbolp)
(function-put 'vm-generated-fn 'interactive-only 'vm-generated-target)
(define-obsolete-function-alias 'vm-generated-old #'vm-generated-fn "31.1" "Old doc.")
"#;
    fs::write(&file, source).expect("write generated loaddefs fixture");

    let mut eval = Evaluator::new();
    eval.set_variable(
        "definition-prefixes",
        Value::hash_table(HashTableTest::Equal),
    );

    load_file(&mut eval, &file).unwrap_or_else(|err| {
        panic!(
            "generated loaddefs should load: {}",
            format_eval_error(&eval, &err)
        )
    });

    let autoload = eval
        .obarray()
        .symbol_function("vm-generated-fn")
        .copied()
        .expect("autoload function cell");
    assert!(
        crate::emacs_core::autoload::is_autoload_value(&autoload),
        "autoload form should be installed"
    );

    let prefixes = crate::emacs_core::builtins::builtin_gethash(vec![
        Value::string("vm-generated-"),
        eval.obarray()
            .symbol_value("definition-prefixes")
            .copied()
            .expect("definition-prefixes table"),
    ])
    .expect("gethash definition-prefixes");
    let prefix_items = crate::emacs_core::value::list_to_vec(&prefixes)
        .expect("definition-prefixes entry should be a list");
    assert_eq!(prefix_items, vec![Value::string("vm-generated")]);

    let custom_autoload = crate::emacs_core::builtins::builtin_get(
        &mut eval,
        vec![
            Value::symbol("vm-generated-option"),
            Value::symbol("custom-autoload"),
        ],
    )
    .expect("custom-autoload property");
    assert_eq!(custom_autoload, Value::symbol("noset"));

    let custom_loads = crate::emacs_core::builtins::builtin_get(
        &mut eval,
        vec![
            Value::symbol("vm-generated-option"),
            Value::symbol("custom-loads"),
        ],
    )
    .expect("custom-loads property");
    let custom_loads_items = crate::emacs_core::value::list_to_vec(&custom_loads)
        .expect("custom-loads should be a list");
    assert_eq!(custom_loads_items, vec![Value::string("vm-generated")]);

    let safe_local = crate::emacs_core::builtins::builtin_get(
        &mut eval,
        vec![
            Value::symbol("vm-generated-option"),
            Value::symbol("safe-local-variable"),
        ],
    )
    .expect("safe-local-variable property");
    assert_eq!(safe_local, Value::symbol("symbolp"));

    let interactive_only = crate::emacs_core::builtins::builtin_get(
        &mut eval,
        vec![
            Value::symbol("vm-generated-fn"),
            Value::symbol("interactive-only"),
        ],
    )
    .expect("interactive-only property");
    assert_eq!(interactive_only, Value::symbol("vm-generated-target"));

    let old_function = eval
        .obarray()
        .symbol_function("vm-generated-old")
        .copied()
        .expect("obsolete alias function cell");
    assert_eq!(old_function, Value::symbol("vm-generated-fn"));

    let obsolete_info = crate::emacs_core::builtins::builtin_get(
        &mut eval,
        vec![
            Value::symbol("vm-generated-old"),
            Value::symbol("byte-obsolete-info"),
        ],
    )
    .expect("byte-obsolete-info property");
    let obsolete_items =
        crate::emacs_core::value::list_to_vec(&obsolete_info).expect("obsolete info list");
    assert_eq!(
        obsolete_items,
        vec![
            Value::symbol("vm-generated-fn"),
            Value::Nil,
            Value::string("31.1"),
        ]
    );

    let old_doc = crate::emacs_core::builtins::builtin_get(
        &mut eval,
        vec![
            Value::symbol("vm-generated-old"),
            Value::symbol("function-documentation"),
        ],
    )
    .expect("function-documentation property");
    assert_eq!(old_doc, Value::string("Old doc."));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn contains_opaque_value_detection() {
    // Plain forms should not contain opaque values
    let plain = Expr::List(vec![
        Expr::Symbol(intern("setq")),
        Expr::Symbol(intern("x")),
        Expr::Int(42),
    ]);
    assert!(
        !plain.contains_opaque_value(),
        "plain form should not contain opaque values"
    );

    // Form with OpaqueValue should be detected
    let opaque = Expr::List(vec![
        Expr::Symbol(intern("quote")),
        Expr::OpaqueValue(Value::Int(99)),
    ]);
    assert!(
        opaque.contains_opaque_value(),
        "form with OpaqueValue should be detected"
    );

    // Nested OpaqueValue in vector
    let nested = Expr::Vector(vec![
        Expr::Int(1),
        Expr::List(vec![Expr::OpaqueValue(Value::True)]),
    ]);
    assert!(
        nested.contains_opaque_value(),
        "nested OpaqueValue should be detected"
    );

    // DottedList with OpaqueValue in tail
    let dotted = Expr::DottedList(vec![Expr::Int(1)], Box::new(Expr::OpaqueValue(Value::Nil)));
    assert!(
        dotted.contains_opaque_value(),
        "OpaqueValue in dotted tail should be detected"
    );

    // DottedList without OpaqueValue
    let dotted_clean = Expr::DottedList(vec![Expr::Int(1)], Box::new(Expr::Int(2)));
    assert!(
        !dotted_clean.contains_opaque_value(),
        "clean dotted list should not contain opaque values"
    );
}

#[test]
fn bootstrap_interpreted_closure_body_shape_matches_gnu_emacs() {
    let form = r#"(let* ((compose (lambda (f g) (lambda (x) (funcall f (funcall g x)))))
         (church-zero (lambda (f) (lambda (x) x))))
    (list (aref compose 1)
          (aref church-zero 1)))"#;
    let rendered = crate::emacs_core::oracle_test::common::run_neovm_eval_with_bootstrap(form)
        .expect("bootstrap eval should run");
    assert_eq!(
        rendered,
        "OK (((lambda (x) (funcall f (funcall g x)))) (#'(lambda (x) x)))"
    );
}
