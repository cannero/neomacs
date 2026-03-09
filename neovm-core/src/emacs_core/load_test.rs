use super::*;
use crate::emacs_core::eval::Evaluator;
use crate::emacs_core::expr::Expr;
use crate::emacs_core::intern::{intern, resolve_sym};
use crate::emacs_core::value::{HashTableTest, Value, with_heap};
use crate::emacs_core::{format_eval_result, parse_forms};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

struct CacheWriteFailGuard;

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
              (mapcar (lambda (nil) nil) '(4 5 6))))",
    )
    .expect("parse");
    let result = eval.eval_expr(&forms[0]);
    assert_eq!(
        format_eval_result(&result),
        "OK (7 9 t 7 (1 2 3) (4 5 6))",
        "bootstrap evaluator should match GNU's special-symbol parameter binding"
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
               (fboundp 'gv-get)
               (autoloadp (symbol-function 'gv-get))
               (fboundp 'setf)
               (autoloadp (symbol-function 'setf))
               (fboundp 'emacs-lisp-mode)
               (autoloadp (symbol-function 'emacs-lisp-mode))
               (functionp (symbol-function 'emacs-lisp-mode)))",
    );
    assert_eq!(
        rendered, "OK (nil nil nil nil nil t t nil nil nil nil nil nil nil nil t t t t t nil t)",
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
               (featurep 'oclosure))",
    );
    assert_eq!(
        rendered, "OK (t nil t nil t t t t)",
        "bootstrap runtime should match GNU -Q oclosure/nadvice surface"
    );
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
    fs::write(&plain, "plain").expect("write plain fixture");
    fs::write(&el, "el").expect("write el fixture");

    let load_path = vec![dir.to_string_lossy().to_string()];

    // Default mode prefers suffixed files.
    assert_eq!(
        find_file_in_load_path_with_flags("choice", &load_path, false, false, false),
        Some(el.clone())
    );
    // no-suffix mode only tries exact name.
    assert_eq!(
        find_file_in_load_path_with_flags("choice", &load_path, true, false, false),
        Some(plain.clone())
    );
    // must-suffix mode rejects plain file and requires suffixed one.
    assert_eq!(
        find_file_in_load_path_with_flags("choice", &load_path, false, true, false),
        Some(el)
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
        Some(el.clone())
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
        eprintln!("pdump advice lifecycle step: {label}");
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
    let compiled_rendered = cached_bootstrap_eval_with_loaded_file(&compiled_path, form);

    assert_eq!(source_rendered, "OK (1 20 30 4 5)");
    assert_eq!(compiled_rendered, "OK (1 20 30 4 5)");
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
    let compiled_rendered = cached_bootstrap_eval_with_loaded_file(&compiled_path, form);

    assert_eq!(source_rendered, "OK (invalid-function t t)");
    assert_eq!(compiled_rendered, "OK (invalid-function t t)");
}

#[test]
fn debug_compiled_cl_extra_setter_bytecode_disasm() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let cl_extra_base = project_root.join("lisp/emacs-lisp/cl-extra");
    let compiled_path = compiled_suffixed_path(&cl_extra_base);

    let form = r#"
(let* ((expander (function-get 'cl-subseq 'gv-expander)))
  (funcall expander (lambda (_getter setter) setter) 'v 1 3))
"#;

    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    load_file(&mut eval, &compiled_path).expect("load compiled cl-extra");

    let expander_form = r#"(function-get 'cl-subseq 'gv-expander)"#;
    let parsed_expander = crate::emacs_core::parser::parse_forms(expander_form).expect("parse");
    let expander = eval
        .eval_expr(&parsed_expander[0])
        .expect("evaluate expander");
    let gv_defsetter = eval
        .obarray()
        .symbol_function("gv--defsetter")
        .copied()
        .expect("gv--defsetter fboundp");
    eprintln!(
        "gv--defsetter runtime value: {}",
        crate::emacs_core::print::print_value_with_buffers(&gv_defsetter, &eval.buffers)
    );
    if let Some(bc) = gv_defsetter.get_bytecode_data() {
        eprintln!("gv--defsetter bytecode:\n{}", bc.disassemble());
    }
    let expander_bc = expander.get_bytecode_data().expect("expander bytecode");
    eprintln!("compiled expander bytecode:\n{}", expander_bc.disassemble());

    let parsed = crate::emacs_core::parser::parse_forms(form).expect("parse");
    let value = eval.eval_expr(&parsed[0]).expect("evaluate setter form");
    eprintln!(
        "compiled setter form: {}",
        crate::emacs_core::print::print_value_with_buffers(&value, &eval.buffers)
    );

    let items = crate::emacs_core::value::list_to_vec(&value).expect("setter list");
    let body = items.last().copied().expect("setter body");
    let bc = body.get_bytecode_data().expect("setter bytecode");
    eprintln!("compiled setter bytecode:\n{}", bc.disassemble());
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
