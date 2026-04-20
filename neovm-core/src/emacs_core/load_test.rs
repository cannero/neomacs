use super::*;
use crate::emacs_core::eval::Context;
use crate::emacs_core::fontset::{
    DEFAULT_FONTSET_NAME, FontSpecEntry, matching_entries_for_fontset,
};
use crate::emacs_core::format_eval_result;
use crate::emacs_core::intern::{intern, resolve_sym};
use crate::emacs_core::value::{
    HashKey, HashTableTest, Value, ValueKind, VecLikeType, equal_value, list_to_vec,
};
use crate::test_utils::load_minimal_gnu_help_runtime;
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::tempdir;

fn isolated_runtime_bootstrap_eval() -> Context {
    let dump_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/test-cache/neovm-advice-stack-minibuffer-partial.pdump");
    std::fs::create_dir_all(
        dump_path
            .parent()
            .expect("advice-stack partial bootstrap cache parent"),
    )
    .expect("create advice-stack partial bootstrap cache dir");
    if dump_path.exists()
        && let Ok(eval) = crate::emacs_core::pdump::load_from_dump(&dump_path)
    {
        return eval;
    }

    let eval = partial_bootstrap_eval_until("minibuffer", true);
    crate::emacs_core::pdump::dump_to_file(&eval, &dump_path)
        .expect("cache advice-stack partial bootstrap");
    eval
}

fn bootstrap_lisp_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().expect("project root").join("lisp")
}

fn source_bootstrap_path(rel: &str) -> PathBuf {
    bootstrap_lisp_root().join(rel)
}

fn copy_source_fixture(dir: &std::path::Path, rel: &str) -> PathBuf {
    let source = source_bootstrap_path(rel);
    let copied = dir.join(rel);
    if let Some(parent) = copied.parent() {
        std::fs::create_dir_all(parent).unwrap_or_else(|err| {
            panic!("create temp source fixture dir {}: {err}", parent.display())
        });
    }
    std::fs::copy(&source, &copied).unwrap_or_else(|err| {
        panic!(
            "copy source fixture {} -> {}: {err}",
            source.display(),
            copied.display()
        )
    });
    copied
}

#[test]
fn loaded_source_paths_accepts_raw_unibyte_load_history_entries() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let raw_path = Value::heap_string(crate::heap_types::LispString::from_unibyte(
        b"/tmp/\xFF.el".to_vec(),
    ));
    eval.obarray.set_symbol_value(
        "load-history",
        Value::list(vec![Value::cons(raw_path, Value::NIL)]),
    );

    let paths = loaded_source_paths(&mut eval);
    assert_eq!(paths.len(), 1);
    #[cfg(unix)]
    assert_eq!(paths[0].as_os_str().as_bytes(), b"/tmp/\xFF.el");
    #[cfg(not(unix))]
    assert_eq!(
        paths[0].to_string_lossy(),
        crate::emacs_core::builtins::lisp_string_to_runtime_string(raw_path)
    );
}

fn definition_is_macroish(value: Value) -> bool {
    value.is_macro() || (value.is_cons() && value.cons_car().as_symbol_name() == Some("macro"))
}

fn is_named_defun(form: Value, name: &str) -> bool {
    if !form.is_cons() {
        return false;
    }
    let car = form.cons_car();
    let cdr = form.cons_cdr();
    if car.as_symbol_name() != Some("defun") {
        return false;
    }
    if !cdr.is_cons() {
        return false;
    }
    cdr.cons_car().as_symbol_name() == Some(name)
}

#[test]
fn cached_bootstrap_evaluator_clears_top_level_eval_state() {
    crate::test_utils::init_test_tracing();
    let eval =
        create_bootstrap_evaluator_cached_with_features(&["neomacs"]).expect("bootstrap evaluator");
    assert!(
        eval.top_level_eval_state_is_clean(),
        "cached bootstrap evaluator should not retain stale lexenv/specpdl state"
    );
}

#[test]
fn dump_emacs_portable_writes_reloadable_snapshot() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.set_variable("dump-portable-test-var", Value::fixnum(42));

    let dir = tempdir().expect("dump tempdir");
    let dump_path = dir.path().join("portable-test.pdump");
    crate::emacs_core::builtins::builtin_dump_emacs_portable(
        &mut eval,
        vec![Value::string(dump_path.to_string_lossy().into_owned())],
    )
    .expect("dump-emacs-portable should succeed");

    let loaded = crate::emacs_core::pdump::load_from_dump(&dump_path)
        .expect("reloading dumped snapshot should succeed");
    assert_eq!(
        loaded.obarray().symbol_value("dump-portable-test-var"),
        Some(&Value::fixnum(42))
    );
}

#[test]
fn dump_emacs_portable_overwrites_existing_target() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let dir = tempdir().expect("dump tempdir");
    let dump_path = dir.path().join("portable-overwrite-test.pdump");

    eval.set_variable("dump-portable-test-var", Value::fixnum(1));
    crate::emacs_core::builtins::builtin_dump_emacs_portable(
        &mut eval,
        vec![Value::string(dump_path.to_string_lossy().into_owned())],
    )
    .expect("first dump-emacs-portable should succeed");

    eval.set_variable("dump-portable-test-var", Value::fixnum(2));
    crate::emacs_core::builtins::builtin_dump_emacs_portable(
        &mut eval,
        vec![Value::string(dump_path.to_string_lossy().into_owned())],
    )
    .expect("second dump-emacs-portable should overwrite");

    let loaded = crate::emacs_core::pdump::load_from_dump(&dump_path)
        .expect("reloading overwritten snapshot should succeed");
    assert_eq!(
        loaded.obarray().symbol_value("dump-portable-test-var"),
        Some(&Value::fixnum(2))
    );
}

#[test]
fn dump_emacs_portable_expands_relative_target_against_default_directory() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let dir = tempdir().expect("dump tempdir");
    let default_dir = dir.path().join("default-dir");
    std::fs::create_dir_all(&default_dir).expect("create default-directory");
    eval.set_variable(
        "default-directory",
        Value::string(format!("{}/", default_dir.to_string_lossy())),
    );
    eval.set_variable("dump-portable-test-var", Value::fixnum(7));

    crate::emacs_core::builtins::builtin_dump_emacs_portable(
        &mut eval,
        vec![Value::string("relative-portable-test.pdump")],
    )
    .expect("relative dump-emacs-portable should succeed");

    let dump_path = default_dir.join("relative-portable-test.pdump");
    assert!(
        dump_path.exists(),
        "dump-emacs-portable should expand relative names against default-directory"
    );

    let loaded = crate::emacs_core::pdump::load_from_dump(&dump_path)
        .expect("reloading relative dump snapshot should succeed");
    assert_eq!(
        loaded.obarray().symbol_value("dump-portable-test-var"),
        Some(&Value::fixnum(7))
    );
}

#[test]
fn dump_emacs_portable_requires_batch_mode() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.set_variable("noninteractive", Value::NIL);

    let err = crate::emacs_core::builtins::builtin_dump_emacs_portable(
        &mut eval,
        vec![Value::string("/tmp/portable-batch-mode-test.pdump")],
    )
    .expect_err("interactive dump-emacs-portable should fail");

    match err {
        crate::emacs_core::error::Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data.len(), 1);
            assert!(
                sig.data[0]
                    .as_str_owned()
                    .is_some_and(|message| message.contains("only in batch mode")),
                "unexpected error payload: {:?}",
                sig.data
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn dump_emacs_portable_rejects_other_live_lisp_threads() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.threads.create_thread(
        Value::NIL,
        Some(crate::heap_types::LispString::from_unibyte(
            b"worker".to_vec(),
        )),
    );

    let err = crate::emacs_core::builtins::builtin_dump_emacs_portable(
        &mut eval,
        vec![Value::string("/tmp/portable-thread-test.pdump")],
    )
    .expect_err("dump-emacs-portable should reject other live threads");

    match err {
        crate::emacs_core::error::Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(sig.data.len(), 1);
            assert!(
                sig.data[0].as_str_owned().is_some_and(|message| {
                    message.contains("No other Lisp threads can be running")
                }),
                "unexpected error payload: {:?}",
                sig.data
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }
}

#[test]
fn raw_source_bootstrap_starts_without_extra_function_cells() {
    crate::test_utils::init_test_tracing();
    let eval = Context::new();
    assert!(
        eval.obarray
            .symbol_function_id(intern("eval-and-compile"))
            .is_none()
    );

    for name in [
        "defvar-local",
        "track-mouse",
        "with-current-buffer",
        "with-temp-buffer",
        "with-output-to-string",
        "with-syntax-table",
        "with-mutex",
        "substitute-command-keys",
        "wholenump",
    ] {
        assert!(
            eval.obarray.symbol_function_id(intern(name)).is_none(),
            "{name} should come from GNU Lisp, not Rust source bootstrap"
        );
    }
}

#[test]
fn raw_source_debug_early_and_byte_run_define_eval_and_compile_without_shim() {
    crate::test_utils::init_test_tracing();
    let lisp_root = bootstrap_lisp_root();
    let temp = tempfile::tempdir().expect("tempdir for source-only bootstrap fixtures");
    let temp_root = temp.path().join("lisp");
    let debug_early = copy_source_fixture(&temp_root, "emacs-lisp/debug-early.el");
    let byte_run = copy_source_fixture(&temp_root, "emacs-lisp/byte-run.el");

    let mut eval = Context::new();
    eval.set_variable(
        "load-path",
        Value::list(vec![
            Value::string(temp_root.to_string_lossy().to_string()),
            Value::string(temp_root.join("emacs-lisp").to_string_lossy().to_string()),
            Value::string(lisp_root.to_string_lossy().to_string()),
            Value::string(lisp_root.join("emacs-lisp").to_string_lossy().to_string()),
        ]),
    );
    eval.set_variable("dump-mode", Value::symbol("pbootstrap"));
    eval.set_variable("purify-flag", Value::NIL);
    eval.set_variable("max-lisp-eval-depth", Value::fixnum(1600));
    eval.set_variable(
        "macroexp--pending-eager-loads",
        Value::list(vec![Value::symbol("skip")]),
    );

    for path in [&debug_early, &byte_run] {
        load_file(&mut eval, path)
            .unwrap_or_else(|err| panic!("failed loading {}: {:?}", path.display(), err));
    }

    let eval_and_compile = eval
        .obarray
        .symbol_function_id(intern("eval-and-compile"))
        .expect("GNU byte-run should define eval-and-compile");
    assert!(definition_is_macroish(eval_and_compile));
}

#[test]
fn raw_context_does_not_seed_gnu_string_helper_cells() {
    crate::test_utils::init_test_tracing();
    let eval = Context::new();

    for name in [
        "string-blank-p",
        "string-empty-p",
        "string-fill",
        "string-limit",
        "string-pad",
        "string-chop-newline",
    ] {
        assert!(
            eval.obarray.symbol_function_id(intern(name)).is_none(),
            "{name} should come from GNU Lisp bootstrap files, not Context::new"
        );
    }
}

#[test]
fn gnu_bootstrap_files_define_string_helpers_without_rust_shims() {
    crate::test_utils::init_test_tracing();
    let eval = crate::test_utils::eval_with_ldefs_boot_autoloads(&[
        "string-fill",
        "string-limit",
        "string-pad",
        "string-chop-newline",
    ]);

    for name in [
        "string-fill",
        "string-limit",
        "string-pad",
        "string-chop-newline",
    ] {
        let function = eval
            .obarray
            .symbol_function(name)
            .unwrap_or_else(|| panic!("{name} should be installed by GNU ldefs-boot"));
        assert!(
            crate::emacs_core::autoload::is_autoload_value(&function),
            "{name} should come from GNU autoloads"
        );
    }
}

#[test]
fn gnu_subr_x_string_chop_newline_loads_without_rust_builtin() {
    crate::test_utils::init_test_tracing();
    // Split responsibilities cleanly:
    // - `gnu_bootstrap_files_define_string_helpers_without_rust_shims` proves
    //   GNU `ldefs-boot.el` owns the autoload cell.
    // - this test proves the real implementation comes from loaded GNU
    //   `subr-x.el`, not from a Rust builtin.
    let mut eval = Context::new();
    crate::test_utils::load_minimal_gnu_help_runtime(&mut eval);
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let lisp_dir = project_root.join("lisp");
    eval.set_variable(
        "load-path",
        Value::list(bootstrap_load_path_entries(&lisp_dir)),
    );
    let load_path = get_load_path(&eval.obarray());
    let bindings_path =
        bootstrap_fixture_path(&load_path, "bindings", true).expect("bindings fixture path");
    load_file(&mut eval, &bindings_path).unwrap_or_else(|err| {
        panic!(
            "failed loading bindings from {}: {}",
            bindings_path.display(),
            format_eval_error(&eval, &err)
        )
    });
    eval.require_value(Value::symbol("gv"), None, None)
        .expect("require gv before GNU cl-lib/subr-x");
    eval.require_value(Value::symbol("cl-lib"), None, None)
        .expect("preload GNU cl-lib before GNU subr-x");
    eval.obarray_mut().fmakunbound("string-chop-newline");
    let subr_x_path =
        bootstrap_fixture_path(&load_path, "emacs-lisp/subr-x", false).expect("subr-x path");
    let subr_x_source =
        fs::read_to_string(&subr_x_path).unwrap_or_else(|err| panic!("read subr-x.el: {err}"));
    let subr_x_forms =
        crate::emacs_core::value_reader::read_all(&subr_x_source).expect("parse subr-x.el");
    let roots = eval.save_specpdl_roots();
    for form in &subr_x_forms {
        eval.push_specpdl_root(*form);
    }
    let mut found_string_chop_newline = false;
    for (index, form) in subr_x_forms.iter().enumerate() {
        eval.eval_form(*form).unwrap_or_else(|err| {
            panic!(
                "eval subr-x prefix from {} form #{index}: {}",
                subr_x_path.display(),
                format_eval_error(&eval, &err)
            )
        });
        if is_named_defun(*form, "string-chop-newline") {
            found_string_chop_newline = true;
            break;
        }
    }
    eval.restore_specpdl_roots(roots);
    assert!(
        found_string_chop_newline,
        "subr-x.el should define string-chop-newline before later helpers"
    );

    let rendered = eval_rendered(
        &mut eval,
        r#"
(list (string-chop-newline "x")
      (string-chop-newline "x\n")
      (string-chop-newline "x\r\n")
      (condition-case err (string-chop-newline 1) (error (car err))))
"#,
    );
    assert_eq!(rendered, "OK (\"x\" \"x\" \"x\r\" wrong-type-argument)");
}

#[test]
fn load_bindings_source_survives_gc_stress_after_custom_runtime() {
    crate::test_utils::init_test_tracing();

    let mut eval = Context::new();
    crate::test_utils::load_minimal_gnu_help_runtime(&mut eval);

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let lisp_dir = project_root.join("lisp");
    eval.set_variable(
        "load-path",
        Value::list(bootstrap_load_path_entries(&lisp_dir)),
    );
    eval.gc_stress = true;
    eval.tagged_heap.set_gc_threshold(1);

    let load_path = get_load_path(&eval.obarray());
    let bindings_path =
        bootstrap_fixture_path(&load_path, "bindings", false).expect("bindings.el fixture path");
    load_file(&mut eval, &bindings_path).unwrap_or_else(|err| {
        panic!(
            "failed loading source bindings from {}: {}",
            bindings_path.display(),
            format_eval_error(&eval, &err)
        )
    });

    let probe = eval
        .eval_str(
            r#"(list nil
                     (symbol-function 'bindings--define-key)
                     (get 'bindings--define-key 'byte-obsolete-info)
                     (boundp 'mode-line-right-align-edge)
                     (get 'mode-line-right-align-edge 'standard-value)
                     (get 'mode-line-format 'standard-value)
                     (default-toplevel-value 'mode-line-format))"#,
        )
        .expect("probe bindings custom state");
    let values = list_to_vec(&probe).expect("bindings probe should return list");
    assert_eq!(values[0], Value::NIL);
    assert!(!values[1].is_nil(), "bindings--define-key function cell");
    assert!(!values[2].is_nil(), "bindings--define-key obsolete plist");
    assert_eq!(values[3], Value::T);
    assert!(
        !values[4].is_nil(),
        "mode-line-right-align-edge standard-value"
    );
    assert!(!values[5].is_nil(), "mode-line-format standard-value");
    assert!(!values[6].is_nil(), "mode-line-format default value");
}

#[test]
fn obsolete_function_alias_metadata_survives_gc_stress_after_help_runtime() {
    crate::test_utils::init_test_tracing();

    let mut eval = Context::new();
    crate::test_utils::load_minimal_gnu_help_runtime(&mut eval);
    eval.gc_stress = true;
    eval.tagged_heap.set_gc_threshold(1);

    let result = eval
        .eval_str(
            r#"(progn
                 (defalias 'vm-obsolete-old #'ignore "Old doc.")
                 (make-obsolete 'vm-obsolete-old 'ignore "31.1")
                 (list (symbol-function 'vm-obsolete-old)
                       (get 'vm-obsolete-old 'byte-obsolete-info)
                       (get 'vm-obsolete-old 'function-documentation)))"#,
        )
        .expect("obsolete alias form should survive gc stress");
    let values = list_to_vec(&result).expect("obsolete alias probe should return list");
    assert_eq!(values[0], Value::symbol("ignore"));
    assert_eq!(values[2], Value::string("Old doc."));
    let obsolete_items = list_to_vec(&values[1]).expect("byte-obsolete-info should be a list");
    assert_eq!(
        obsolete_items,
        vec![Value::symbol("ignore"), Value::NIL, Value::string("31.1"),]
    );
}

#[test]
fn bindings_split_source_survives_gc_stress_after_help_runtime() {
    crate::test_utils::init_test_tracing();

    let mut eval = Context::new();
    crate::test_utils::load_minimal_gnu_help_runtime(&mut eval);
    eval.gc_stress = true;
    eval.tagged_heap.set_gc_threshold(1);

    let path = source_bootstrap_path("bindings.el");
    let content = std::fs::read_to_string(&path).expect("read bindings.el");
    let forms = crate::emacs_core::value_reader::read_all(&content).expect("parse bindings.el");

    let split_at = forms.len().saturating_sub(16);
    let prefix_source = format!(
        ";;; bindings-prefix-subset.el --- focused bootstrap slice -*- lexical-binding: t; -*-\n\n{}\n",
        forms[..split_at]
            .iter()
            .map(crate::emacs_core::print::print_value)
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    let tail_source = format!(
        ";;; bindings-tail-subset.el --- focused bootstrap slice -*- lexical-binding: t; -*-\n\n{}\n",
        forms[split_at..]
            .iter()
            .map(crate::emacs_core::print::print_value)
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let prefix_path = dir.path().join("bindings-prefix-subset.el");
    std::fs::write(&prefix_path, prefix_source).expect("write bindings prefix subset");
    let tail_path = dir.path().join("bindings-tail-subset.el");
    std::fs::write(&tail_path, tail_source).expect("write bindings tail subset");

    load_file(&mut eval, &prefix_path).unwrap_or_else(|err| {
        panic!(
            "failed loading focused bindings prefix from {}: {}",
            prefix_path.display(),
            format_eval_error(&eval, &err)
        )
    });
    load_file(&mut eval, &tail_path).unwrap_or_else(|err| {
        panic!(
            "failed loading focused bindings tail from {}: {}",
            tail_path.display(),
            format_eval_error(&eval, &err)
        )
    });
}

#[test]
fn gnu_subr_el_defines_wholenump_without_rust_shim() {
    crate::test_utils::init_test_tracing();
    let eval = partial_bootstrap_eval_until("keymap", true);
    assert_eq!(
        eval.obarray.symbol_function("wholenump"),
        Some(Value::symbol("natnump"))
    );
}

#[test]
fn load_subr_survives_exact_post_form_gc_after_byte_run() {
    crate::test_utils::init_test_tracing();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let lisp_dir = project_root.join("lisp");
    let mut eval = Context::new();

    let mut load_path_entries = Vec::new();
    for subdir in ["", "emacs-lisp"] {
        let dir = if subdir.is_empty() {
            lisp_dir.clone()
        } else {
            lisp_dir.join(subdir)
        };
        if dir.is_dir() {
            load_path_entries.push(Value::string(dir.to_string_lossy().to_string()));
        }
    }
    eval.set_variable("load-path", Value::list(load_path_entries));
    eval.set_variable("dump-mode", Value::symbol("pbootstrap"));
    eval.set_variable("purify-flag", Value::NIL);
    eval.set_variable("max-lisp-eval-depth", Value::fixnum(1600));

    let load_path = get_load_path(&eval.obarray());
    for name in &[
        "emacs-lisp/debug-early",
        "emacs-lisp/byte-run",
        "emacs-lisp/backquote",
        "subr",
    ] {
        let path = find_file_in_load_path(name, &load_path)
            .unwrap_or_else(|| panic!("cannot find {name} in load-path"));
        load_file(&mut eval, &path).unwrap_or_else(|err| panic!("failed to load {name}: {err:?}"));
    }

    let zerop = eval
        .eval_str("(list (zerop 0) (zerop 1))")
        .expect("zerop after subr load");
    assert_eq!(list_to_vec(&zerop), Some(vec![Value::T, Value::NIL]));
}

#[test]
fn raw_context_does_not_prebind_frame_creation_function() {
    crate::test_utils::init_test_tracing();
    let eval = Context::new();
    assert!(
        eval.obarray
            .symbol_value("frame-creation-function")
            .is_none(),
        "frame-creation-function should come from GNU frame.el/cl-generic bootstrap, not Context::new"
    );
}

#[test]
fn gnu_help_el_defines_substitute_command_keys_without_rust_shim() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    load_minimal_gnu_help_runtime(&mut eval);
    let function = eval
        .obarray
        .symbol_function("substitute-command-keys")
        .expect("help.el should define substitute-command-keys");
    assert!(!crate::emacs_core::autoload::is_autoload_value(&function));
}

#[test]
fn raw_context_does_not_seed_window_size_alias_cells() {
    crate::test_utils::init_test_tracing();
    let eval = Context::new();
    for name in ["window-height", "window-width"] {
        assert!(
            eval.obarray.symbol_function_id(intern(name)).is_none(),
            "{name} should come from GNU window.el, not Context::new"
        );
    }
}

#[test]
fn gnu_window_el_defines_window_size_aliases() {
    crate::test_utils::init_test_tracing();
    let eval = partial_bootstrap_eval_until("files", true);
    assert_eq!(
        eval.obarray.symbol_function("window-height"),
        Some(Value::symbol("window-total-height"))
    );
    assert_eq!(
        eval.obarray.symbol_function("window-width"),
        Some(Value::symbol("window-body-width"))
    );
}

#[test]
fn bootstrap_source_fingerprint_tracks_lisp_files_only() {
    let temp = tempdir().expect("temp runtime root");
    let runtime_root = temp.path();
    fs::create_dir_all(runtime_root.join("lisp")).expect("create lisp dir");
    fs::create_dir_all(runtime_root.join("etc")).expect("create etc dir");

    fs::write(runtime_root.join("lisp/loadup.el"), "(message \"one\")").expect("write loadup");
    fs::write(runtime_root.join("README.md"), "ignored").expect("write readme");

    let original = bootstrap_source_fingerprint(runtime_root);

    fs::write(runtime_root.join("README.md"), "still ignored").expect("rewrite readme");
    let after_non_lisp_change = bootstrap_source_fingerprint(runtime_root);
    assert_eq!(original, after_non_lisp_change);

    fs::write(runtime_root.join("lisp/loadup.el"), "(message \"two\")").expect("rewrite loadup");
    let after_lisp_change = bootstrap_source_fingerprint(runtime_root);
    assert_ne!(original, after_lisp_change);
}

#[test]
fn bootstrap_dump_path_changes_when_runtime_lisp_changes() {
    let temp = tempdir().expect("temp runtime root");
    let runtime_root = temp.path();
    fs::create_dir_all(runtime_root.join("lisp")).expect("create lisp dir");
    fs::create_dir_all(runtime_root.join("etc")).expect("create etc dir");
    fs::write(runtime_root.join("lisp/loadup.el"), "(message \"one\")").expect("write loadup");

    let first = bootstrap_dump_path(runtime_root, &["neomacs"]);

    fs::write(runtime_root.join("lisp/loadup.el"), "(message \"two\")").expect("rewrite loadup");
    let second = bootstrap_dump_path(runtime_root, &["neomacs"]);

    assert_ne!(first, second);
    assert!(
        first
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-neomacs.pdump"))
    );
}

#[test]
fn runtime_image_role_names_match_neomacs_pipeline() {
    assert_eq!(
        RuntimeImageRole::Bootstrap.image_file_name(),
        "bootstrap-neomacs.pdump"
    );
    assert_eq!(RuntimeImageRole::Final.image_file_name(), "neomacs.pdump");
}

#[test]
fn runtime_image_path_for_executable_uses_executable_basename() {
    let bootstrap = runtime_image_path_for_executable(
        PathBuf::from("/tmp/bootstrap-neomacs").as_path(),
        RuntimeImageRole::Bootstrap,
    );
    let final_image = runtime_image_path_for_executable(
        PathBuf::from("/tmp/renamed-neomacs").as_path(),
        RuntimeImageRole::Final,
    );

    assert_eq!(bootstrap, PathBuf::from("/tmp/bootstrap-neomacs.pdump"));
    assert_eq!(final_image, PathBuf::from("/tmp/renamed-neomacs.pdump"));
}

#[test]
fn fingerprinted_runtime_image_path_uses_canonical_role_name() {
    let final_image = fingerprinted_runtime_image_path_for_executable(
        PathBuf::from("/tmp/renamed-neomacs").as_path(),
        RuntimeImageRole::Final,
    );

    assert_eq!(
        final_image,
        PathBuf::from(format!(
            "/tmp/neomacs-{}.pdump",
            crate::emacs_core::pdump::fingerprint_hex()
        ))
    );
}

#[test]
fn missing_runtime_image_reports_heapless_startup_error() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let missing =
        std::env::temp_dir().join(format!("neomacs-missing-runtime-image-{unique}.pdump"));

    let err = match load_runtime_image_with_features(
        RuntimeImageRole::Bootstrap,
        &[],
        Some(missing.as_path()),
    ) {
        Ok(_) => panic!("missing image should report startup error"),
        Err(err) => err,
    };

    match err {
        EvalError::Signal {
            symbol,
            data,
            raw_data,
        } => {
            assert_eq!(resolve_sym(symbol), "error");
            assert_eq!(
                data.len(),
                1,
                "startup image load should report one payload value"
            );
            let raw = raw_data.expect("startup image load should preserve raw payload");
            assert_eq!(
                raw, data[0],
                "raw startup payload should match normalized data"
            );
            let payload = raw
                .as_symbol_name()
                .expect("startup image load payload should stay heapless");
            assert!(
                payload.contains("failed to load bootstrap image"),
                "unexpected startup payload: {payload}"
            );
            assert!(
                payload.contains(missing.to_string_lossy().as_ref()),
                "startup payload should include dump path: {payload}"
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn runtime_image_loader_falls_back_to_fingerprinted_candidate_when_primary_is_missing() {
    crate::test_utils::init_test_tracing();
    let dir = tempdir().expect("runtime image tempdir");
    let executable = dir.path().join("renamed-neomacs");
    let dump_path =
        fingerprinted_runtime_image_path_for_executable(&executable, RuntimeImageRole::Final);

    let mut eval = Context::new();
    eval.set_variable("runtime-image-candidate-test-var", Value::fixnum(42));
    crate::emacs_core::pdump::dump_to_file(&eval, &dump_path)
        .expect("write fingerprinted runtime image");

    let loaded = load_runtime_image_with_features_for_executable(
        RuntimeImageRole::Final,
        &[],
        None,
        &executable,
    )
    .expect("fingerprinted fallback should load");
    assert_eq!(
        loaded
            .obarray()
            .symbol_value("runtime-image-candidate-test-var"),
        Some(&Value::fixnum(42))
    );
}

#[test]
fn runtime_image_loader_stops_on_primary_fingerprint_mismatch() {
    crate::test_utils::init_test_tracing();
    let dir = tempdir().expect("runtime image tempdir");
    let executable = dir.path().join("renamed-neomacs");
    let primary = runtime_image_path_for_executable(&executable, RuntimeImageRole::Final);
    let fallback =
        fingerprinted_runtime_image_path_for_executable(&executable, RuntimeImageRole::Final);

    let mut stale = Context::new();
    stale.set_variable("runtime-image-candidate-test-var", Value::fixnum(1));
    crate::emacs_core::pdump::dump_to_file(&stale, &primary).expect("write primary runtime image");
    let mut primary_bytes = fs::read(&primary).expect("read primary runtime image");
    primary_bytes[12] ^= 0x01;
    fs::write(&primary, primary_bytes).expect("corrupt primary fingerprint");

    let mut fresh = Context::new();
    fresh.set_variable("runtime-image-candidate-test-var", Value::fixnum(2));
    crate::emacs_core::pdump::dump_to_file(&fresh, &fallback)
        .expect("write fallback runtime image");

    let err = match load_runtime_image_with_features_for_executable(
        RuntimeImageRole::Final,
        &[],
        None,
        &executable,
    ) {
        Ok(_) => panic!("fingerprint mismatch should not fall through"),
        Err(err) => err,
    };

    match err {
        EvalError::Signal {
            symbol,
            raw_data: Some(payload),
            ..
        } => {
            assert_eq!(resolve_sym(symbol), "error");
            let rendered = payload
                .as_symbol_name()
                .expect("heapless startup error payload");
            assert!(
                rendered.contains(primary.to_string_lossy().as_ref()),
                "startup error should reference the primary candidate: {rendered}"
            );
            assert!(
                rendered.contains("fingerprint mismatch"),
                "startup error should expose the real mismatch: {rendered}"
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn after_pdump_load_hook_runs_after_finalize_and_only_once() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let setup = crate::emacs_core::value_reader::read_all(
        "(progn
           (setq compat-pdump-hook-fired nil)
           (setq compat-pdump-hook-saw-load-path nil)
           (setq after-pdump-load-hook
                 (list
                  (lambda ()
                    (setq compat-pdump-hook-fired t)
                    (setq compat-pdump-hook-saw-load-path
                          (consp load-path))))))",
    )
    .unwrap();
    eval.eval_sub(setup[0]).expect("setup hook should evaluate");

    let dir = tempdir().expect("pdump hook tempdir");
    let dump_path = dir.path().join("after-pdump-load-hook-ordering.pdump");
    crate::emacs_core::pdump::dump_to_file(&eval, &dump_path).expect("dump should succeed");
    drop(eval);

    let mut loaded = crate::emacs_core::pdump::load_from_dump(&dump_path).expect("load dump");
    assert_eq!(
        loaded.obarray().symbol_value("compat-pdump-hook-fired"),
        Some(&Value::NIL)
    );

    finalize_cached_bootstrap_eval(&mut loaded, &runtime_project_root())
        .expect("finalize cached bootstrap eval");
    assert!(
        maybe_run_after_pdump_load_hook(&mut loaded),
        "startup helper should consume the pending pdump hook"
    );
    assert_eq!(
        loaded.obarray().symbol_value("compat-pdump-hook-fired"),
        Some(&Value::T)
    );
    assert_eq!(
        loaded
            .obarray()
            .symbol_value("compat-pdump-hook-saw-load-path"),
        Some(&Value::T)
    );
    assert!(
        !maybe_run_after_pdump_load_hook(&mut loaded),
        "after-pdump-load-hook should be a one-shot startup hook"
    );
}

#[test]
fn load_file_stops_immediately_on_kill_emacs() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("kill-emacs-stop.el");
    fs::write(
        &file,
        "(setq load-kill-before t)\n(kill-emacs 7)\n(setq load-kill-after t)\n",
    )
    .expect("write kill-emacs fixture");

    let err = match load_file(&mut eval, &file) {
        Ok(value) => panic!("kill-emacs load should not return {value:?}"),
        Err(err) => err,
    };

    match err {
        EvalError::Signal { symbol, data, .. } => {
            assert_eq!(resolve_sym(symbol), "kill-emacs");
            assert!(
                data.is_empty(),
                "kill-emacs should not synthesize an ordinary error payload"
            );
        }
        other => panic!("unexpected load error: {other:?}"),
    }

    assert_eq!(
        eval.shutdown_request(),
        Some(crate::emacs_core::eval::ShutdownRequest {
            exit_code: 7,
            restart: false,
        })
    );
    assert_eq!(
        eval.obarray().symbol_value("load-kill-before"),
        Some(&Value::T)
    );
    assert_eq!(eval.obarray().symbol_value("load-kill-after"), None);
}

#[test]
fn context_seeds_pdumper_fingerprint() {
    let eval = Context::new();
    assert_eq!(
        eval.obarray().symbol_value("pdumper-fingerprint"),
        Some(&Value::string(crate::emacs_core::pdump::fingerprint_hex()))
    );
}

#[test]
fn loadup_startup_surface_seeds_pre_startup_command_line_state() {
    let mut eval = Context::new();
    apply_loadup_startup_surface(
        &mut eval,
        &LoadupStartupSurface {
            command_line_args: vec![
                "neomacs-temacs".to_string(),
                "-l".to_string(),
                "loadup".to_string(),
                "--temacs=pdump".to_string(),
            ],
            noninteractive: true,
        },
    );

    assert_eq!(
        list_to_vec(
            eval.obarray()
                .symbol_value("command-line-args")
                .expect("command-line-args seeded")
        )
        .expect("command-line-args list"),
        vec![
            Value::string("neomacs-temacs"),
            Value::string("-l"),
            Value::string("loadup"),
            Value::string("--temacs=pdump"),
        ]
    );
    assert_eq!(
        eval.obarray().symbol_value("command-line-args-left"),
        Some(&Value::NIL)
    );
    assert_eq!(
        eval.obarray().symbol_value("command-line-processed"),
        Some(&Value::NIL)
    );
    assert_eq!(
        eval.obarray().symbol_value("noninteractive"),
        Some(&Value::T)
    );
}

#[test]
fn runtime_startup_state_clears_top_level_eval_state() {
    crate::test_utils::init_test_tracing();
    let mut eval =
        create_bootstrap_evaluator_cached_with_features(&["neomacs"]).expect("bootstrap evaluator");
    apply_runtime_startup_state(&mut eval).unwrap_or_else(|err| {
        panic!("runtime startup state: {}", format_eval_error(&eval, &err));
    });
    assert!(
        eval.top_level_eval_state_is_clean(),
        "runtime startup state should end at a clean top-level evaluator surface"
    );
}

/// Legacy bootstrap load sequence, retained for partial-bootstrap test utilities.
/// The production code now loads loadup.el directly instead.
const BOOTSTRAP_LOAD_SEQUENCE: &[&str] = &[
    "emacs-lisp/debug-early",
    "emacs-lisp/byte-run",
    "emacs-lisp/backquote",
    "subr",
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
    "emacs-lisp/macroexp",
    "emacs-lisp/pcase",
    "!require-gv",
    "!enable-eager-expansion",
    "emacs-lisp/macroexp",
    "emacs-lisp/inline",
    "cus-face",
    "faces",
    "!bootstrap-cl-preloaded-stubs",
    "!reload-subr-after-gv",
    "!load-ldefs-boot",
    "button",
    "emacs-lisp/cl-preloaded",
    "emacs-lisp/oclosure",
    "obarray",
    "abbrev",
    "help",
    "jka-cmpr-hook",
    "epa-hook",
    "international/mule-cmds",
    "case-table",
    "international/characters",
    "composite",
    "language/chinese",
    "language/cyrillic",
    "language/indian",
    "language/sinhala",
    "language/english",
    "language/ethiopic",
    "language/european",
    "language/czech",
    "language/slovak",
    "language/romanian",
    "language/greek",
    "language/hebrew",
    "international/cp51932",
    "international/eucjp-ms",
    "language/japanese",
    "language/korean",
    "language/lao",
    "language/tai-viet",
    "language/thai",
    "language/tibetan",
    "language/vietnamese",
    "language/misc-lang",
    "language/utf-8-lang",
    "language/georgian",
    "language/khmer",
    "language/burmese",
    "language/cham",
    "language/philippine",
    "language/indonesian",
    "indent",
    "emacs-lisp/cl-generic",
    "simple",
    "emacs-lisp/seq",
    "emacs-lisp/nadvice",
    "minibuffer",
    "frame",
    "startup",
    "term/tty-colors",
    "font-core",
    "emacs-lisp/syntax",
    "font-lock",
    "jit-lock",
    "mouse",
    "select",
    "emacs-lisp/timer",
    "emacs-lisp/easymenu",
    "isearch",
    "rfn-eshadow",
    "menu-bar",
    "tab-bar",
    "emacs-lisp/lisp",
    "textmodes/page",
    "register",
    "textmodes/paragraphs",
    "progmodes/prog-mode",
    "emacs-lisp/rx",
    "emacs-lisp/lisp-mode",
    "textmodes/text-mode",
    "textmodes/fill",
    "newcomment",
    "replace",
    "emacs-lisp/tabulated-list",
    "buff-menu",
    "fringe",
    "emacs-lisp/regexp-opt",
    "image",
    "international/fontset",
    "dnd",
    "tool-bar",
    "touch-screen",
    "x-dnd",
    "!load-x-win",
    "progmodes/elisp-mode",
    "emacs-lisp/float-sup",
    "vc/vc-hooks",
    "vc/ediff-hook",
    "uniquify",
    "electric",
    "paren",
    "emacs-lisp/shorthands",
    "emacs-lisp/eldoc",
    "emacs-lisp/cconv",
    "tooltip",
    "international/iso-transl",
    "emacs-lisp/rmc",
];

fn init_test_tracing() {
    crate::test_utils::init_test_tracing();
}

fn load_path_runtime_strings(load_path: &[crate::heap_types::LispString]) -> Vec<String> {
    load_path
        .iter()
        .map(crate::emacs_core::builtins::runtime_string_from_lisp_string)
        .collect()
}

fn runtime_path_entry(path: &str) -> crate::heap_types::LispString {
    crate::emacs_core::builtins::runtime_string_to_lisp_string(path, !path.is_ascii())
}

#[cfg(unix)]
fn raw_path_entry(path: Vec<u8>) -> crate::heap_types::LispString {
    crate::heap_types::LispString::from_unibyte(path)
}

fn bootstrap_fixture_path(
    load_path: &[crate::heap_types::LispString],
    name: &str,
    prefer_compiled: bool,
) -> Option<PathBuf> {
    for dir in load_path {
        let base = PathBuf::from(crate::emacs_core::builtins::runtime_string_from_lisp_string(dir))
            .join(name);
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

fn format_eval_error(eval: &Context, err: &EvalError) -> String {
    match err {
        EvalError::Signal { symbol, data, .. } => {
            let mut items = Vec::with_capacity(data.len() + 1);
            items.push(Value::symbol(resolve_sym(*symbol)));
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

fn partial_bootstrap_eval_until(stop_before: &str, prefer_compiled: bool) -> Context {
    crate::test_utils::init_test_tracing();

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let lisp_dir = project_root.join("lisp");
    assert!(
        lisp_dir.is_dir(),
        "lisp/ directory not found at {}",
        lisp_dir.display()
    );

    let mut eval = Context::new();
    eval.set_variable(
        "load-path",
        Value::list(bootstrap_load_path_entries(&lisp_dir)),
    );
    eval.set_variable("dump-mode", Value::symbol("pbootstrap"));
    eval.set_variable("purify-flag", Value::NIL);
    eval.set_variable("max-lisp-eval-depth", Value::fixnum(1600));
    eval.set_variable("inhibit-load-charset-map", Value::T);

    let etc_dir = project_root.join("etc");
    eval.set_variable(
        "data-directory",
        Value::unibyte_string(format!("{}/", etc_dir.to_string_lossy())),
    );
    eval.set_variable(
        "source-directory",
        Value::unibyte_string(format!("{}/", project_root.to_string_lossy())),
    );
    eval.set_variable(
        "installation-directory",
        Value::unibyte_string(format!("{}/", project_root.to_string_lossy())),
    );

    let path_dirs: Vec<Value> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(|s| Value::unibyte_string(s.to_string()))
        .collect();
    eval.set_variable("exec-path", Value::list(path_dirs));
    eval.set_variable("exec-suffixes", Value::NIL);
    eval.set_variable("exec-directory", Value::NIL);
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
        let _ = eval.eval_str_each(&stub);
    }

    let load_path = get_load_path(&eval.obarray());
    for name in BOOTSTRAP_LOAD_SEQUENCE {
        if *name == stop_before {
            break;
        }
        if *name == "!enable-eager-expansion" {
            eval.set_variable("macroexp--pending-eager-loads", Value::NIL);
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

fn build_pre_macroexp_reload_eval() -> Context {
    crate::test_utils::init_test_tracing();

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let lisp_dir = project_root.join("lisp");
    assert!(
        lisp_dir.is_dir(),
        "lisp/ directory not found at {}",
        lisp_dir.display()
    );

    let mut eval = Context::new();
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
    eval.set_variable("purify-flag", Value::NIL);
    eval.set_variable("max-lisp-eval-depth", Value::fixnum(1600));
    eval.set_variable(
        "macroexp--pending-eager-loads",
        Value::list(vec![Value::symbol("skip")]),
    );

    let load_path = get_load_path(&eval.obarray());
    for name in &[
        "emacs-lisp/debug-early",
        "emacs-lisp/byte-run",
        "emacs-lisp/backquote",
        "subr",
        "emacs-lisp/macroexp",
        "emacs-lisp/pcase",
    ] {
        let path = bootstrap_fixture_path(&load_path, name, false)
            .unwrap_or_else(|| panic!("bootstrap file not found: {name}"));
        load_file(&mut eval, &path).unwrap_or_else(|err| {
            panic!(
                "failed loading {name} from {}: {}",
                path.display(),
                format_eval_error(&eval, &err)
            )
        });
    }

    eval.require_value(Value::symbol("gv"), None, None)
        .expect("require gv for eager macroexpansion");
    eval.set_variable("macroexp--pending-eager-loads", Value::NIL);

    assert!(
        get_eager_macroexpand_fn(&eval).is_some(),
        "pre-reload eager bootstrap should expose internal-macroexpand-for-load"
    );

    eval
}

fn minimal_eager_macroexpand_eval() -> Context {
    let mut eval = build_pre_macroexp_reload_eval();
    let load_path = get_load_path(&eval.obarray());
    let macroexp_path = bootstrap_fixture_path(&load_path, "emacs-lisp/macroexp", false)
        .expect("macroexp source fixture path");
    load_file(&mut eval, &macroexp_path).unwrap_or_else(|err| {
        panic!(
            "failed reloading emacs-lisp/macroexp from {}: {}",
            macroexp_path.display(),
            format_eval_error(&eval, &err)
        )
    });

    assert!(
        get_eager_macroexpand_fn(&eval).is_some(),
        "minimal eager bootstrap should expose internal-macroexpand-for-load"
    );

    eval
}

#[test]
fn bootstrap_lambda_parameters_bind_special_symbols_like_gnu_emacs() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).unwrap_or_else(|err| {
        panic!("startup state: {}", format_eval_error(&eval, &err));
    });
    let result = eval.eval_str(
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
    );
    assert_eq!(
        format_eval_result(&result),
        "OK (7 9 t 7 (1 2 3) (4 5 6) (7 42) 10)",
        "bootstrap evaluator should match GNU's special-symbol parameter binding"
    );
}

#[test]
fn bootstrap_lambda_parameter_named_pi_shadows_obsolete_global_constant() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    // GNU loadup.el explicitly requires gv for the interpreted add-hook path,
    // but `cl-lib` itself is still not a loaded runtime feature at `-Q`
    // startup. The loaddefs entry points remain visible as autoloads.
    assert_eq!(
        rendered,
        "OK (nil nil nil nil nil t t nil nil nil nil nil nil nil nil nil nil t t t t t nil t)",
        "bootstrap runtime should match GNU -Q startup visibility for cl preload and loaddefs"
    );
}

#[test]
fn bootstrap_runtime_matches_gnu_oclosure_advice_surface() {
    crate::test_utils::init_test_tracing();
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
    // GNU -Q runtime exposes nadvice advice prototypes as byte-code oclosures.
    assert_eq!(
        rendered, "OK (t nil t nil t t t t t byte-code-function t)",
        "bootstrap runtime should match GNU -Q oclosure/nadvice surface"
    );
}

const BOOTSTRAP_CACHE_RACE_DUMP_ENV: &str = "NEOVM_BOOTSTRAP_RACE_DUMP_PATH";
const BOOTSTRAP_CACHE_RACE_WORKER_TEST: &str =
    "emacs_core::load::tests::bootstrap_cache_parallel_creation_worker";
const BOOTSTRAP_CACHE_LOCK_HOLDER_ENV: &str = "NEOVM_BOOTSTRAP_CACHE_LOCK_HOLDER";
const BOOTSTRAP_CACHE_LOCK_READY_ENV: &str = "NEOVM_BOOTSTRAP_CACHE_LOCK_READY";
const BOOTSTRAP_CACHE_LOCK_HOLDER_TEST: &str =
    "emacs_core::load::tests::bootstrap_cache_lock_holder_worker";

#[test]
fn bootstrap_cache_lock_holder_worker() {
    crate::test_utils::init_test_tracing();
    let Some(lock_path) = std::env::var_os(BOOTSTRAP_CACHE_LOCK_HOLDER_ENV) else {
        return;
    };
    let ready_path = PathBuf::from(
        std::env::var_os(BOOTSTRAP_CACHE_LOCK_READY_ENV).expect("lock ready marker path"),
    );

    let _lock =
        BootstrapCacheWriteLock::acquire(&PathBuf::from(lock_path)).expect("acquire held lock");
    fs::write(&ready_path, b"ready").expect("write lock-ready marker");
    std::thread::sleep(std::time::Duration::from_secs(3));
}

#[test]
fn bootstrap_cache_write_lock_reports_busy_without_blocking() {
    crate::test_utils::init_test_tracing();
    let dir = tempdir().expect("tempdir");
    let lock_path = dir.path().join("bootstrap.lock");
    let ready_path = dir.path().join("bootstrap.lock.ready");
    let exe = std::env::current_exe().expect("current test binary");

    let mut holder = Command::new(&exe);
    holder
        .env(BOOTSTRAP_CACHE_LOCK_HOLDER_ENV, &lock_path)
        .env(BOOTSTRAP_CACHE_LOCK_READY_ENV, &ready_path)
        .arg("--exact")
        .arg(BOOTSTRAP_CACHE_LOCK_HOLDER_TEST)
        .arg("--nocapture");
    let mut child = holder.spawn().expect("spawn lock holder");

    let ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ready_path.exists() {
        if let Some(status) = child.try_wait().expect("poll lock holder") {
            panic!("lock holder exited before signaling readiness: {status}");
        }
        assert!(
            std::time::Instant::now() < ready_deadline,
            "timed out waiting for lock holder readiness marker at {}",
            ready_path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    let start = std::time::Instant::now();
    let err = match BootstrapCacheWriteLock::acquire(&lock_path) {
        Ok(_) => panic!("lock should be busy"),
        Err(err) => err,
    };
    let elapsed = start.elapsed();
    assert!(
        matches!(err, BootstrapCacheLockError::Busy(_)),
        "expected busy-lock error, got: {err}"
    );
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "busy lock acquisition should fail fast, took {elapsed:?}"
    );

    let status = child.wait().expect("wait for lock holder");
    assert!(status.success(), "lock holder failed: {status}");
}

#[test]
fn bootstrap_cache_parallel_creation_worker() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
fn bootstrap_cache_parallel_stale_repair_is_safe() {
    crate::test_utils::init_test_tracing();
    let dir = tempfile::tempdir().expect("tempdir");
    let dump_path = dir.path().join("parallel-bootstrap-stale.pdump");
    let exe = std::env::current_exe().expect("current test binary");

    crate::emacs_core::pdump::dump_to_file(&Context::new(), &dump_path)
        .expect("write seed bootstrap cache");
    let mut stale_bytes = fs::read(&dump_path).expect("read initial bootstrap cache");
    stale_bytes[12] ^= 0x01;
    fs::write(&dump_path, stale_bytes).expect("corrupt bootstrap cache fingerprint");

    let mut children = Vec::new();
    for _ in 0..2 {
        let mut cmd = Command::new(&exe);
        cmd.env(BOOTSTRAP_CACHE_RACE_DUMP_ENV, &dump_path)
            .arg("--exact")
            .arg(BOOTSTRAP_CACHE_RACE_WORKER_TEST)
            .arg("--nocapture");
        children.push(cmd.spawn().expect("spawn stale-cache bootstrap worker"));
    }

    for mut child in children {
        let status = child.wait().expect("wait for stale-cache bootstrap worker");
        assert!(
            status.success(),
            "stale-cache bootstrap worker failed: {status}"
        );
    }

    let mut loaded = create_bootstrap_evaluator_cached_at_path(&[], &dump_path)
        .expect("reload repaired dump after stale race");
    apply_runtime_startup_state(&mut loaded).expect("runtime startup after stale repair");
    let rendered = eval_rendered(
        &mut loaded,
        "(list (featurep 'cl-lib) (fboundp 'setf) (autoloadp (symbol-function 'setf)))",
    );
    assert_eq!(rendered, "OK (nil t t)");
}

#[test]
fn bootstrap_runtime_advice_copy_and_add_behavior() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    // GNU emacs 31.0.50 verified: advice-add, advice-remove, and
    // advice-member-p are loaded as compiled bytecode functions
    // from nadvice.el. Their wrong-arity errors carry the
    // (MIN . MAX) tuple from the bytecode arglist descriptor,
    // not the surface symbol name -- this is GNU funcall_lambda
    // (eval.c:3411) signaling with the closure value's arity.
    assert_eq!(
        rendered,
        "OK ((wrong-number-of-arguments (3 . 4) 2) (wrong-number-of-arguments (2 . 2) 1) (wrong-number-of-arguments (2 . 2) 1))"
    );
}

#[test]
fn bootstrap_runtime_matches_gnu_cl_loaddefs_default_q_surface() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
                       (autoloadp (symbol-function 'cl-every))
                       (not (null (get 'button 'icon--properties)))))
             (error (list 'error err)))"#,
    );
    assert_eq!(rendered, "OK (t t t t t)");
}

#[test]
fn runtime_source_bootstrap_surface_tracks_icons_owned_surface() {
    crate::test_utils::init_test_tracing();
    let project_root = runtime_project_root();
    let state =
        runtime_source_bootstrap_surface_state(&project_root).expect("runtime source surface");

    assert!(state.function_names.contains("define-icon"));
    assert!(state.function_names.contains("icon-string"));
    assert!(state.function_names.contains("describe-icon"));
    assert!(state.variable_names.contains("icon-preference"));
    assert!(state.variable_names.contains("icon"));
    assert!(state.variable_names.contains("icon-button"));
    assert!(state.face_names.contains("icon"));
    assert!(state.face_names.contains("icon-button"));
    assert!(
        state
            .property_keys
            .contains(&(String::from("button"), String::from("icon--properties")))
    );
    assert!(state.features.contains("icons"));
}

#[test]
fn bootstrap_runtime_gui_surface_matches_gnu_icons_residency() {
    crate::test_utils::init_test_tracing();
    let mut eval =
        create_bootstrap_evaluator_cached_with_features(&["x", "neomacs"]).expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(list (featurep 'icons)
                 (get 'button 'icon--properties)
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
    assert_eq!(rendered, "OK (nil nil nil nil nil nil nil t t t t nil)");
}

#[test]
fn bootstrap_runtime_display_selections_p_is_true_under_neomacs_gui_surface() {
    crate::test_utils::init_test_tracing();
    let mut eval =
        create_bootstrap_evaluator_cached_with_features(&["x", "neomacs"]).expect("bootstrap");
    let value = eval
        .eval_str("(display-selections-p)")
        .expect("display-selections-p");
    assert_eq!(value, Value::T);
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["x", "neomacs"])
        .expect("bootstrap evaluator");
    assert!(
        eval.frames.frame_list().is_empty(),
        "cached GUI bootstrap should not synthesize a fallback frame before host bootstrap"
    );
    let rendered = eval_rendered(
        &mut eval,
        r#"(list (window-system)
                 initial-window-system
                 (display-graphic-p)
                 (display-color-cells)
                 (display-visual-class))"#,
    );
    assert_eq!(rendered, "OK (neo neo t 16777216 true-color)");
    assert!(
        eval.frames.frame_list().is_empty(),
        "display queries should not synthesize a fallback frame before host bootstrap"
    );
}

#[test]
fn bootstrap_runtime_require_eieio_restores_cl_loaddefs_surface() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let rendered = eval_rendered(
        &mut eval,
        r#"(list
             (lookup-key (current-global-map) "\e")
             (lookup-key esc-map "x")
             (lookup-key (current-global-map) "\ev")
             (lookup-key (current-global-map) "\C-x")
             (lookup-key ctl-x-map "2")
             (lookup-key ctl-x-map "3")
             (lookup-key (current-global-map) "\e\e\e")
             (lookup-key (current-global-map) "\C-x\C-z"))"#,
    );
    assert_eq!(
        rendered,
        "OK (ESC-prefix execute-extended-command scroll-down-command Control-X-prefix split-window-below split-window-right keyboard-escape-quit suspend-emacs)"
    );
}

#[test]
fn pdump_roundtrip_preserves_gnu_prefix_keymap_links() {
    crate::test_utils::init_test_tracing();

    let mut eval = Context::new();
    eval.eval_str(
        r#"(progn
             (setq esc-map (make-sparse-keymap "ESC-prefix"))
             (define-key esc-map "x" 'execute-extended-command)
             (fset 'ESC-prefix esc-map)
             (setq ctl-x-map (make-sparse-keymap "Control-X-prefix"))
             (define-key ctl-x-map "2" 'split-window-below)
             (define-key ctl-x-map "3" 'split-window-right)
             (fset 'Control-X-prefix ctl-x-map)
             (setq global-map (make-sparse-keymap))
             (define-key global-map "\e" 'ESC-prefix)
             (define-key global-map "\C-x" 'Control-X-prefix)
             (define-key global-map "\e\e\e" 'keyboard-escape-quit)
             (define-key global-map "\C-x\C-z" 'suspend-emacs)
             (use-global-map global-map))"#,
    )
    .expect("evaluate prefix keymap setup");

    let dir = tempfile::tempdir().expect("tempdir");
    let dump_path = dir.path().join("prefix-keymaps.pdump");
    crate::emacs_core::pdump::dump_to_file(&eval, &dump_path).expect("dump should succeed");
    drop(eval);

    let mut loaded =
        crate::emacs_core::pdump::load_from_dump(&dump_path).expect("load should succeed");
    let rendered = eval_rendered(
        &mut loaded,
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
fn partial_bootstrap_subr_defines_gnu_prefix_maps_before_bindings() {
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("bindings", true);
    let rendered = eval_rendered(
        &mut eval,
        r#"(list
             (lookup-key (current-global-map) "\e")
             (lookup-key esc-map "x")
             (lookup-key (current-global-map) "\ev")
             (lookup-key (current-global-map) "\C-x")
             (lookup-key ctl-x-map "4")
             (lookup-key ctl-x-map "5")
             (lookup-key ctl-x-map "t"))"#,
    );
    assert_eq!(
        rendered,
        "OK (ESC-prefix execute-extended-command scroll-down-command Control-X-prefix ctl-x-4-prefix ctl-x-5-prefix (keymap))"
    );
}

#[test]
fn pdump_roundtrip_preserves_partial_bootstrap_subr_prefix_maps() {
    crate::test_utils::init_test_tracing();
    let eval = partial_bootstrap_eval_until("bindings", true);

    let dir = tempfile::tempdir().expect("tempdir");
    let dump_path = dir.path().join("partial-subr-prefixes.pdump");
    crate::emacs_core::pdump::dump_to_file(&eval, &dump_path).expect("dump should succeed");

    let mut loaded =
        crate::emacs_core::pdump::load_from_dump(&dump_path).expect("load should succeed");
    let rendered = eval_rendered(
        &mut loaded,
        r#"(list
             (lookup-key (current-global-map) "\e")
             (lookup-key esc-map "x")
             (lookup-key (current-global-map) "\C-x")
             (lookup-key ctl-x-map "4")
             (lookup-key ctl-x-map "5")
             (lookup-key ctl-x-map "t"))"#,
    );
    assert_eq!(
        rendered,
        "OK (ESC-prefix execute-extended-command Control-X-prefix ctl-x-4-prefix ctl-x-5-prefix (keymap))"
    );
}

#[test]
fn normalize_runtime_surface_preserves_partial_bootstrap_subr_prefix_maps() {
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("bindings", true);
    let project_root = runtime_project_root();
    normalize_bootstrap_runtime_surface(&mut eval, &project_root)
        .expect("normalize runtime surface");
    let rendered = eval_rendered(
        &mut eval,
        r#"(list
             (lookup-key (current-global-map) "\e")
             (lookup-key esc-map "x")
             (lookup-key (current-global-map) "\C-x")
             (lookup-key ctl-x-map "4")
             (lookup-key ctl-x-map "5")
             (lookup-key ctl-x-map "t"))"#,
    );
    assert_eq!(
        rendered,
        "OK (ESC-prefix execute-extended-command Control-X-prefix ctl-x-4-prefix ctl-x-5-prefix (keymap))"
    );
}

#[test]
fn normalize_runtime_surface_preserves_partial_bindings_global_prefix_links() {
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("files", true);
    let project_root = runtime_project_root();
    normalize_bootstrap_runtime_surface(&mut eval, &project_root)
        .expect("normalize runtime surface");
    let rendered = eval_rendered(
        &mut eval,
        r#"(list
             (lookup-key (current-global-map) "\e")
             (lookup-key esc-map "x")
             (lookup-key (current-global-map) "\C-x")
             (lookup-key ctl-x-map "4")
             (lookup-key ctl-x-map "5")
             (lookup-key ctl-x-map "t")
             (lookup-key (current-global-map) "\e\e\e")
             (lookup-key (current-global-map) "\C-x\C-z"))"#,
    );
    assert_eq!(
        rendered,
        "OK (ESC-prefix execute-extended-command Control-X-prefix ctl-x-4-prefix ctl-x-5-prefix (keymap (112 . project-other-tab-command) (100 . dired-other-tab)) keyboard-escape-quit suspend-emacs)"
    );
}

#[test]
fn partial_bootstrap_through_simple_preserves_gnu_prefix_maps() {
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("mouse", true);
    let rendered = eval_rendered(
        &mut eval,
        r#"(list
             (lookup-key (current-global-map) "\e")
             (lookup-key esc-map "x")
             (lookup-key (current-global-map) "\C-x")
             (lookup-key ctl-x-map "4")
             (lookup-key ctl-x-map "5")
             (lookup-key ctl-x-map "t")
             (lookup-key (current-global-map) "\e\e\e")
             (lookup-key (current-global-map) "\C-x\C-z"))"#,
    );
    assert_eq!(
        rendered,
        "OK (ESC-prefix execute-extended-command Control-X-prefix ctl-x-4-prefix ctl-x-5-prefix (keymap (112 . project-other-tab-command) (100 . dired-other-tab)) keyboard-escape-quit suspend-emacs)"
    );
}

#[test]
fn bootstrap_runtime_preserves_gnu_minibuffer_completion_bindings() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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

    eval.eval_str(
        r#"(progn
             (setq neo-ret-probe-ran nil)
             (defun neo-ret-probe ()
               (interactive)
               (setq neo-ret-probe-ran t)))"#,
    )
    .expect("eval execute-extended-command RET probe");

    let (tx, rx) = crossbeam_channel::unbounded();
    drop(tx);
    eval.input_rx = Some(rx);
    eval.command_loop.running = true;

    for ch in "neo-ret-probe".chars() {
        eval.command_loop
            .keyboard
            .kboard
            .unread_events
            .push_back(Value::fixnum(ch as i64));
    }
    eval.command_loop.keyboard.kboard.unread_events.push_back(
        crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Return).to_emacs_event_value(),
    );

    let result = eval
        .apply(Value::symbol("execute-extended-command"), vec![Value::NIL])
        .expect("execute-extended-command should return after RET");
    assert_eq!(result, Value::NIL);
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
    let scratch = eval.buffers.create_buffer("*m-x-target*");
    eval.buffers.set_current(scratch);
    let frame_id = eval.frames.create_frame("F1", 960, 640, scratch);
    assert!(
        eval.frames.select_frame(frame_id),
        "runtime command-loop test should have a selected frame"
    );

    let _ = eval.eval_str_each(
        r#"(progn
             (setq neo-ret-probe-ran nil)
             (defun neo-ret-probe ()
               (interactive)
               (setq neo-ret-probe-ran t)
               (exit-recursive-edit)))"#,
    );

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('x', crate::keyboard::Modifiers::meta()),
    ))
    .expect("queue M-x");
    for ch in "neo-ret-probe".chars() {
        tx.send(crate::keyboard::InputEvent::key_press(
            crate::keyboard::KeyEvent::char(ch),
        ))
        .expect("queue command chars");
    }
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Return),
    ))
    .expect("queue RET");
    drop(tx);

    eval.input_rx = Some(rx);
    eval.command_loop.running = true;

    let result = eval
        .recursive_edit_inner()
        .expect("command loop should exit normally");
    assert_eq!(result, Value::NIL);
    assert!(
        eval.eval_symbol("neo-ret-probe-ran")
            .expect("probe var should exist")
            .is_truthy(),
        "expected M-x command RET path to run the command before shutdown fallback"
    );
}

#[test]
fn bootstrap_runtime_command_loop_executes_help_describe_function_on_ret() {
    init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let scratch = eval.buffers.create_buffer("*help-f-target*");
    eval.buffers.set_current(scratch);
    let frame_id = eval.frames.create_frame("F1", 960, 640, scratch);
    assert!(
        eval.frames.select_frame(frame_id),
        "runtime command-loop help test should have a selected frame"
    );

    let _ = eval.eval_str_each(
        r#"(progn
             (setq neo-help-f-log nil)
             (defun neo--capture-describe-function (&rest _args)
               (setq neo-help-f-log
                     (list
                      (bufferp (get-buffer "*Help*"))
                      (with-current-buffer "*Help*"
                        (not (null (save-excursion
                                     (goto-char (point-min))
                                     (search-forward "find-file is" nil t)))))
                      (with-current-buffer "*Help*"
                        (not (null (save-excursion
                                     (goto-char (point-min))
                                     (search-forward "C-x C-f" nil t))))))
               (exit-recursive-edit))
             (advice-add 'describe-function :after #'neo--capture-describe-function))"#,
    );

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('h', crate::keyboard::Modifiers::ctrl()),
    ))
    .expect("queue C-h");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('f'),
    ))
    .expect("queue f");
    for ch in "find-file".chars() {
        tx.send(crate::keyboard::InputEvent::key_press(
            crate::keyboard::KeyEvent::char(ch),
        ))
        .expect("queue command chars");
    }
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Return),
    ))
    .expect("queue RET");
    drop(tx);

    eval.input_rx = Some(rx);
    eval.command_loop.running = true;

    let result = eval
        .recursive_edit_inner()
        .expect("C-h f command loop should exit normally");
    assert_eq!(result, Value::NIL);
    assert_eq!(
        format_eval_result(&eval.eval_str(
            r#"(prog1 neo-help-f-log
                 (advice-remove 'describe-function #'neo--capture-describe-function)
                 (fmakunbound 'neo--capture-describe-function)
                 (makunbound 'neo-help-f-log))"#
        )),
        "OK (t t t)",
        "expected C-h f keyboard path to populate *Help* like GNU"
    );
}

#[test]
fn bootstrap_runtime_read_key_after_two_minibuffers_consumes_fresh_key() {
    init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let scratch = eval.buffers.create_buffer("*read-key-after-minibuffers*");
    eval.buffers.set_current(scratch);
    let frame_id = eval.frames.create_frame("F1", 960, 640, scratch);
    assert!(
        eval.frames.select_frame(frame_id),
        "runtime command-loop read-key test should have a selected frame"
    );

    let _ = eval.eval_str_each(
        r#"(progn
             (setq neo-read-key-after-minibuffers-log nil)
             (defun neo-read-key-after-minibuffers ()
               (interactive)
               (let ((a (read-from-minibuffer "A: "))
                     (b (read-from-minibuffer "B: "))
                     (k (read-key "K: ")))
                 (setq neo-read-key-after-minibuffers-log
                       (list a b k unread-command-events))
                 (exit-recursive-edit))))"#,
    );

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('x', crate::keyboard::Modifiers::meta()),
    ))
    .expect("queue M-x");
    for ch in "neo-read-key-after-minibuffers".chars() {
        tx.send(crate::keyboard::InputEvent::key_press(
            crate::keyboard::KeyEvent::char(ch),
        ))
        .expect("queue command chars");
    }
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Return),
    ))
    .expect("queue RET to run command");
    for ch in "alpha".chars() {
        tx.send(crate::keyboard::InputEvent::key_press(
            crate::keyboard::KeyEvent::char(ch),
        ))
        .expect("queue first minibuffer chars");
    }
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Return),
    ))
    .expect("queue RET for first minibuffer");
    for ch in "beta".chars() {
        tx.send(crate::keyboard::InputEvent::key_press(
            crate::keyboard::KeyEvent::char(ch),
        ))
        .expect("queue second minibuffer chars");
    }
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Return),
    ))
    .expect("queue RET for second minibuffer");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('!'),
    ))
    .expect("queue fresh read-key input");
    drop(tx);

    eval.input_rx = Some(rx);
    eval.command_loop.running = true;

    let result = eval
        .recursive_edit_inner()
        .expect("read-key test command loop should exit normally");
    assert_eq!(result, Value::NIL);
    assert_eq!(
        eval_rendered(&mut eval, "neo-read-key-after-minibuffers-log"),
        r#"OK ("alpha" "beta" 33 nil)"#,
        "expected read-key to consume the fresh ! event after the minibuffer exits"
    );
}

#[test]
fn bootstrap_runtime_window_close_routes_through_handle_delete_frame() {
    init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let _ = eval.eval_str_each(
        r#"(progn
             (setq neo-delete-frame-log nil)
             (defun neo--log-delete-frame-advice (event)
               (setq neo-delete-frame-log
                     (list (car event)
                           (framep (car (cadr event))))))
             (advice-add 'handle-delete-frame :before
                         #'neo--log-delete-frame-advice))"#,
    );

    let scratch = eval.buffers.create_buffer("*close-frame-target*");
    eval.buffers.set_current(scratch);
    let frame_id = eval.frames.create_frame("F1", 960, 640, scratch);
    assert!(
        eval.frames.select_frame(frame_id),
        "new runtime frame should become selectable"
    );

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::WindowClose {
        emacs_frame_id: frame_id.0,
    })
    .expect("queue window close");
    drop(tx);

    eval.input_rx = Some(rx);
    eval.command_loop.running = true;

    let result = eval
        .recursive_edit_inner()
        .expect("window close should exit command loop normally");
    assert_eq!(result, Value::NIL);

    assert_eq!(
        format_eval_result(&eval.eval_str(
            r#"(prog1 neo-delete-frame-log
              (advice-remove 'handle-delete-frame
                             #'neo--log-delete-frame-advice)
              (fmakunbound 'neo--log-delete-frame-advice)
              (makunbound 'neo-delete-frame-log))"#
        )),
        "OK (delete-frame t)",
        "expected WM close to route through GNU handle-delete-frame"
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
fn bootstrap_runtime_file_directories_are_unibyte_and_vc_mode_matches_gnu() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"(progn
             (view-hello-file)
             (list
              (multibyte-string-p invocation-directory)
              (multibyte-string-p default-directory)
              (multibyte-string-p buffer-file-name)
              (multibyte-string-p (car exec-path))
              (and (memq 'vc-refresh-state find-file-hook) t)
              (vc-registered buffer-file-name)
              (vc-file-getprop buffer-file-name 'vc-backend)
              (and buffer-file-name (vc-backend buffer-file-name))
              (and vc-mode (string-prefix-p " Git-" vc-mode))))"#,
    );

    assert_eq!(rendered, "OK (nil nil nil nil t t Git Git t)");
}

#[test]
fn bootstrap_runtime_cd_accepts_existing_abbreviated_directory_like_gnu() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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

    eval.command_loop.keyboard.kboard.unread_events.push_back(
        crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Escape).to_emacs_event_value(),
    );
    eval.command_loop
        .keyboard
        .kboard
        .unread_events
        .push_back(Value::fixnum('x' as i64));

    let (keys, binding) = eval.read_key_sequence().expect("read ESC x sequence");
    assert_eq!(keys, vec![Value::fixnum(27), Value::fixnum('x' as i64)]);
    assert_eq!(binding, Value::symbol("execute-extended-command"));
}

#[test]
fn bootstrap_runtime_read_key_sequence_follows_meta_x_command() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    eval.command_loop.keyboard.kboard.unread_events.push_back(
        crate::keyboard::KeyEvent::char_with_mods('x', crate::keyboard::Modifiers::meta())
            .to_emacs_event_value(),
    );

    let (keys, binding) = eval.read_key_sequence().expect("read M-x sequence");
    assert_eq!(keys, vec![Value::fixnum(134_217_848)]);
    assert_eq!(binding, Value::symbol("execute-extended-command"));
}

#[test]
fn bootstrap_runtime_read_key_sequence_follows_help_command_keymap_prefix() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    eval.command_loop
        .keyboard
        .kboard
        .unread_events
        .push_back(Value::fixnum(8));
    eval.command_loop
        .keyboard
        .kboard
        .unread_events
        .push_back(Value::fixnum('m' as i64));

    let (keys, binding) = eval.read_key_sequence().expect("read C-h m sequence");
    assert_eq!(keys, vec![Value::fixnum(8), Value::fixnum('m' as i64)]);
    assert_eq!(binding, Value::symbol("describe-mode"));
}

#[test]
fn bootstrap_runtime_read_key_sequence_follows_help_describe_function_binding() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    eval.command_loop
        .keyboard
        .kboard
        .unread_events
        .push_back(Value::fixnum(8));
    eval.command_loop
        .keyboard
        .kboard
        .unread_events
        .push_back(Value::fixnum('f' as i64));

    let (keys, binding) = eval.read_key_sequence().expect("read C-h f sequence");
    assert_eq!(keys, vec![Value::fixnum(8), Value::fixnum('f' as i64)]);
    assert_eq!(binding, Value::symbol("describe-function"));
}

#[test]
fn bootstrap_runtime_read_char_from_input_rx_preserves_ctrl_h_help_char() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('h', crate::keyboard::Modifiers::ctrl()),
    ))
    .expect("queue C-h");
    drop(tx);

    eval.input_rx = Some(rx);
    let event = eval.read_char().expect("read queued C-h");
    assert_eq!(event, Value::fixnum(8));
}

#[test]
fn bootstrap_runtime_read_key_sequence_from_input_rx_follows_help_describe_function_binding() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('h', crate::keyboard::Modifiers::ctrl()),
    ))
    .expect("queue C-h");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('f'),
    ))
    .expect("queue f");
    drop(tx);

    eval.input_rx = Some(rx);
    let (keys, binding) = eval
        .read_key_sequence()
        .expect("read C-h f sequence from input_rx");
    assert_eq!(keys, vec![Value::fixnum(8), Value::fixnum('f' as i64)]);
    assert_eq!(binding, Value::symbol("describe-function"));
}

#[test]
fn bootstrap_runtime_read_key_sequence_from_input_rx_follows_help_describe_bindings_binding() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('h', crate::keyboard::Modifiers::ctrl()),
    ))
    .expect("queue C-h");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('b'),
    ))
    .expect("queue b");
    drop(tx);

    eval.input_rx = Some(rx);
    let (keys, binding) = eval
        .read_key_sequence()
        .expect("read C-h b sequence from input_rx");
    assert_eq!(keys, vec![Value::fixnum(8), Value::fixnum('b' as i64)]);
    assert_eq!(binding, Value::symbol("describe-bindings"));
}

#[test]
fn bootstrap_runtime_read_key_sequence_from_input_rx_leaves_following_minibuffer_input() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('h', crate::keyboard::Modifiers::ctrl()),
    ))
    .expect("queue C-h");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('f'),
    ))
    .expect("queue f");
    for ch in "find-file".chars() {
        tx.send(crate::keyboard::InputEvent::key_press(
            crate::keyboard::KeyEvent::char(ch),
        ))
        .expect("queue function chars");
    }
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Return),
    ))
    .expect("queue RET");
    drop(tx);

    eval.input_rx = Some(rx);
    let (keys, binding) = eval
        .read_key_sequence()
        .expect("read C-h f sequence from input_rx");
    assert_eq!(keys, vec![Value::fixnum(8), Value::fixnum('f' as i64)]);
    assert_eq!(binding, Value::symbol("describe-function"));

    let mut remaining = Vec::new();
    for _ in 0.."find-file".chars().count() {
        remaining.push(eval.read_char().expect("read queued minibuffer char"));
    }
    remaining.push(eval.read_char().expect("read queued minibuffer RET"));
    assert_eq!(
        remaining,
        "find-file"
            .chars()
            .map(|ch| Value::fixnum(ch as i64))
            .chain(std::iter::once(
                crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Return)
                    .to_emacs_event_value(),
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn bootstrap_runtime_save_some_buffers_space_saves_modified_file() {
    init_test_tracing();
    let dir = tempdir().expect("save-some tempdir");
    let file_path = dir.path().join("save-some-probe.txt");
    fs::write(&file_path, "alpha line\n").expect("write probe file");

    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let scratch = eval.buffers.create_buffer("*save-some-target*");
    eval.buffers.set_current(scratch);
    let frame_id = eval.frames.create_frame("F1", 960, 640, scratch);
    assert!(
        eval.frames.select_frame(frame_id),
        "runtime save-some-buffers test should have a selected frame"
    );

    let path_literal = format!("{:?}", file_path.to_string_lossy());
    eval.eval_str(&format!(
        r#"(progn
             (setq neo-save-some-error nil)
             (setq neo-save-some-save-buffer-ran nil)
             (advice-add
              'save-buffer :before
              (lambda (&rest _)
                (setq neo-save-some-save-buffer-ran t)))
             (defun neo-save-some-probe ()
               (interactive)
               (setq neo-save-some-error
                     (condition-case err
                         (list :ok (save-some-buffers nil))
                       (error
                        (list :error
                              err
                              last-command-event
                              this-command
                              real-this-command
                              last-input-event
                              last-nonmenu-event
                              (ignore-errors (selected-window))
                              (ignore-errors (frame-selected-window))
                              (ignore-errors (minibuffer-selected-window))
                              (ignore-errors (active-minibuffer-window))))))
               (setq neo-save-some-buffer-modified (buffer-modified-p))
               (exit-recursive-edit))
             (let ((buf (find-file-noselect {path_literal})))
               (switch-to-buffer buf)
               (goto-char (point-max))
               (insert "omega line\n")))"#
    ))
    .expect("setup save-some-buffers probe");

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('x', crate::keyboard::Modifiers::meta()),
    ))
    .expect("queue M-x");
    for ch in "neo-save-some-probe".chars() {
        tx.send(crate::keyboard::InputEvent::key_press(
            crate::keyboard::KeyEvent::char(ch),
        ))
        .expect("queue command chars");
    }
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Return),
    ))
    .expect("queue RET");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char(' '),
    ))
    .expect("queue SPC");
    drop(tx);

    eval.input_rx = Some(rx);
    eval.command_loop.running = true;

    let result = eval
        .recursive_edit_inner()
        .expect("command loop should exit normally");
    assert_eq!(result, Value::NIL);

    let saved = fs::read_to_string(&file_path).expect("read probe file after save-some-buffers");
    let save_buffer_ran = eval
        .eval_symbol("neo-save-some-save-buffer-ran")
        .expect("save-buffer trace var should exist");
    let save_error = eval
        .eval_symbol("neo-save-some-error")
        .expect("save-some error var should exist");
    let modified = eval
        .eval_symbol("neo-save-some-buffer-modified")
        .expect("buffer modified trace var should exist");

    assert_eq!(
        saved, "alpha line\nomega line\n",
        "error={save_error} save-buffer-ran={save_buffer_ran} modified={modified}"
    );
    assert_eq!(
        save_buffer_ran,
        Value::T,
        "error={save_error} saved={saved:?} modified={modified}"
    );
    assert_eq!(
        modified,
        Value::NIL,
        "error={save_error} save-buffer-ran={save_buffer_ran} saved={saved:?}"
    );
}

#[test]
fn bootstrap_runtime_command_loop_sets_last_nonmenu_event_for_keyboard_invocation() {
    init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let scratch = eval.buffers.create_buffer("*last-nonmenu-target*");
    eval.buffers.set_current(scratch);
    let frame_id = eval.frames.create_frame("F1", 960, 640, scratch);
    assert!(eval.frames.select_frame(frame_id));

    eval.eval_str(
        r#"(progn
             (setq neo-last-nonmenu-observed nil)
             (defun neo-last-nonmenu-probe ()
               (interactive)
               (setq neo-last-nonmenu-observed
                     (list last-command-event last-input-event last-nonmenu-event))
               (exit-recursive-edit)))"#,
    )
    .expect("define last-nonmenu probe");

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('x', crate::keyboard::Modifiers::meta()),
    ))
    .expect("queue M-x");
    for ch in "neo-last-nonmenu-probe".chars() {
        tx.send(crate::keyboard::InputEvent::key_press(
            crate::keyboard::KeyEvent::char(ch),
        ))
        .expect("queue command chars");
    }
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Return),
    ))
    .expect("queue RET");
    drop(tx);

    eval.input_rx = Some(rx);
    eval.command_loop.running = true;

    let result = eval
        .recursive_edit_inner()
        .expect("command loop should exit normally");
    assert_eq!(result, Value::NIL);

    let observed = eval
        .eval_symbol("neo-last-nonmenu-observed")
        .expect("probe var should exist");
    assert_eq!(
        observed,
        Value::list(vec![
            Value::fixnum('\r' as i64),
            Value::fixnum('\r' as i64),
            Value::fixnum('\r' as i64),
        ]),
    );
}

#[test]
fn bootstrap_runtime_command_loop_disabled_command_consumes_space_reply_once() {
    init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let scratch = eval.buffers.create_buffer("*disabled-command-target*");
    eval.buffers.set_current(scratch);
    let frame_id = eval.frames.create_frame("F1", 960, 640, scratch);
    assert!(eval.frames.select_frame(frame_id));

    eval.eval_str(
        r#"(progn
             (switch-to-buffer "*disabled-command-target*")
             (erase-buffer)
             (insert "ALPHA LINE\nBETA LINE\n")
             (goto-char (point-min))
             (setq neo-disabled-command-finish nil))"#,
    )
    .expect("setup disabled-command probe");

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('x', crate::keyboard::Modifiers::ctrl()),
    ))
    .expect("queue C-x");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('h'),
    ))
    .expect("queue h");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('x', crate::keyboard::Modifiers::ctrl()),
    ))
    .expect("queue C-x");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('l', crate::keyboard::Modifiers::ctrl()),
    ))
    .expect("queue C-l");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char(' '),
    ))
    .expect("queue SPC reply");
    drop(tx);

    eval.input_rx = Some(rx);
    eval.command_loop.running = true;

    let result = eval
        .recursive_edit_inner()
        .expect("disabled-command loop should exit normally");
    assert_eq!(result, Value::NIL);

    let observed = eval_rendered(
        &mut eval,
        r#"(list
             (with-current-buffer "*disabled-command-target*"
               (buffer-string))
             (buffer-name (current-buffer))
             (buffer-name (window-buffer (selected-window)))
             (not (null (get-buffer "*Disabled Command*"))))"#,
    );
    assert_eq!(
        observed,
        "OK (\"alpha line\nbeta line\n\" \"*disabled-command-target*\" \"*disabled-command-target*\" nil)",
    );
}

#[test]
fn bootstrap_runtime_command_loop_universal_argument_prefix_reaches_following_command() {
    init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let scratch = eval.buffers.create_buffer("*universal-argument-target*");
    eval.buffers.set_current(scratch);
    let frame_id = eval.frames.create_frame("F1", 960, 640, scratch);
    assert!(eval.frames.select_frame(frame_id));

    eval.eval_str(
        r#"(progn
             (switch-to-buffer "*universal-argument-target*")
             (erase-buffer)
             (setq neo-universal-argument-finish nil))"#,
    )
    .expect("setup universal argument probe");

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('u', crate::keyboard::Modifiers::ctrl()),
    ))
    .expect("queue C-u");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('8'),
    ))
    .expect("queue 8");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('a'),
    ))
    .expect("queue a");
    drop(tx);

    eval.input_rx = Some(rx);
    eval.command_loop.running = true;

    let result = eval
        .recursive_edit_inner()
        .expect("universal argument loop should exit normally");
    assert_eq!(result, Value::NIL);

    let observed = eval_rendered(
        &mut eval,
        r#"(list
             (with-current-buffer "*universal-argument-target*"
               (buffer-string))
             prefix-arg)"#,
    );
    assert_eq!(observed, r#"OK ("aaaaaaaa" nil)"#);
}

#[test]
fn bootstrap_runtime_disabled_command_from_visited_file_restores_single_selected_file_window() {
    init_test_tracing();
    let dir = tempdir().expect("visited-file disabled-command tempdir");
    let file_path = dir.path().join("disabled-command-file.txt");
    fs::write(&file_path, "ALPHA LINE\nBETA LINE\n").expect("write disabled-command file");

    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let scratch = eval
        .buffers
        .find_buffer_by_name("*scratch*")
        .unwrap_or_else(|| eval.buffers.create_buffer("*scratch*"));
    eval.buffers.set_current(scratch);
    let frame_id = eval.frames.create_frame("F1", 960, 640, scratch);
    assert!(eval.frames.select_frame(frame_id));

    eval.eval_str(&format!(
        r#"(progn
             (switch-to-buffer "*scratch*")
             (let ((buf (find-file-noselect {path:?})))
               (switch-to-buffer buf))
             (goto-char (point-min))
             (setq neo-disabled-command-finish nil))"#,
        path = file_path.to_string_lossy(),
    ))
    .expect("setup visited-file disabled-command probe");

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('x', crate::keyboard::Modifiers::ctrl()),
    ))
    .expect("queue C-x");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('h'),
    ))
    .expect("queue h");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('x', crate::keyboard::Modifiers::ctrl()),
    ))
    .expect("queue C-x");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('l', crate::keyboard::Modifiers::ctrl()),
    ))
    .expect("queue C-l");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char(' '),
    ))
    .expect("queue SPC reply");
    drop(tx);

    eval.input_rx = Some(rx);
    eval.command_loop.running = true;

    let result = eval
        .recursive_edit_inner()
        .expect("disabled-command loop should exit normally");
    assert_eq!(result, Value::NIL);

    let observed = eval_rendered(
        &mut eval,
        r#"(list
             (buffer-name (current-buffer))
             (buffer-name (window-buffer (selected-window)))
             (mapcar (lambda (w) (buffer-name (window-buffer w))) (window-list))
             (with-current-buffer "disabled-command-file.txt"
               (buffer-string))
             (not (null (get-buffer "*Disabled Command*"))))"#,
    );
    assert_eq!(
        observed,
        "OK (\"disabled-command-file.txt\" \"disabled-command-file.txt\" (\"disabled-command-file.txt\") \"alpha line\nbeta line\n\" nil)",
    );
}

#[test]
fn bootstrap_runtime_display_buffer_pop_up_window_records_quit_restore_window_metadata() {
    init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let scratch = eval
        .buffers
        .find_buffer_by_name("*scratch*")
        .unwrap_or_else(|| eval.buffers.create_buffer("*scratch*"));
    eval.buffers.set_current(scratch);
    let frame_id = eval.frames.create_frame("F1", 960, 640, scratch);
    assert!(eval.frames.select_frame(frame_id));

    let observed = eval_rendered(
        &mut eval,
        r#"(let* ((orig (generate-new-buffer "*qr-orig*"))
                  (target (get-buffer-create "*qr-target*")))
             (switch-to-buffer orig)
             (let* ((window (display-buffer target '(display-buffer-pop-up-window)))
                    (quit-restore (window-parameter window 'quit-restore)))
               (list (car quit-restore)
                     (nth 1 quit-restore)
                     (eq (nth 2 quit-restore) (selected-window))
                     (buffer-name (nth 3 quit-restore)))))"#,
    );
    assert_eq!(observed, r#"OK (window window t "*qr-target*")"#);
}

#[test]
fn bootstrap_runtime_kill_buffer_quit_windows_deletes_pop_up_help_window() {
    init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let scratch = eval
        .buffers
        .find_buffer_by_name("*scratch*")
        .unwrap_or_else(|| eval.buffers.create_buffer("*scratch*"));
    eval.buffers.set_current(scratch);
    let frame_id = eval.frames.create_frame("F1", 960, 640, scratch);
    assert!(eval.frames.select_frame(frame_id));

    let observed = eval_rendered(
        &mut eval,
        r#"(let* ((orig (generate-new-buffer "*qr-kill-orig*"))
                  (help (get-buffer-create "*qr-kill-help*")))
             (switch-to-buffer orig)
             (display-buffer help '(display-buffer-pop-up-window))
             (let ((kill-buffer-quit-windows t))
               (kill-buffer help))
             (list (count-windows)
                   (buffer-name (current-buffer))
                   (buffer-name (window-buffer (selected-window)))
                   (mapcar (lambda (w) (buffer-name (window-buffer w))) (window-list))
                   (get-buffer "*qr-kill-help*")))"#,
    );
    assert_eq!(
        observed,
        r#"OK (1 "*qr-kill-orig*" "*qr-kill-orig*" ("*qr-kill-orig*") nil)"#
    );
}

#[test]
fn bootstrap_runtime_cx_s_space_saves_typed_edit_from_command_loop() {
    init_test_tracing();
    let dir = tempdir().expect("save-some typed tempdir");
    let file_path = dir.path().join("save-some-typed.txt");
    fs::write(&file_path, "alpha line\n").expect("write typed probe file");

    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let scratch = eval.buffers.create_buffer("*save-some-typed-target*");
    eval.buffers.set_current(scratch);
    let frame_id = eval.frames.create_frame("F1", 960, 640, scratch);
    assert!(eval.frames.select_frame(frame_id));

    let path_literal = format!("{:?}", file_path.to_string_lossy());
    eval.eval_str(&format!(
        r#"(progn
             (setq neo-save-some-typed-finish nil)
             (setq neo-save-some-typed-save-buffer-ran nil)
             (advice-add
              'save-buffer :before
              (lambda (&rest _)
                (setq neo-save-some-typed-save-buffer-ran t)))
             (defun neo-save-some-typed-finish ()
               (interactive)
               (setq neo-save-some-typed-finish
                     (list
                      (buffer-name)
                      (buffer-modified-p)
                      last-command-event
                      this-command
                      real-this-command
                      last-input-event
                      last-nonmenu-event))
               (exit-recursive-edit))
             (let ((buf (find-file-noselect {path_literal})))
               (switch-to-buffer buf)
               (goto-char (point-max))))"#
    ))
    .expect("setup typed save-some probe");

    let (tx, rx) = crossbeam_channel::unbounded();
    for ch in "omega line\n".chars() {
        tx.send(crate::keyboard::InputEvent::key_press(
            crate::keyboard::KeyEvent::char(ch),
        ))
        .expect("queue typed chars");
    }
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('x', crate::keyboard::Modifiers::ctrl()),
    ))
    .expect("queue C-x");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('s'),
    ))
    .expect("queue s");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char(' '),
    ))
    .expect("queue SPC");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('x', crate::keyboard::Modifiers::meta()),
    ))
    .expect("queue M-x");
    for ch in "neo-save-some-typed-finish".chars() {
        tx.send(crate::keyboard::InputEvent::key_press(
            crate::keyboard::KeyEvent::char(ch),
        ))
        .expect("queue finish chars");
    }
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Return),
    ))
    .expect("queue finish RET");
    drop(tx);

    eval.input_rx = Some(rx);
    eval.command_loop.running = true;

    let result = eval
        .recursive_edit_inner()
        .expect("typed save-some command loop should exit normally");
    assert_eq!(result, Value::NIL);

    let saved = fs::read_to_string(&file_path).expect("read typed probe file after save-some");
    let save_buffer_ran = eval
        .eval_symbol("neo-save-some-typed-save-buffer-ran")
        .expect("typed save-buffer trace var should exist");
    let finish = eval
        .eval_symbol("neo-save-some-typed-finish")
        .expect("typed finish var should exist");
    let modified = eval
        .buffers
        .current_buffer()
        .expect("current buffer after typed save-some")
        .is_modified();

    assert_eq!(
        saved, "alpha line\nomega line\n",
        "finish={finish} save-buffer-ran={save_buffer_ran} modified={modified}"
    );
    assert_eq!(
        save_buffer_ran,
        Value::T,
        "finish={finish} saved={saved:?} modified={modified}"
    );
    assert!(
        !modified,
        "finish={finish} save-buffer-ran={save_buffer_ran} saved={saved:?}"
    );
}

#[test]
fn bootstrap_runtime_command_loop_logs_help_route_for_ch_f() {
    init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let scratch = eval.buffers.create_buffer("*help-f-route-target*");
    eval.buffers.set_current(scratch);
    let frame_id = eval.frames.create_frame("F1", 960, 640, scratch);
    assert!(
        eval.frames.select_frame(frame_id),
        "runtime command-loop help route test should have a selected frame"
    );

    let _ = eval.eval_str_each(
        r#"(progn
             (setq neo-help-route-log nil)
             (defun neo--capture-prefix-help (&rest _args)
               (setq neo-help-route-log (append neo-help-route-log '(describe-prefix-bindings)))
               (exit-recursive-edit))
             (defun neo--capture-describe-function (&rest _args)
               (setq neo-help-route-log (append neo-help-route-log '(describe-function)))
               (exit-recursive-edit))
             (advice-add 'describe-prefix-bindings :before #'neo--capture-prefix-help)
             (advice-add 'describe-function :before #'neo--capture-describe-function))"#,
    );

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('h', crate::keyboard::Modifiers::ctrl()),
    ))
    .expect("queue C-h");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('f'),
    ))
    .expect("queue f");
    drop(tx);

    eval.input_rx = Some(rx);
    eval.command_loop.running = true;

    let _ = eval.recursive_edit_inner();
    assert_eq!(
        format_eval_result(&eval.eval_str(
            r#"(prog1 neo-help-route-log
                 (advice-remove 'describe-prefix-bindings #'neo--capture-prefix-help)
                 (advice-remove 'describe-function #'neo--capture-describe-function)
                 (fmakunbound 'neo--capture-prefix-help)
                 (fmakunbound 'neo--capture-describe-function)
                 (makunbound 'neo-help-route-log))"#
        )),
        "OK (describe-function)",
        "expected C-h f command-loop path to dispatch describe-function, not prefix help"
    );
}

#[test]
fn bootstrap_runtime_command_loop_logs_help_route_for_ch_b() {
    init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let scratch = eval.buffers.create_buffer("*help-b-route-target*");
    eval.buffers.set_current(scratch);
    let frame_id = eval.frames.create_frame("F1", 960, 640, scratch);
    assert!(
        eval.frames.select_frame(frame_id),
        "runtime command-loop help route test should have a selected frame"
    );

    let _ = eval.eval_str_each(
        r#"(progn
             (setq neo-help-b-route-log nil)
             (defun neo--capture-prefix-help-b (&rest _args)
               (setq neo-help-b-route-log
                     (append neo-help-b-route-log '(describe-prefix-bindings)))
               (exit-recursive-edit))
             (defun neo--capture-describe-bindings (&rest _args)
               (setq neo-help-b-route-log
                     (append neo-help-b-route-log '(describe-bindings)))
               (exit-recursive-edit))
             (advice-add 'describe-prefix-bindings :before #'neo--capture-prefix-help-b)
             (advice-add 'describe-bindings :before #'neo--capture-describe-bindings))"#,
    );

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('h', crate::keyboard::Modifiers::ctrl()),
    ))
    .expect("queue C-h");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('b'),
    ))
    .expect("queue b");
    drop(tx);

    eval.input_rx = Some(rx);
    eval.command_loop.running = true;

    let _ = eval.recursive_edit_inner();
    assert_eq!(
        format_eval_result(&eval.eval_str(
            r#"(prog1 neo-help-b-route-log
                 (advice-remove 'describe-prefix-bindings #'neo--capture-prefix-help-b)
                 (advice-remove 'describe-bindings #'neo--capture-describe-bindings)
                 (fmakunbound 'neo--capture-prefix-help-b)
                 (fmakunbound 'neo--capture-describe-bindings)
                 (makunbound 'neo-help-b-route-log))"#
        )),
        "OK (describe-bindings)",
        "expected C-h b command-loop path to dispatch describe-bindings, not prefix help"
    );
}

#[test]
fn bootstrap_runtime_command_loop_traces_describe_function_body_for_ch_f() {
    init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    let scratch = eval.buffers.create_buffer("*help-f-trace-target*");
    eval.buffers.set_current(scratch);
    let frame_id = eval.frames.create_frame("F1", 960, 640, scratch);
    assert!(eval.frames.select_frame(frame_id));

    let _ = eval.eval_str_each(
        r#"(progn
             (setq neo-help-f-trace nil)
             (defun neo--trace-describe-function (orig &rest args)
               (setq neo-help-f-trace (append neo-help-f-trace '(entered)))
               (condition-case err
                   (prog1 (apply orig args)
                     (setq neo-help-f-trace (append neo-help-f-trace '(returned)))
                     (exit-recursive-edit))
                 (error
                  (setq neo-help-f-trace
                        (append neo-help-f-trace (list (list 'error err))))
                  (exit-recursive-edit)
                  nil)))
             (advice-add 'describe-function :around #'neo--trace-describe-function))"#,
    );

    let (tx, rx) = crossbeam_channel::unbounded();
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char_with_mods('h', crate::keyboard::Modifiers::ctrl()),
    ))
    .expect("queue C-h");
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::char('f'),
    ))
    .expect("queue f");
    for ch in "find-file".chars() {
        tx.send(crate::keyboard::InputEvent::key_press(
            crate::keyboard::KeyEvent::char(ch),
        ))
        .expect("queue function chars");
    }
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Return),
    ))
    .expect("queue RET");
    drop(tx);

    eval.input_rx = Some(rx);
    eval.command_loop.running = true;
    let result = eval
        .recursive_edit_inner()
        .expect("command loop should exit via trace advice");
    assert_eq!(result, Value::NIL);

    let rendered = format_eval_result(&eval.eval_str(
        r#"(prog1 neo-help-f-trace
             (advice-remove 'describe-function #'neo--trace-describe-function)
             (fmakunbound 'neo--trace-describe-function)
             (makunbound 'neo-help-f-trace))"#,
    ));

    assert_eq!(
        rendered, "OK (entered returned)",
        "expected C-h f command-loop path to enter and return from describe-function"
    );
}

#[test]
fn bootstrap_runtime_documentation_resolves_compiled_bytecode_doc_refs() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = format_eval_result(&eval.eval_str(
        r#"(list (string-prefix-p
                  "Major mode for typing and evaluating Lisp forms."
                  (documentation 'lisp-interaction-mode t))
                 (stringp (documentation 'emacs-lisp-mode t))
                 (stringp (documentation 'fundamental-mode t)))"#,
    ));

    assert_eq!(rendered, "OK (t t t)");
}

#[test]
fn bootstrap_runtime_lookup_key_accepts_keymap_spine_tails_like_gnu() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = format_eval_result(&eval.eval_str(
        r#"(let ((tail (cdr lisp-interaction-mode-map)))
             (list (lookup-key tail [10] t)
                   (lookup-key tail [127] t)))"#,
    ));

    assert_eq!(
        rendered,
        "OK (eval-print-last-sexp backward-delete-char-untabify)"
    );
}

#[test]
fn bootstrap_runtime_documentation_substitutes_lisp_interaction_mode_keymap_help() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = format_eval_result(&eval.eval_str(
        r#"(let ((doc (documentation 'lisp-interaction-mode)))
             (list (stringp doc)
                   (string-prefix-p
                    "Major mode for typing and evaluating Lisp forms."
                    doc)
                   (not (null (string-match-p
                               "converts tabs to spaces as it moves back"
                               doc)))))"#,
    ));

    assert_eq!(rendered, "OK (t t t)");
}

#[test]
fn bootstrap_runtime_loads_gnu_window_split_entry_point() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    let rendered = format_eval_result(&eval.eval_str(
        "(list (fboundp 'split-window)
               (let ((w (split-window)))
                 (list (window-live-p w)
                       (length (window-list)))))",
    ));
    assert_eq!(rendered, "OK (t (t 2))");
}

#[test]
fn bootstrap_runtime_cl_reduce_entry_point_works() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
fn runtime_interpreted_closure_filter_requires_explicit_runtime_binding() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.set_lexical_binding(true);
    eval.eval_str("(setq neovm--hook-count 0)")
        .expect("initialize hook count");
    sync_runtime_interpreted_closure_filter(&mut eval);
    let rendered = eval_rendered(
        &mut eval,
        r#"(list
             internal-make-interpreted-closure-function
             (funcall (let ((x 1)) (lambda () x)))
             (funcall (let ((x 1)) (lambda () x)))
             neovm--hook-count)"#,
    );
    assert_eq!(rendered, "OK (nil 1 1 0)");
}

#[test]
fn runtime_interpreted_closure_filter_honors_explicit_runtime_binding() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.set_lexical_binding(true);
    eval.eval_str(
        r#"
        (setq neovm--hook-count 0)
        (fset 'cconv-make-interpreted-closure
              (lambda (args body env docstring iform)
                (setq neovm--hook-count (1+ neovm--hook-count))
                (make-interpreted-closure args body env docstring iform)))
        (setq internal-make-interpreted-closure-function
              'cconv-make-interpreted-closure)
        "#,
    )
    .expect("eval forms");
    sync_runtime_interpreted_closure_filter(&mut eval);
    let rendered = eval_rendered(
        &mut eval,
        r#"(progn
             (list
              internal-make-interpreted-closure-function
              (funcall (let ((x 1)) (lambda () x)))
              neovm--hook-count))"#,
    );
    assert_eq!(rendered, "OK (cconv-make-interpreted-closure 1 1)");
}

#[test]
fn bootstrap_runtime_cl_defstruct_macroexpand_all_head_matches_gnu() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
fn bootstrap_runtime_autoload_do_load_survives_exact_gc() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");
    eval.gc_stress = true;
    let rendered = eval_rendered(
        &mut eval,
        r#"(progn
             (require 'cl-lib)
             (let ((before (symbol-function 'cl-defstruct)))
               (list
                 (autoloadp before)
                 (condition-case err
                     (progn
                       (autoload-do-load before 'cl-defstruct t)
                       (cl-defstruct vm-autoload-exact slot)
                       (vm-autoload-exact-slot (make-vm-autoload-exact :slot 91)))
                   (error err)))))"#,
    );
    assert_eq!(rendered, "OK (t 91)");
}

#[test]
fn bootstrap_runtime_cl_defun_entry_point_matches_gnu() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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

fn eval_rendered(eval: &mut Context, form: &str) -> String {
    match eval.eval_str(form) {
        Ok(value) => format!(
            "OK {}",
            crate::emacs_core::print::print_value_with_buffers(&value, &eval.buffers)
        ),
        Err(err) => format!("ERR {}", format_eval_error(eval, &err)),
    }
}

#[test]
fn bootstrap_condition_case_lexical_handler_binding_restores_outer_let() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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

    // GNU verified: `(list left-margin-width right-margin-width ...)`
    // returns `(0 0 nil nil nil nil nil t t)` after fresh batch
    // startup. Earlier expectation of nil-nil pre-dated the
    // BUFFER_OBJFWD slot defaults that init left/right margins to 0.
    assert_eq!(rendered, "OK (0 0 nil nil nil nil nil t t)");
}

#[test]
fn bootstrap_runtime_standard_fontset_spec_creates_named_fontset() {
    crate::test_utils::init_test_tracing();
    let mut eval =
        create_bootstrap_evaluator_cached_with_features(&["neomacs"]).expect("bootstrap evaluator");
    let result = eval
        .eval_str(
            r#"(let ((name (create-fontset-from-fontset-spec standard-fontset-spec t)))
             (list name (query-fontset "fontset-standard")))"#,
        )
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
    crate::test_utils::init_test_tracing();
    let mut eval =
        create_bootstrap_evaluator_with_features(&["neomacs"]).expect("fresh bootstrap evaluator");
    let rendered = eval_rendered(
        &mut eval,
        r#"(list (charsetp 'devanagari-cdac)
                 (aref char-script-table ?好))"#,
    );
    assert_eq!(rendered, "OK (t han)");

    eval.eval_str("(setup-default-fontset)")
        .expect("setup-default-fontset should evaluate");

    let entries = matching_entries_for_fontset(DEFAULT_FONTSET_NAME, '好');
    let registries: Vec<Option<String>> = entries
        .iter()
        .take(23)
        .map(|entry| match entry {
            FontSpecEntry::Font(spec) => spec.registry.map(|sym| resolve_sym(sym).to_string()),
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
fn bootstrap_neomacs_runtime_loads_neo_term_layer() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_with_features(&["neomacs"])
        .expect("neomacs bootstrap evaluator");
    assert!(eval.feature_present("neomacs"));
    assert!(eval.feature_present("neo-win"));
    assert!(!eval.feature_present("x-win"));
}

#[test]
fn bootstrap_neomacs_gui_runtime_loads_x_and_neo_term_layers() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_with_features(&["neomacs", "x"])
        .expect("neomacs+x bootstrap evaluator");
    assert!(eval.feature_present("neomacs"));
    assert!(eval.feature_present("x"));
    assert!(eval.feature_present("neo-win"));
    assert!(eval.feature_present("x-win"));
}

#[test]
fn loadup_source_preloads_mouse_help_fixup_runtime_surface() {
    crate::test_utils::init_test_tracing();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let loadup = project_root.join("lisp/loadup.el");
    let source = fs::read_to_string(&loadup).expect("read loadup.el");

    assert!(
        source.contains("(load \"mouse\")"),
        "loadup.el should preload mouse.el so mouse-fixup-help-message is on the normal runtime surface"
    );
}

#[test]
fn bootstrap_help_fns_loads_and_preserves_hook_depth_metadata() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
fn bootstrap_runtime_call_interactively_autoloaded_describe_function_reads_prompt() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let (tx, rx) = crossbeam_channel::unbounded();
    drop(tx);
    eval.input_rx = Some(rx);
    eval.command_loop.running = true;

    for ch in "find-file".chars() {
        eval.command_loop
            .keyboard
            .kboard
            .unread_events
            .push_back(Value::fixnum(ch as i64));
    }
    eval.command_loop.keyboard.kboard.unread_events.push_back(
        crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Return).to_emacs_event_value(),
    );

    let result = eval
        .apply(
            Value::symbol("call-interactively"),
            vec![Value::symbol("describe-function")],
        )
        .expect("call-interactively should read describe-function args");
    assert!(
        result.is_string(),
        "describe-function should still return its help buffer string, got {result}"
    );

    let rendered = eval_rendered(
        &mut eval,
        r#"(list
             (autoloadp (symbol-function 'describe-function))
             (with-current-buffer "*Help*"
               (not (null (save-excursion
                            (goto-char (point-min))
                            (search-forward "find-file is" nil t)))))
             (with-current-buffer "*Help*"
               (not (null (save-excursion
                            (goto-char (point-min))
                            (search-forward "C-x C-f" nil t))))))"#,
    );

    assert_eq!(rendered, "OK (nil t t)");
}

#[test]
fn bootstrap_runtime_command_execute_autoloaded_describe_function_reads_prompt() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let (tx, rx) = crossbeam_channel::unbounded();
    drop(tx);
    eval.input_rx = Some(rx);
    eval.command_loop.running = true;

    for ch in "find-file".chars() {
        eval.command_loop
            .keyboard
            .kboard
            .unread_events
            .push_back(Value::fixnum(ch as i64));
    }
    eval.command_loop.keyboard.kboard.unread_events.push_back(
        crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Return).to_emacs_event_value(),
    );

    let result = eval
        .apply(
            Value::symbol("command-execute"),
            vec![Value::symbol("describe-function")],
        )
        .expect("command-execute should read describe-function args");
    assert!(
        result.is_string(),
        "describe-function should still return its help buffer string, got {result}"
    );

    let rendered = eval_rendered(
        &mut eval,
        r#"(list
             (autoloadp (symbol-function 'describe-function))
             (with-current-buffer "*Help*"
               (not (null (save-excursion
                            (goto-char (point-min))
                            (search-forward "find-file is" nil t)))))
             (with-current-buffer "*Help*"
               (not (null (save-excursion
                            (goto-char (point-min))
                            (search-forward "C-x C-f" nil t))))))"#,
    );

    assert_eq!(rendered, "OK (nil t t)");
}

#[test]
fn bootstrap_runtime_call_interactively_autoloaded_describe_function_reads_prompt_from_input_rx() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let (tx, rx) = crossbeam_channel::unbounded();
    for ch in "find-file".chars() {
        tx.send(crate::keyboard::InputEvent::key_press(
            crate::keyboard::KeyEvent::char(ch),
        ))
        .expect("queue function chars");
    }
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Return),
    ))
    .expect("queue RET");
    drop(tx);

    eval.input_rx = Some(rx);
    eval.command_loop.running = true;

    let result = eval
        .apply(
            Value::symbol("call-interactively"),
            vec![Value::symbol("describe-function")],
        )
        .expect("call-interactively should read describe-function args from input_rx");
    assert!(
        result.is_string(),
        "describe-function should still return its help buffer string, got {result}"
    );

    let rendered = eval_rendered(
        &mut eval,
        r#"(list
             (autoloadp (symbol-function 'describe-function))
             (with-current-buffer "*Help*"
               (not (null (save-excursion
                            (goto-char (point-min))
                            (search-forward "find-file is" nil t)))))
             (with-current-buffer "*Help*"
               (not (null (save-excursion
                            (goto-char (point-min))
                            (search-forward "C-x C-f" nil t))))))"#,
    );

    assert_eq!(rendered, "OK (nil t t)");
}

#[test]
fn bootstrap_runtime_command_execute_autoloaded_describe_function_reads_prompt_from_input_rx() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let (tx, rx) = crossbeam_channel::unbounded();
    for ch in "find-file".chars() {
        tx.send(crate::keyboard::InputEvent::key_press(
            crate::keyboard::KeyEvent::char(ch),
        ))
        .expect("queue function chars");
    }
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Return),
    ))
    .expect("queue RET");
    drop(tx);

    eval.input_rx = Some(rx);
    eval.command_loop.running = true;

    let result = eval
        .apply(
            Value::symbol("command-execute"),
            vec![Value::symbol("describe-function")],
        )
        .expect("command-execute should read describe-function args from input_rx");
    assert!(
        result.is_string(),
        "describe-function should still return its help buffer string, got {result}"
    );

    let rendered = eval_rendered(
        &mut eval,
        r#"(list
             (autoloadp (symbol-function 'describe-function))
             (with-current-buffer "*Help*"
               (not (null (save-excursion
                            (goto-char (point-min))
                            (search-forward "find-file is" nil t)))))
             (with-current-buffer "*Help*"
               (not (null (save-excursion
                            (goto-char (point-min))
                            (search-forward "C-x C-f" nil t))))))"#,
    );

    assert_eq!(rendered, "OK (nil t t)");
}

#[test]
fn bootstrap_runtime_call_interactively_describe_function_with_outer_command_state() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let scratch = eval
        .buffers
        .create_buffer("*describe-function-state-target*");
    eval.buffers.set_current(scratch);
    let frame_id = eval.frames.create_frame("F1", 960, 640, scratch);
    assert!(eval.frames.select_frame(frame_id));

    let (tx, rx) = crossbeam_channel::unbounded();
    for ch in "find-file".chars() {
        tx.send(crate::keyboard::InputEvent::key_press(
            crate::keyboard::KeyEvent::char(ch),
        ))
        .expect("queue function chars");
    }
    tx.send(crate::keyboard::InputEvent::key_press(
        crate::keyboard::KeyEvent::named(crate::keyboard::NamedKey::Return),
    ))
    .expect("queue RET");
    drop(tx);

    eval.input_rx = Some(rx);
    eval.command_loop.running = true;
    let help_keys = vec![Value::fixnum(8), Value::fixnum('f' as i64)];
    eval.set_command_key_sequences(help_keys.clone(), help_keys);
    eval.assign("this-command", Value::symbol("describe-function"));
    eval.assign("real-this-command", Value::symbol("describe-function"));
    eval.assign("this-original-command", Value::symbol("describe-function"));
    eval.assign("last-command-event", Value::fixnum('f' as i64));

    let result = eval
        .apply(
            Value::symbol("call-interactively"),
            vec![Value::symbol("describe-function")],
        )
        .expect("call-interactively should succeed with outer command state");
    assert!(
        result.is_string(),
        "describe-function should still return its help buffer string, got {result}"
    );

    let rendered = eval_rendered(
        &mut eval,
        r#"(list
             (with-current-buffer "*Help*"
               (not (null (save-excursion
                            (goto-char (point-min))
                            (search-forward "find-file is" nil t)))))
             (with-current-buffer "*Help*"
               (not (null (save-excursion
                            (goto-char (point-min))
                            (search-forward "C-x C-f" nil t))))))"#,
    );

    assert_eq!(rendered, "OK (t t)");
}

#[test]
fn bootstrap_runtime_describe_bindings_includes_major_mode_section() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"(progn
             (describe-bindings)
             (with-current-buffer "*Help*"
               (list
                (not (null (save-excursion
                             (goto-char (point-min))
                             (search-forward "Major Mode Bindings" nil t))))
                (not (null (save-excursion
                             (goto-char (point-min))
                             (search-forward "lisp-interaction-mode" nil t)))))))"#,
    );

    assert_eq!(rendered, "OK (t t)");
}

#[test]
fn bootstrap_runtime_describe_bindings_window_starts_at_visible_heading() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"(progn
             (describe-bindings)
             (let* ((w (get-buffer-window "*Help*"))
                    (ws (window-start w))
                    (visible
                     (with-current-buffer "*Help*"
                       (save-excursion
                         (goto-char ws)
                         (while (and (< (point) (point-max))
                                     (or (get-text-property (point) 'invisible)
                                         (memq (char-after) '(?\n ?\r ?\t ?\f ? ))))
                           (forward-char 1))
                         (buffer-substring-no-properties
                          (point)
                          (min (point-max) (+ (point) 160)))))))
               (list
                (windowp w)
                ws
                visible)))"#,
    );

    assert!(
        rendered.starts_with("OK (t "),
        "describe-bindings should display in a live help window, got {rendered}"
    );
    assert!(
        rendered.contains("Key translations"),
        "describe-bindings should start at the GNU key-translations heading, got {rendered}"
    );
}

#[test]
fn bootstrap_runtime_describe_variable_autoloads_help_fns() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    if std::env::var("NEOVM_PROFILE_BOOTSTRAP_FILE").is_err() {
        return;
    }

    crate::test_utils::init_test_tracing();

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
    let path = if std::env::var("NEOVM_PROFILE_BOOTSTRAP_DISABLE_NEOBC").as_deref() == Ok("1") {
        let source_path = source_suffixed_path(&path.with_extension(""));
        let temp = tempfile::tempdir().expect("tempdir for source-only bootstrap profile");
        let copied = temp.path().join(
            source_path
                .file_name()
                .expect("bootstrap source file should have name"),
        );
        std::fs::write(
            &copied,
            std::fs::read_to_string(&source_path).unwrap_or_else(|err| {
                panic!(
                    "read source bootstrap file {}: {err}",
                    source_path.display()
                )
            }),
        )
        .unwrap_or_else(|err| panic!("copy source bootstrap file {}: {err}", copied.display()));
        copied
    } else {
        path
    };

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
fn strip_reader_prefix_handles_bom_and_shebang() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    assert_eq!(
        lexical_binding_cookie_in_file_local_cookie_line(
            ";; -*- mode: emacs-lisp; lexical-binding: nil; -*-",
        ),
        LexicalBindingCookie::Dynamic,
        "explicit lexical-binding: nil cookie should force dynamic binding",
    );
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
    assert_eq!(
        lexical_binding_cookie_for_source(
            "#!/usr/bin/env emacs --script\n;; -*- lexical-binding: nil; -*-\n(setq vm-lb 1)\n",
        ),
        LexicalBindingCookie::Dynamic,
        "second-line lexical-binding: nil cookie should be honored for shebang scripts",
    );
}

#[test]
fn lexical_binding_cookie_for_lisp_source_handles_raw_unibyte_cookie_lines() {
    crate::test_utils::init_test_tracing();

    let lexical = crate::heap_types::LispString::from_unibyte(
        b"#!/usr/bin/env emacs --script\n;; \xFF -*- lexical-binding: t; mode: emacs-lisp; -*-\n"
            .to_vec(),
    );
    assert_eq!(
        lexical_binding_cookie_for_lisp_source(&lexical),
        LexicalBindingCookie::Lexical,
        "raw unibyte shebang sources should still expose lexical-binding cookies",
    );

    let dynamic = crate::heap_types::LispString::from_unibyte(
        b";; -*- lexical-binding: nil; foo: \xFE -*-\n(setq vm-lb 1)\n".to_vec(),
    );
    assert_eq!(
        lexical_binding_cookie_for_lisp_source(&dynamic),
        LexicalBindingCookie::Dynamic,
        "raw unibyte cookie lines should preserve explicit dynamic binding",
    );
}

#[test]
fn find_file_nonexistent() {
    crate::test_utils::init_test_tracing();
    assert!(find_file_in_load_path("nonexistent", &[]).is_none());
}

#[test]
fn load_path_extraction() {
    crate::test_utils::init_test_tracing();
    let mut ob = super::super::symbol::Obarray::new();
    ob.set_symbol_value("default-directory", Value::string("/tmp/project"));
    ob.set_symbol_value(
        "load-path",
        Value::list(vec![
            Value::string("/usr/share/emacs/lisp"),
            Value::NIL,
            Value::string("/home/user/.emacs.d"),
        ]),
    );
    let paths = get_load_path(&ob);
    assert_eq!(
        load_path_runtime_strings(&paths),
        vec![
            "/usr/share/emacs/lisp",
            "/tmp/project",
            "/home/user/.emacs.d"
        ]
    );
}

#[test]
fn plan_load_accepts_raw_unibyte_filename_values() {
    crate::test_utils::init_test_tracing();
    let mut ob = super::super::symbol::Obarray::new();
    ob.set_symbol_value("default-directory", Value::string("/tmp"));
    ob.set_symbol_value("load-path", Value::list(vec![Value::string("/tmp")]));

    let plan = plan_load_in_state(
        &ob,
        Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![0xFF])),
        Some(Value::T),
        None,
        None,
    )
    .expect("raw unibyte file values should be accepted");

    assert!(matches!(plan, LoadPlan::Return(value) if value.is_nil()));
}

#[cfg(unix)]
#[test]
fn builtin_load_accepts_raw_unibyte_absolute_filename_values() {
    crate::test_utils::init_test_tracing();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-absolute-raw-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");

    let mut path_bytes = dir.as_os_str().as_bytes().to_vec();
    path_bytes.extend_from_slice(b"/absolute-");
    path_bytes.push(0xFF);
    path_bytes.extend_from_slice(b".el");
    let raw_path = PathBuf::from(std::ffi::OsString::from_vec(path_bytes.clone()));
    fs::write(&raw_path, "(setq vm-load-absolute-raw-ran t)\n").expect("write fixture");

    let mut eval = super::super::eval::Context::new();
    let value = Value::heap_string(crate::heap_types::LispString::from_unibyte(path_bytes));
    let loaded = crate::emacs_core::builtins::builtin_load(&mut eval, vec![value])
        .expect("load should accept raw unibyte absolute filename values");
    assert_eq!(loaded, Value::T);
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-load-absolute-raw-ran")
            .copied(),
        Some(Value::T)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn builtin_load_resolves_raw_unibyte_load_path_entries() {
    crate::test_utils::init_test_tracing();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("neovm-load-path-raw-{unique}"));
    fs::create_dir_all(&root).expect("create temp root");

    let mut dir_bytes = root.as_os_str().as_bytes().to_vec();
    dir_bytes.extend_from_slice(b"/dir-");
    dir_bytes.push(0xFF);
    let raw_dir = PathBuf::from(std::ffi::OsString::from_vec(dir_bytes.clone()));
    fs::create_dir_all(&raw_dir).expect("create raw dir");

    let file = raw_dir.join("probe.el");
    fs::write(&file, "(setq vm-load-raw-load-path-ran t)\n").expect("write fixture");

    let mut eval = super::super::eval::Context::new();
    eval.set_variable(
        "load-path",
        Value::list(vec![Value::heap_string(raw_path_entry(dir_bytes))]),
    );

    let loaded = crate::emacs_core::builtins::builtin_load(&mut eval, vec![Value::string("probe")])
        .expect("load should resolve raw unibyte load-path entries");
    assert_eq!(loaded, Value::T);
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-load-raw-load-path-ran")
            .copied(),
        Some(Value::T)
    );

    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn builtin_load_substitutes_environment_variables_before_search() {
    crate::test_utils::init_test_tracing();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("neovm-load-env-{unique}"));
    fs::create_dir_all(&root).expect("create temp root");

    let mut dir_bytes = root.as_os_str().as_bytes().to_vec();
    dir_bytes.extend_from_slice(b"/env-");
    dir_bytes.push(0xFF);
    let raw_dir = PathBuf::from(std::ffi::OsString::from_vec(dir_bytes.clone()));
    fs::create_dir_all(&raw_dir).expect("create raw dir");

    let file = raw_dir.join("probe.el");
    fs::write(&file, "(setq vm-load-env-ran t)\n").expect("write fixture");

    let env_name = "NEOVM_LOAD_ENV_RAW";
    unsafe {
        std::env::set_var(env_name, std::ffi::OsString::from_vec(dir_bytes.clone()));
    }

    let mut eval = super::super::eval::Context::new();
    let loaded = crate::emacs_core::builtins::builtin_load(
        &mut eval,
        vec![Value::string(format!("${env_name}/probe"))],
    )
    .expect("load should substitute environment variables before search");
    assert_eq!(loaded, Value::T);
    assert_eq!(
        eval.obarray().symbol_value("vm-load-env-ran").copied(),
        Some(Value::T)
    );

    unsafe {
        std::env::remove_var(env_name);
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn builtin_load_dispatches_file_name_handlers_before_search() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    assert_eq!(
        eval_rendered(
            &mut eval,
            r#"
        (setq vm-load-handler-log nil)
        (setq file-name-handler-alist
              (cons (cons "\\`/fake:"
                          (lambda (op &rest args)
                            (setq vm-load-handler-log (cons (cons op args) vm-load-handler-log))
                            'load-sentinel))
                    nil))
        (load "/fake:foo" nil t nil nil)
        "#,
        ),
        "OK load-sentinel"
    );
    assert_eq!(
        eval_rendered(&mut eval, "(car (car vm-load-handler-log))"),
        "OK load"
    );
    assert_eq!(
        eval_rendered(&mut eval, "(car (cdr (car vm-load-handler-log)))"),
        "OK \"/fake:foo\""
    );
}

#[test]
fn find_file_with_suffix_flags() {
    crate::test_utils::init_test_tracing();
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

    let load_path = vec![runtime_path_entry(dir.to_string_lossy().as_ref())];

    // GNU `load-suffixes` is `(".so" ".elc" ".el")` and `find_file_*`
    // tries them in that order, so .elc is preferred over .el. NeoVM
    // matches that since .elc loading was enabled (see commit history
    // for the .elc bootstrap support).
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
    // .elc is preferred over .el (matches GNU load-suffixes order).
    assert_eq!(
        find_file_in_load_path_with_flags("choice", &load_path, false, true, false),
        Some(elc)
    );
    let _el_unused = el;
    // no-suffix takes precedence if both flags are set.
    assert_eq!(
        find_file_in_load_path_with_flags("choice", &load_path, true, true, false),
        Some(plain)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn bootstrap_find_file_prefers_ldefs_boot_over_runtime_loaddefs() {
    crate::test_utils::init_test_tracing();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-bootstrap-ldefs-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");

    let loaddefs = dir.join("loaddefs.el");
    let ldefs_boot = dir.join("ldefs-boot.el");
    fs::write(&loaddefs, "runtime loaddefs").expect("write runtime loaddefs fixture");
    fs::write(&ldefs_boot, "bootstrap ldefs-boot").expect("write bootstrap ldefs fixture");

    let load_path = vec![runtime_path_entry(dir.to_string_lossy().as_ref())];
    assert_eq!(
        find_file_in_load_path_with_flags("loaddefs", &load_path, false, false, false),
        Some(loaddefs.clone())
    );

    {
        let _guard = BootstrapLdefsBootPreferenceGuard::enable();
        assert_eq!(
            find_file_in_load_path_with_flags("loaddefs", &load_path, false, false, false),
            Some(ldefs_boot.clone())
        );
    }

    assert_eq!(
        find_file_in_load_path_with_flags("loaddefs", &load_path, false, false, false),
        Some(loaddefs)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn collect_loaddefs_autoload_args_preserves_raw_unibyte_file_name() {
    crate::test_utils::init_test_tracing();
    let source: String = [
        b'(', b'a', b'u', b't', b'o', b'l', b'o', b'a', b'd', b' ', b'\'', b'r', b'a', b'w', b'-',
        b'f', b'n', b' ', b'"', 0xFF, b'"', b' ', b'n', b'i', b'l', b' ', b't', b')',
    ]
    .into_iter()
    .map(char::from)
    .collect();
    let form = crate::emacs_core::value_reader::read_one_with_source_multibyte(&source, false, 0)
        .expect("parse unibyte autoload form")
        .expect("autoload form should parse")
        .0;

    let mut state = LoaddefsSurfaceState::default();
    collect_loaddefs_autoload_args(form, None, None, &mut state);

    assert_eq!(state.names.len(), 1);
    assert_eq!(state.autoload_args.len(), 1);
    let file = state.autoload_args[0][1]
        .as_lisp_string()
        .expect("autoload file should stay a LispString");
    assert!(!file.is_multibyte());
    assert_eq!(file.as_bytes(), &[0xFF]);
}

#[test]
fn find_file_prefers_earlier_load_path_directory() {
    crate::test_utils::init_test_tracing();
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
        runtime_path_entry(d1.to_string_lossy().as_ref()),
        runtime_path_entry(d2.to_string_lossy().as_ref()),
    ];
    assert_eq!(
        find_file_in_load_path_with_flags("choice", &load_path, false, false, false),
        Some(plain)
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn find_file_prefers_newer_source_when_enabled() {
    crate::test_utils::init_test_tracing();
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

    let load_path = vec![runtime_path_entry(dir.to_string_lossy().as_ref())];
    // GNU's load order is (.so .elc .el), so .elc is preferred over
    // .el when both exist and prefer-newer is off.
    assert_eq!(
        find_file_in_load_path_with_flags("choice", &load_path, false, false, false),
        Some(elc.clone())
    );
    // With prefer-newer=t, the newer source (.el here, written 1s
    // later) wins.
    assert_eq!(
        find_file_in_load_path_with_flags("choice", &load_path, false, false, true),
        Some(el)
    );
    let _elc_unused = elc;

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_file_records_load_history() {
    crate::test_utils::init_test_tracing();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-history-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    fs::write(&file, "(setq vm-load-history-probe t)\n").expect("write fixture");

    let mut eval = super::super::eval::Context::new();
    let loaded = load_file(&mut eval, &file).expect("load file");
    assert_eq!(loaded, Value::T);

    let history = eval
        .obarray()
        .symbol_value("load-history")
        .cloned()
        .unwrap_or(Value::NIL);
    let entries = super::super::value::list_to_vec(&history).expect("load-history is a list");
    assert!(
        !entries.is_empty(),
        "load-history should have at least one entry"
    );
    let first = super::super::value::list_to_vec(&entries[0]).expect("entry is a list");
    let path_str = file.to_string_lossy().to_string();
    assert_eq!(
        first.first().and_then(|v| v.as_utf8_str()),
        Some(path_str.as_str())
    );
    assert_eq!(
        eval.obarray().symbol_value("load-file-name").cloned(),
        Some(Value::NIL)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_file_records_gnu_style_defalias_provide_and_require_history_items() {
    crate::test_utils::init_test_tracing();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-history-defs-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");

    let dep = dir.join("dep.el");
    fs::write(&dep, "(provide 'vm-loadhist-dep)\n").expect("write dependency");

    let main = dir.join("main.el");
    fs::write(
        &main,
        &format!(
            "(require 'vm-loadhist-dep {:?})\n\
         (defalias 'vm-loadhist-main-fn #'ignore)\n\
         (provide 'vm-loadhist-main)\n",
            dep.to_string_lossy().to_string()
        ),
    )
    .expect("write main fixture");

    let mut eval = Context::new();
    let loaded = load_file(&mut eval, &main).expect("load file");
    assert_eq!(loaded, Value::T);

    let history = eval
        .obarray()
        .symbol_value("load-history")
        .cloned()
        .unwrap_or(Value::NIL);
    let entries = list_to_vec(&history).expect("load-history is a list");

    let entry_for = |path: &std::path::Path| {
        entries.iter().find_map(|entry| {
            let items = list_to_vec(entry)?;
            (items.first().and_then(|value| value.as_utf8_str())
                == Some(path.to_string_lossy().as_ref()))
            .then_some(items)
        })
    };

    let dep_entry = entry_for(&dep).expect("dependency load-history entry");
    assert!(dep_entry.iter().skip(1).any(|item| equal_value(
        item,
        &Value::cons(Value::symbol("provide"), Value::symbol("vm-loadhist-dep")),
        0
    )));

    let main_entry = entry_for(&main).expect("main load-history entry");
    assert!(main_entry.iter().skip(1).any(|item| equal_value(
        item,
        &Value::cons(Value::symbol("require"), Value::symbol("vm-loadhist-dep")),
        0
    )));
    assert!(main_entry.iter().skip(1).any(|item| equal_value(
        item,
        &Value::cons(Value::symbol("defun"), Value::symbol("vm-loadhist-main-fn")),
        0
    )));
    assert!(main_entry.iter().skip(1).any(|item| equal_value(
        item,
        &Value::cons(Value::symbol("provide"), Value::symbol("vm-loadhist-main")),
        0
    )));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn builtin_load_uses_hist_file_name_when_purify_flag_is_set() {
    crate::test_utils::init_test_tracing();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-purify-history-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    fs::write(
        &file,
        "(setq vm-purify-load-file-name-seen load-file-name)\n\
         (setq vm-purify-load-true-file-name-seen load-true-file-name)\n\
         (setq vm-purify-current-load-list-seen current-load-list)\n",
    )
    .expect("write fixture");

    let mut eval = super::super::eval::Context::new();
    eval.set_variable(
        "load-path",
        Value::list(vec![Value::string(dir.to_string_lossy().to_string())]),
    );
    eval.set_variable("purify-flag", Value::T);

    let loaded = crate::emacs_core::builtins::builtin_load(&mut eval, vec![Value::string("probe")])
        .expect("load under purify-flag");
    assert_eq!(loaded, Value::T);

    let true_name = file.to_string_lossy().to_string();
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-purify-load-file-name-seen")
            .and_then(|value| value.as_utf8_str()),
        Some("probe.el")
    );
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-purify-load-true-file-name-seen")
            .and_then(|value| value.as_utf8_str()),
        Some(true_name.as_str())
    );

    let current_load_list = eval
        .obarray()
        .symbol_value("vm-purify-current-load-list-seen")
        .cloned()
        .expect("captured current-load-list");
    let current_entries = list_to_vec(&current_load_list).expect("current-load-list is a list");
    assert_eq!(
        current_entries
            .first()
            .and_then(|value| value.as_utf8_str()),
        Some("probe.el")
    );

    let history = eval
        .obarray()
        .symbol_value("load-history")
        .cloned()
        .unwrap_or(Value::NIL);
    let entries = list_to_vec(&history).expect("load-history is a list");
    let first = list_to_vec(&entries[0]).expect("entry is a list");
    assert_eq!(
        first.first().and_then(|value| value.as_utf8_str()),
        Some("probe.el")
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn builtin_load_prepends_history_entry_and_preserves_existing_tail() {
    crate::test_utils::init_test_tracing();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-history-tail-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    fs::write(&file, "(setq vm-load-history-tail-probe t)\n").expect("write fixture");

    let mut eval = super::super::eval::Context::new();
    let old_history = Value::list(vec![Value::list(vec![
        Value::string("/tmp/older.el"),
        Value::symbol("vm-older-probe"),
    ])]);
    eval.set_variable("load-history", old_history);

    let loaded = load_file(&mut eval, &file).expect("load file");
    assert_eq!(loaded, Value::T);

    let history = eval
        .obarray()
        .symbol_value("load-history")
        .cloned()
        .unwrap_or(Value::NIL);
    let entries = list_to_vec(&history).expect("load-history is a list");
    let first = list_to_vec(&entries[0]).expect("entry is a list");
    let path_str = file.to_string_lossy().to_string();
    assert_eq!(
        first.first().and_then(|value| value.as_utf8_str()),
        Some(path_str.as_str())
    );
    assert!(
        crate::emacs_core::value::equal_value(&history.cons_cdr(), &old_history, 0),
        "load should preserve the previous load-history tail",
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_file_exact_gc_roots_load_history_and_after_load_filename() {
    crate::test_utils::init_test_tracing();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-history-exact-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    fs::write(&file, "(setq vm-load-history-probe t)\n").expect("write fixture");

    let mut eval = super::super::eval::Context::new();
    eval.tagged_heap.set_gc_threshold(1);

    eval.eval_str(
        "(progn
           (setq vm-after-load-filename nil)
           (fset 'do-after-load-evaluation
                 (lambda (file)
                   (setq vm-after-load-filename file))))",
    )
    .expect("install do-after-load-evaluation probe");

    let loaded = load_file(&mut eval, &file).expect("load file under exact gc");
    assert_eq!(loaded, Value::T);
    assert!(eval.gc_count > 0, "exact GC should have run during load");

    let history = eval
        .obarray()
        .symbol_value("load-history")
        .cloned()
        .unwrap_or(Value::NIL);
    let entries = super::super::value::list_to_vec(&history).expect("load-history is a list");
    assert!(
        !entries.is_empty(),
        "load-history should have at least one entry"
    );
    let first = super::super::value::list_to_vec(&entries[0]).expect("entry is a list");
    let path_str = file.to_string_lossy().to_string();
    assert_eq!(
        first.first().and_then(|v| v.as_utf8_str()),
        Some(path_str.as_str())
    );
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-after-load-filename")
            .and_then(|v| v.as_utf8_str()),
        Some(path_str.as_str())
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ensure_startup_compat_variables_backfills_xfaces_bootstrap_state() {
    crate::test_utils::init_test_tracing();
    let mut eval = super::super::eval::Context::new();
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
        Some(Value::fixnum(30_000))
    );
    assert_eq!(
        eval.obarray()
            .symbol_value("face-font-lax-matched-attributes")
            .copied(),
        Some(Value::T)
    );
    assert!(
        eval.obarray()
            .symbol_value("system-configuration")
            .is_some_and(|v| v.is_string()),
        "system-configuration should be backfilled to a string"
    );
    assert!(
        eval.obarray()
            .symbol_value("system-configuration-options")
            .is_some_and(|v| v.is_string()),
        "system-configuration-options should be backfilled to a string"
    );
    assert!(
        eval.obarray()
            .symbol_value("system-configuration-features")
            .is_some_and(|v| v.is_string()),
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
        Some(Value::NIL)
    );

    let table = eval
        .obarray()
        .symbol_value("face--new-frame-defaults")
        .copied()
        .expect("face hash table backfilled");
    let ht = table
        .as_hash_table()
        .expect("face--new-frame-defaults must be a hash table");
    assert_eq!(ht.test, HashTableTest::Eq);
    let has_seeded_faces = ht.data.contains_key(&HashKey::Symbol(intern("default")))
        && ht.data.contains_key(&HashKey::Symbol(intern("mode-line")));
    assert!(
        has_seeded_faces,
        "face--new-frame-defaults should be preseeded with GNU face entries"
    );
}

#[test]
fn nested_load_restores_parent_load_file_name() {
    crate::test_utils::init_test_tracing();
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

    let mut eval = super::super::eval::Context::new();
    let loaded = load_file(&mut eval, &parent).expect("load parent fixture");
    assert_eq!(loaded, Value::T);

    let parent_str = parent.to_string_lossy().to_string();
    let child_str = child.to_string_lossy().to_string();
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-parent-seen")
            .and_then(|v| v.as_utf8_str()),
        Some(parent_str.as_str())
    );
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-child-seen")
            .and_then(|v| v.as_utf8_str()),
        Some(child_str.as_str())
    );
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-parent-after-child")
            .and_then(|v| v.as_utf8_str()),
        Some(parent_str.as_str())
    );
    assert_eq!(
        eval.obarray().symbol_value("load-file-name").cloned(),
        Some(Value::NIL),
        "load-file-name should be restored after top-level load",
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn nested_load_exact_gc_preserves_reader_load_file_name() {
    crate::test_utils::init_test_tracing();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-file-name-reader-exact-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let parent = dir.join("parent.el");
    let child = dir.join("child.el");

    fs::write(
        &parent,
        "(setq vm-parent-reader-before #$)\n\
         (load (expand-file-name \"child\" (file-name-directory load-file-name)) nil 'nomessage)\n\
         (setq vm-parent-reader-after #$)\n",
    )
    .expect("write parent fixture");
    fs::write(
        &child,
        "(setq vm-child-reader #$)\n\
         (setq vm-child-junk (make-list 20000 nil))\n",
    )
    .expect("write child fixture");

    let mut eval = super::super::eval::Context::new();
    eval.gc_stress = true;

    let loaded = load_file(&mut eval, &parent).expect("load nested fixture under exact gc");
    assert_eq!(loaded, Value::T);
    assert!(
        eval.gc_count > 0,
        "exact GC should have run during nested load"
    );

    let parent_str = parent.to_string_lossy().to_string();
    let child_str = child.to_string_lossy().to_string();
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-parent-reader-before")
            .and_then(|v| v.as_utf8_str()),
        Some(parent_str.as_str())
    );
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-child-reader")
            .and_then(|v| v.as_utf8_str()),
        Some(child_str.as_str())
    );
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-parent-reader-after")
            .and_then(|v| v.as_utf8_str()),
        Some(parent_str.as_str())
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_file_binds_load_true_file_name_and_current_load_list() {
    crate::test_utils::init_test_tracing();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-true-file-name-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    fs::write(
        &file,
        "(setq vm-load-true-file-name-seen load-true-file-name)\n\
         (setq vm-current-load-list-seen current-load-list)\n",
    )
    .expect("write fixture");

    let mut eval = super::super::eval::Context::new();
    let old_load_true_file = eval.obarray().symbol_value("load-true-file-name").cloned();
    let old_current_load_list = eval.obarray().symbol_value("current-load-list").cloned();

    let loaded = load_file(&mut eval, &file).expect("load fixture");
    assert_eq!(loaded, Value::T);

    let file_str = file.to_string_lossy().to_string();
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-load-true-file-name-seen")
            .and_then(|v| v.as_utf8_str()),
        Some(file_str.as_str())
    );

    let current_load_list = eval
        .obarray()
        .symbol_value("vm-current-load-list-seen")
        .copied()
        .expect("load should capture current-load-list");
    let entries = list_to_vec(&current_load_list).expect("current-load-list should be a list");
    let first = entries
        .first()
        .copied()
        .expect("current-load-list should contain the filename");
    assert_eq!(first.as_utf8_str(), Some(file_str.as_str()));

    assert_eq!(
        eval.obarray().symbol_value("load-true-file-name").cloned(),
        old_load_true_file.or(Some(Value::NIL)),
        "load-true-file-name should be restored after top-level load",
    );
    assert_eq!(
        eval.obarray().symbol_value("current-load-list").cloned(),
        old_current_load_list,
        "current-load-list should be restored after top-level load",
    );

    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn builtin_load_file_accepts_raw_unibyte_filename_values() {
    crate::test_utils::init_test_tracing();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-file-raw-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");

    let mut path_bytes = dir.as_os_str().as_bytes().to_vec();
    path_bytes.extend_from_slice(b"/raw-");
    path_bytes.push(0xFF);
    path_bytes.extend_from_slice(b".el");
    let raw_path = PathBuf::from(std::ffi::OsString::from_vec(path_bytes.clone()));
    fs::write(&raw_path, "(setq vm-load-file-raw-ran t)\n").expect("write fixture");

    let mut eval = super::super::eval::Context::new();
    let value = Value::heap_string(crate::heap_types::LispString::from_unibyte(path_bytes));
    let loaded = crate::emacs_core::builtins::builtin_load_file(&mut eval, vec![value])
        .expect("load-file should accept raw unibyte filename values");
    assert_eq!(loaded, Value::T);
    assert_eq!(
        eval.obarray().symbol_value("vm-load-file-raw-ran").copied(),
        Some(Value::T)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_file_accepts_shebang_and_honors_second_line_lexical_binding_cookie() {
    crate::test_utils::init_test_tracing();
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

    let mut eval = super::super::eval::Context::new();
    let loaded = load_file(&mut eval, &file).expect("load shebang fixture");
    assert_eq!(loaded, Value::T);
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-load-shebang-probe")
            .cloned(),
        Some(Value::T),
        "second-line lexical-binding cookie should set lexical-binding to t during load",
    );

    let value = eval
        .eval_str("(let ((lexical-binding nil)) (funcall vm-load-shebang-fn))")
        .expect("evaluate closure");
    assert_eq!(
        value.as_int(),
        Some(42),
        "closure should capture lexical scope from loaded file",
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_file_does_not_enable_lexical_binding_from_non_cookie_second_line_text() {
    crate::test_utils::init_test_tracing();
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

    let mut eval = super::super::eval::Context::new();
    let loaded = load_file(&mut eval, &file).expect("load shebang non-cookie fixture");
    assert_eq!(loaded, Value::T);
    assert_eq!(
        eval.obarray()
            .symbol_value("vm-load-shebang-false-probe")
            .cloned(),
        Some(Value::NIL),
        "non-cookie second-line text must not flip lexical-binding to t",
    );

    let value = eval
        .eval_str("(condition-case err (let ((lexical-binding nil)) (funcall vm-load-shebang-false-fn)) (error (list 'error (car err))))")
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
    crate::test_utils::init_test_tracing();
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

    let mut eval = super::super::eval::Context::new();
    let loaded = load_file(&mut eval, &file).expect("load bom fixture");
    assert_eq!(loaded, Value::T);
    assert_eq!(
        eval.obarray().symbol_value("vm-load-bom-probe").cloned(),
        Some(Value::symbol("ok")),
        "utf-8 bom should be ignored by reader before first form",
    );
    assert_eq!(
        eval.obarray().symbol_value("vm-load-bom-flag").cloned(),
        Some(Value::T)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_file_single_line_shebang_signals_end_of_file() {
    crate::test_utils::init_test_tracing();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-shebang-eof-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let file = dir.join("probe.el");
    fs::write(&file, "#!/usr/bin/env emacs --script").expect("write shebang-only fixture");

    let mut eval = super::super::eval::Context::new();
    let err = load_file(&mut eval, &file).expect_err("shebang-only source should signal EOF");
    match err {
        EvalError::Signal { symbol, data, .. } => {
            assert_eq!(resolve_sym(symbol), "end-of-file");
            assert!(data.is_empty());
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_elc_is_supported() {
    crate::test_utils::init_test_tracing();
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

    let mut eval = super::super::eval::Context::new();
    let result = load_file(&mut eval, &compiled);
    assert!(
        result.is_ok(),
        "load should accept .elc: {:?}",
        result.err()
    );
    assert_eq!(
        eval.obarray().symbol_value("vm-elc-loaded").cloned(),
        Some(Value::T),
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_elc_preserves_unibyte_reader_literals() {
    crate::test_utils::init_test_tracing();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-elc-unibyte-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let compiled = dir.join("probe.elc");

    let mut content = b"(setq vm-elc-raw \"".to_vec();
    content.push(0xFF);
    content.extend_from_slice(b"\")\n(setq vm-elc-char ?");
    content.push(0xFF);
    content.extend_from_slice(b")\n");
    fs::write(&compiled, content).expect("write compiled fixture");

    let mut eval = super::super::eval::Context::new();
    load_file(&mut eval, &compiled).expect("load unibyte .elc fixture");

    let raw = eval
        .obarray()
        .symbol_value("vm-elc-raw")
        .copied()
        .expect("load should set vm-elc-raw");
    let text = raw
        .as_lisp_string()
        .expect("vm-elc-raw should be a LispString");
    assert!(!text.is_multibyte());
    assert_eq!(text.as_bytes(), &[0xFF]);
    assert_eq!(
        eval.obarray().symbol_value("vm-elc-char").cloned(),
        Some(Value::fixnum(255))
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_elc_gz_is_rejected() {
    crate::test_utils::init_test_tracing();
    // .elc.gz files are still unsupported.
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-elcgz-rejected-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let compiled = dir.join("probe.elc.gz");
    fs::write(&compiled, "gzipped-data").expect("write compiled fixture");

    let mut eval = super::super::eval::Context::new();
    let err = load_file(&mut eval, &compiled).expect_err("load should reject .elc.gz");
    match err {
        EvalError::Signal { symbol, .. } => assert_eq!(resolve_sym(symbol), "error"),
        other => panic!("unexpected error: {other:?}"),
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn find_file_finds_elc_only_artifact_after_elc_loading_enabled() {
    crate::test_utils::init_test_tracing();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-elc-only-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");

    let compiled = dir.join("module.elc");
    fs::write(&compiled, "compiled").expect("write compiled fixture");

    let load_path = vec![runtime_path_entry(dir.to_string_lossy().as_ref())];
    // With .elc loading enabled, an .elc-only artifact resolves
    // directly via the GNU `load-suffixes` order ((.so .elc .el)).
    let found = find_file_in_load_path_with_flags("module", &load_path, false, false, false);
    assert_eq!(found, Some(compiled));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_elc_gz_is_explicitly_unsupported() {
    crate::test_utils::init_test_tracing();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("neovm-load-elc-gz-unsupported-{unique}"));
    fs::create_dir_all(&dir).expect("create temp fixture dir");
    let compiled = dir.join("probe.elc.gz");
    fs::write(&compiled, "compiled-data").expect("write compiled fixture");

    let mut eval = super::super::eval::Context::new();
    let err = load_file(&mut eval, &compiled).expect_err("load should reject .elc.gz");
    match err {
        EvalError::Signal { symbol, .. } => assert_eq!(resolve_sym(symbol), "error"),
        other => panic!("unexpected error: {other:?}"),
    }

    let _ = fs::remove_dir_all(&dir);
}

/// Try loading the full loadup.el file sequence through the NeoVM
/// evaluator.  This test runs by default.  Set
/// NEOVM_LOADUP_TEST_SKIP=1 to skip it.
///
/// Probes test state directly via `(get SYM 'cl--class)` — which is
/// exactly what compiled .elc code executes when it calls the
/// `cl--find-class` macro. This avoids depending on whether `cl-macs`
/// (a compile-time-only library) has been loaded at runtime: GNU's
/// own bootstrap doesn't load it either, and raw calls to
/// `(cl--find-class ...)` at the top level signal void-function unless
/// user code has done `(require 'cl-macs)` first. See the
/// contribution notes on test authoring re: macro dependencies.
#[test]
fn neovm_loadup_bootstrap() {
    crate::test_utils::init_test_tracing();
    if std::env::var("NEOVM_LOADUP_TEST_SKIP").as_deref() == Ok("1") {
        tracing::info!("skipping neovm_loadup_bootstrap (NEOVM_LOADUP_TEST_SKIP=1)");
        return;
    }

    crate::test_utils::init_test_tracing();

    let mut eval = create_bootstrap_evaluator().expect("loadup bootstrap should succeed");
    let result = eval
        .eval_str(
            "(list (not (null (get 'float 'cl--class))) (not (null (get 'integer 'cl--class))))",
        )
        .expect("evaluate cl class probe");
    let items = crate::emacs_core::value::list_to_vec(&result).expect("result list");
    assert_eq!(
        items,
        vec![Value::T, Value::T],
        "expected float/integer CL classes to be registered, got {result}"
    );

    let float_pred = eval.obarray().get_property("float", "cl-deftype-satisfies");
    let integer_pred = eval
        .obarray()
        .get_property("integer", "cl-deftype-satisfies");
    assert!(
        float_pred.is_some_and(|v| !v.is_nil()),
        "expected float cl-deftype-satisfies property to be non-nil, got {float_pred:?}"
    );
    assert!(
        integer_pred.is_some_and(|v| !v.is_nil()),
        "expected integer cl-deftype-satisfies property to be non-nil, got {integer_pred:?}"
    );

    let compat_result = eval
        .eval_str("(list (coding-system-p 'iso-8859-15) (stringp system-configuration-features))")
        .expect("evaluate startup compatibility probe");
    let compat_items =
        crate::emacs_core::value::list_to_vec(&compat_result).expect("compat probe result list");
    assert_eq!(
        compat_items,
        vec![Value::T, Value::T],
        "expected iso-8859-15 and system-configuration-features to be available, got {compat_result}"
    );
}

#[test]
fn compiled_bootstrap_cl_preload_stubs_work_after_faces() {
    crate::test_utils::init_test_tracing();
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
        for result in eval.eval_str_each(&stub) {
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
fn deftheme_and_provide_theme_works() {
    crate::test_utils::init_test_tracing();

    let mut eval = create_bootstrap_evaluator().expect("bootstrap");

    // Test: deftheme + provide-theme should provide the THEME-theme feature
    let result =
        eval.eval_str("(progn (deftheme test-neovm \"Test\") (provide-theme 'test-neovm))");
    eprintln!("deftheme+provide-theme result: {:?}", result);

    let provided = eval.eval_str("(featurep 'test-neovm-theme)").unwrap();
    eprintln!("(featurep 'test-neovm-theme) = {:?}", provided);
    assert!(
        provided.is_truthy(),
        "provide-theme should provide the THEME-theme feature"
    );
}

#[test]
fn eval_after_load_defines_function_on_provide() {
    crate::test_utils::init_test_tracing();

    let mut eval = create_bootstrap_evaluator().expect("bootstrap");

    // 1. Register eval-after-load (like use-package does)
    eval.eval_str("(eval-after-load 'test-pkg (lambda () (defun test-pkg-fn () 42)))")
        .expect("eval-after-load should succeed");

    // 2. test-pkg-fn should NOT be defined yet
    let before = eval
        .obarray()
        .symbol_function("test-pkg-fn")
        .is_some_and(|f| !f.is_nil());
    assert!(!before, "should NOT be defined before provide");

    // 3. Simulate provide DURING file loading (load-file-name is set).
    // GNU's eval-after-load adds an after-load-functions hook in this case,
    // so the callback must still be deferred immediately after provide.
    let mid = eval
        .eval_str(
            r#"(let ((load-file-name "/tmp/test-pkg.el"))
                 (provide 'test-pkg)
                 (fboundp 'test-pkg-fn))"#,
        )
        .expect("provide during simulated load should succeed");
    assert!(
        mid.is_nil(),
        "feature callback should remain deferred until do-after-load-evaluation"
    );

    // 4. Simulate do-after-load-evaluation (runs after-load-functions)
    eval.eval_str(
        "(when (fboundp 'do-after-load-evaluation)
           (do-after-load-evaluation \"/tmp/test-pkg.el\"))",
    )
    .expect("do-after-load-evaluation should succeed");

    // 5. NOW test-pkg-fn should be defined
    let after = eval
        .obarray()
        .symbol_function("test-pkg-fn")
        .is_some_and(|f| !f.is_nil());
    assert!(
        after,
        "should be defined after do-after-load-evaluation runs after-load-functions"
    );
}

#[test]
fn defface_warning_creates_face_after_bootstrap() {
    crate::test_utils::init_test_tracing();

    let mut eval = create_bootstrap_evaluator().expect("bootstrap");

    // Check: is 'warning a valid face after bootstrap?
    let result = eval
        .eval_str("(facep 'warning)")
        .expect("facep should work");
    eprintln!("(facep 'warning) = {:?}", result);
    assert!(
        result.is_truthy(),
        "'warning' should be a valid face after bootstrap (defined in faces.el)"
    );
}

#[test]
fn add_hook_preserves_uninterned_symbol_callable_object() {
    crate::test_utils::init_test_tracing();

    let mut eval = create_bootstrap_evaluator().expect("bootstrap");
    let result = eval
        .eval_str(
            r#"(progn
                 (defvar test-hook nil)
                 (let ((fun (make-symbol "test-helper")))
                   (fset fun (lambda (x) (+ x 1)))
                   (add-hook 'test-hook fun)
                   (let ((stored (car test-hook)))
                     (list (eq stored fun)
                           (functionp stored)
                           (funcall stored 41)))))"#,
        )
        .expect("bootstrap add-hook should preserve uninterned callable symbol");

    assert_eq!(
        result,
        Value::list(vec![Value::T, Value::T, Value::fixnum(42)]),
        "bootstrap add-hook should preserve the exact uninterned function object"
    );
}

#[test]
fn direct_hook_runtime_accepts_bootstrap_uninterned_symbol_hook_members() {
    crate::test_utils::init_test_tracing();

    let mut eval = create_bootstrap_evaluator().expect("bootstrap");
    eval.eval_str(
        r#"(progn
             (defvar test-hook nil)
             (let ((fun (make-symbol "test-helper")))
               (fset fun (lambda (x) (set 'test-hook-result x)))
               (add-hook 'test-hook fun)))"#,
    )
    .expect("bootstrap add-hook setup should work");

    crate::emacs_core::hook_runtime::run_named_hook_with_args(
        &mut eval,
        &[Value::symbol("test-hook"), Value::fixnum(42)],
    )
    .expect("direct hook runtime should run uninterned hook symbol");

    assert_eq!(
        eval.obarray().symbol_value("test-hook-result").copied(),
        Some(Value::fixnum(42)),
        "direct hook runtime should funcall uninterned hook members"
    );
}

#[test]
fn bootstrap_run_hook_with_args_keeps_builtin_dispatch_surface() {
    crate::test_utils::init_test_tracing();

    let mut eval = create_bootstrap_evaluator().expect("bootstrap");
    let result = eval
        .eval_str(
            r#"(let ((overrides (if (boundp 'internal--compiler-function-overrides)
                                   internal--compiler-function-overrides
                                 nil)))
                 (list
                  (subrp (symbol-function 'run-hook-with-args))
                  (subrp (indirect-function 'run-hook-with-args))
                  (assq 'run-hook-with-args overrides)))"#,
        )
        .expect("bootstrap run-hook-with-args surface should be inspectable");

    assert_eq!(
        result,
        Value::list(vec![Value::T, Value::T, Value::NIL]),
        "bootstrap should leave run-hook-with-args on the builtin subr surface"
    );
}

#[test]
fn bootstrap_lisp_run_hook_with_args_accepts_uninterned_symbol_after_setup_eval() {
    crate::test_utils::init_test_tracing();

    let mut eval = create_bootstrap_evaluator().expect("bootstrap");
    eval.eval_str(
        r#"(progn
             (defvar test-hook nil)
             (let ((fun (make-symbol "test-helper")))
               (fset fun (lambda (x) (set 'test-hook-result x)))
               (add-hook 'test-hook fun)))"#,
    )
    .expect("bootstrap setup should work");

    eval.eval_str("(run-hook-with-args 'test-hook 42)")
        .expect("separate bootstrap run-hook-with-args call should work");

    assert_eq!(
        eval.obarray().symbol_value("test-hook-result").copied(),
        Some(Value::fixnum(42)),
        "separate Lisp eval should still funcall uninterned hook members"
    );
}

#[test]
fn uninterned_symbol_in_hook_works() {
    crate::test_utils::init_test_tracing();

    let mut eval = create_bootstrap_evaluator().expect("bootstrap");

    // Test: add-hook with uninterned symbol, then run-hook-with-args
    eval.eval_str(
        r#"(progn
           (defvar test-hook nil)
           (let ((fun (make-symbol "test-helper")))
             (fset fun (lambda (x) (set 'test-hook-result x)))
             (add-hook 'test-hook fun))
           (run-hook-with-args 'test-hook 42))"#,
    )
    .expect("hook with uninterned symbol should work");

    let result = eval.obarray().symbol_value("test-hook-result").cloned();
    assert!(
        result.is_some_and(|v| v == Value::fixnum(42)),
        "hook with uninterned symbol should fire"
    );
}

#[test]
fn defun_inside_lambda_works() {
    crate::test_utils::init_test_tracing();

    let mut eval = create_bootstrap_evaluator().expect("bootstrap");

    // Test: defun inside a lambda should define globally
    eval.eval_str("(let ((fn (lambda () (defun test-fn-from-lambda () 42)))) (funcall fn))")
        .expect("funcall lambda with defun");

    let defined = eval
        .obarray()
        .symbol_function("test-fn-from-lambda")
        .is_some_and(|f| !f.is_nil());
    eprintln!("test-fn-from-lambda defined={}", defined);
    assert!(
        defined,
        "defun inside lambda should define function globally"
    );
}

#[test]
fn elc_loading_defines_defcustom_variables() {
    crate::test_utils::init_test_tracing();
    let general_elc = std::path::Path::new(
        "/home/exec/.config/emacs/.local/straight/build-31.0.50/general/general.elc",
    );
    if !general_elc.exists() {
        eprintln!("skipping: general.elc not found");
        return;
    }

    crate::test_utils::init_test_tracing();

    let mut eval = create_bootstrap_evaluator().expect("bootstrap");

    // Load general.elc
    let result = super::load_file(&mut eval, general_elc);
    assert!(
        result.is_ok(),
        "general.elc should load without error: {:?}",
        result.err()
    );

    // Check that general-default-states is defined (defcustom)
    let bound = eval
        .obarray()
        .symbol_value("general-default-states")
        .is_some();
    let special = eval.obarray().is_special("general-default-states");
    eprintln!("general-default-states: bound={bound}, special={special}");

    // Check other variables from general.elc
    for var in [
        "general-implicit-kbd",
        "general-keybindings",
        "general-override-mode",
        "general-override-mode-map",
        "general-default-prefix",
        "general-default-keymaps",
    ] {
        let b = eval.obarray().symbol_value(var).is_some();
        let s = eval.obarray().is_special(var);
        let fbound = eval.obarray().symbol_function(var).is_some();
        eprintln!("  {var}: bound={b}, special={s}, fbound={fbound}");
    }

    // Check if custom-declare-variable is fboundp
    let cdv = eval
        .obarray()
        .symbol_function("custom-declare-variable")
        .is_some();
    eprintln!("custom-declare-variable fboundp={cdv}");

    // Check that general feature was provided
    let provided = eval.eval_str("(featurep 'general)");
    eprintln!("(featurep 'general) = {:?}", provided);

    // Test Form 0 in the same evaluator using the streaming Value reader
    let raw_bytes = std::fs::read(general_elc).unwrap();
    let content = super::skip_elc_header(&raw_bytes);
    let (form0, _next_pos) = crate::emacs_core::value_reader::read_one(&content, 0)
        .expect("read first form")
        .expect("EOF before first form");
    eprintln!("Read Form 0 from general.elc via value reader");

    let result = eval
        .eval_sub(form0)
        .map_err(crate::emacs_core::error::map_flow);
    eprintln!("Form 0 result: {:?}", result);

    let gds_bound = eval
        .obarray()
        .symbol_value("general-default-states")
        .is_some();
    let gik_bound = eval
        .obarray()
        .symbol_value("general-implicit-kbd")
        .is_some();
    eprintln!(
        "After Form 0: general-default-states bound={gds_bound}, general-implicit-kbd bound={gik_bound}"
    );

    assert!(
        gds_bound,
        "general-default-states should be bound after Form 0 bytecode"
    );
}

#[test]
fn source_cl_lib_loads_after_early_gv_without_bootstrap_gv_stubs() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
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

    let result = eval
        .eval_str("(fboundp 'built-in-class--make)")
        .expect("evaluate built-in-class constructor probe");
    assert_eq!(result, Value::T);
}

#[test]
fn compiled_custom_declare_face_call_before_faces_succeeds() {
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("faces", true);
    let rendered = eval_rendered(
        &mut eval,
        r#"(condition-case err
               (progn
                 (put 'vm-debug-face 'face-defface-spec t)
                 (custom-declare-face 'vm-debug-face '((t nil)) "Debug doc." :group 'basic-faces)
                 (get 'vm-debug-face 'face-defface-spec))
             (error err))"#,
    );
    assert_eq!(rendered, "OK t");
}

#[test]
fn source_cycle_spacing_form_loads_after_bootstrap_prefix() {
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("simple", false);
    let load_path = get_load_path(&eval.obarray());
    let path = bootstrap_fixture_path(&load_path, "simple", false).expect("simple.el path");
    let content = std::fs::read_to_string(&path).expect("read simple.el");
    let forms = crate::emacs_core::value_reader::read_all(&content).expect("parse simple.el");

    let cycle_spacing_form = forms
        .get(89)
        .copied()
        .expect("cycle-spacing source bootstrap form");
    let printed = crate::emacs_core::print::print_value(&cycle_spacing_form);
    assert!(
        printed.starts_with("(defun cycle-spacing"),
        "unexpected simple.el FORM[89]: {}",
        printed
    );

    let subset_source = format!(
        ";;; cycle-spacing-subset.el --- focused bootstrap slice -*- lexical-binding: t; -*-\n\n{}\n\n{}\n\n{}\n",
        crate::emacs_core::print::print_value(&forms[87]),
        crate::emacs_core::print::print_value(&forms[88]),
        crate::emacs_core::print::print_value(&forms[89]),
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

    let result = eval
        .eval_str("(list (boundp 'cycle-spacing--context) (fboundp 'cycle-spacing))")
        .expect("evaluate cycle-spacing probe");
    assert_eq!(result, Value::list(vec![Value::T, Value::T]));
}

#[test]
fn partial_bootstrap_footer_local_variables_with_empty_suffix_do_not_error() {
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("emacs-lisp/macroexp", false);
    let rendered = eval_rendered(
        &mut eval,
        r#"(with-current-buffer (get-buffer-create " *footer-local-vars*")
             (erase-buffer)
             (insert ";;; footer-local-vars.el --- focused footer locals -*- lexical-binding: t; -*-\n\n"
                     "(setq footer-local-vars-test t)\n\n"
                     ";; Local Variables:\n"
                     ";; no-byte-compile: t\n"
                     ";; version-control: never\n"
                     ";; no-update-autoloads: t\n"
                     ";; End:\n")
             (setq buffer-file-name "/tmp/footer-local-vars.el")
             (setq default-directory "/tmp/")
             (condition-case err
                 (list 'ok (hack-local-variables 'no-mode))
               (error (list 'error (car err) (cdr err)))))"#,
    );

    assert_eq!(rendered, "OK (ok nil)");
}

#[test]
fn partial_bootstrap_with_demoted_errors_swallows_footer_local_variables_error() {
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("emacs-lisp/macroexp", false);
    let rendered = eval_rendered(
        &mut eval,
        r#"(with-current-buffer (get-buffer-create " *footer-local-vars-demoted*")
             (erase-buffer)
             (insert ";;; footer-local-vars.el --- focused footer locals -*- lexical-binding: t; -*-\n\n"
                     "(setq footer-local-vars-test t)\n\n"
                     ";; Local Variables:\n"
                     ";; no-byte-compile: t\n"
                     ";; version-control: never\n"
                     ";; no-update-autoloads: t\n"
                     ";; End:\n")
             (setq buffer-file-name "/tmp/footer-local-vars.el")
             (setq default-directory "/tmp/")
             (with-demoted-errors "File local-variables error: %s"
               (hack-local-variables 'no-mode)))"#,
    );

    assert_eq!(rendered, "OK nil");
}

#[test]
fn partial_bootstrap_load_with_code_conversion_swallows_footer_local_variables_error() {
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("emacs-lisp/macroexp", false);
    eval.set_variable(
        "load-source-file-function",
        Value::symbol("load-with-code-conversion"),
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("footer-local-vars-load.el");
    fs::write(
        &path,
        ";;; footer-local-vars-load.el --- focused footer locals -*- lexical-binding: t; -*-\n\n\
         (setq footer-local-vars-load-test t)\n\n\
         ;; Local Variables:\n\
         ;; no-byte-compile: t\n\
         ;; version-control: never\n\
         ;; no-update-autoloads: t\n\
         ;; End:\n",
    )
    .expect("write footer local vars load fixture");

    let result = load_file(&mut eval, &path);
    assert_eq!(
        format_eval_result(&result),
        "OK t",
        "source load path should demote footer local variable parse errors"
    );
}

#[test]
fn partial_bootstrap_looking_back_matches_empty_suffix_at_line_end() {
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("emacs-lisp/macroexp", false);
    let rendered = eval_rendered(
        &mut eval,
        r#"(with-current-buffer (get-buffer-create " *looking-back-eol*")
             (erase-buffer)
             (insert ";; no-byte-compile: t\n")
             (goto-char (point-min))
             (end-of-line)
             (list (looking-back "$" (line-beginning-position))
                   (looking-back "" (line-beginning-position))
                   (looking-back "t$" (line-beginning-position))
                   (looking-back "t" (line-beginning-position))))"#,
    );

    assert_eq!(rendered, "OK (t t t t)");
}

#[test]
fn compiled_characters_loads_after_case_table() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();

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
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("keymap", false);
    let result = eval
        .eval_str(
            r#"(let ((cmd 'neovm--test-prefix-map))
             (define-prefix-command cmd nil "Test Prefix")
             (list (eq cmd 'neovm--test-prefix-map)
                   (keymapp (symbol-function cmd))
                   (keymapp (symbol-value cmd))))"#,
        )
        .expect("evaluate define-prefix-command probe");
    assert_eq!(
        crate::emacs_core::value::list_to_vec(&result).expect("probe result list"),
        vec![Value::T, Value::T, Value::T]
    );
}

#[test]
fn lookup_key_returned_submenu_symbol_has_bound_value() {
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("keymap", false);
    let result = eval
        .eval_str(
            r#"(let* ((root (make-sparse-keymap))
                  (submenu 'describe-chinese-environment-map))
             (define-prefix-command submenu nil "Chinese Environment")
             (define-key-after root (vector 'Chinese) (cons "Chinese" submenu))
             (let ((found (lookup-key root [Chinese])))
               (list (eq found submenu)
                     (keymapp (symbol-value found)))))"#,
        )
        .expect("evaluate lookup-key submenu probe");
    assert_eq!(
        crate::emacs_core::value::list_to_vec(&result).expect("probe result list"),
        vec![Value::T, Value::T]
    );
}

#[test]
fn set_language_info_alist_reuses_chinese_submenu_like_gnu_emacs() {
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("language/chinese", false);
    let result = eval
        .eval_str(
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
        .expect("evaluate set-language-info-alist submenu probe");
    assert_eq!(result, Value::T);
}

#[test]
fn bootstrap_load_sequence_includes_gnu_x_term_layer_after_tool_bar() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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

    let result = eval
        .eval_str(
            r#"(with-temp-buffer
             (insert "Enable the mode if ARG is nil, omitted, or is a positive number.\n")
             (insert "Disable the mode if ARG is a negative number.\n")
             (let ((to (copy-marker (point) t)))
               (fill-delete-newlines (point-min) to 'left t nil)
               (buffer-string)))"#,
        )
        .expect("evaluation succeeds");

    assert_eq!(
        format_eval_result(&Ok(result)),
        r#"OK "Enable the mode if ARG is nil, omitted, or is a positive number.  Disable the mode if ARG is a negative number. ""#
    );
}

#[test]
fn bootstrap_tool_bar_mode_comes_from_gnu_mode_macro_path() {
    crate::test_utils::init_test_tracing();

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
        crate::emacs_core::value_reader::read_all(&source).expect("parse tool-bar source");
    // GNU `readevalloop` keeps each form live while it is being expanded and
    // evaluated. This probe pre-parses the whole file, so root those forms
    // across the later bootstrap/helper evaluation that can trigger GC.
    let top_level_forms_root_scope = eval.save_specpdl_roots();
    for form in &top_level_forms {
        eval.push_specpdl_root(*form);
    }
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
        tracing::info!("tool-bar probe: helper {}", label);
        let value = eval.eval_str(src).unwrap_or_else(|err| {
            panic!(
                "failed evaluating tool-bar helper {label} from {}: {}",
                tool_bar_path.display(),
                format_eval_error(&eval, &err)
            )
        });
        let rendered = crate::emacs_core::print::print_value_with_buffers(&value, &eval.buffers);
        tracing::info!("tool-bar probe: helper {} => {}", label, rendered);
    }
    tracing::info!("tool-bar probe: macroexpand form 1");
    let expanded = eval
        .eval_str(
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
        .expect("macroexpand tool-bar define-minor-mode");
    tracing::info!("tool-bar probe: macroexpand complete");
    if let Some(forms) = list_to_vec(&expanded) {
        if forms.first().map_or(false, |v| v.is_symbol_named("progn")) {
            for (idx, form) in forms.iter().enumerate().skip(1) {
                tracing::info!("tool-bar probe: eval expanded subform {}", idx);
                eval.eval_form(*form).unwrap_or_else(|err| {
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
    let found = load_path_lisp_string(&tool_bar_path);
    let lexical_binding = source_lexical_binding_for_load(
        &mut eval,
        &source,
        Some(Value::heap_string(found.clone())),
    )
    .expect("tool-bar lexical-binding cookie");
    with_load_context(&mut eval, &found, &found, lexical_binding, |eval| {
        for (idx, form) in top_level_forms.iter().enumerate().skip(1) {
            tracing::info!("tool-bar probe: eval top-level form {}", idx + 1);
            eval.eval_form(*form).unwrap_or_else(|err| {
                panic!(
                    "failed evaluating tool-bar form {} from {}: {}",
                    idx + 1,
                    tool_bar_path.display(),
                    format_eval_error(eval, &err)
                )
            });
        }
        Ok(Value::NIL)
    })
    .expect("evaluate tool-bar forms under load context");
    eval.restore_specpdl_roots(top_level_forms_root_scope);
    tracing::info!("tool-bar probe: load complete");
    let result = eval
        .eval_str(
            r#"(list
             (special-form-p 'define-minor-mode)
             (commandp 'tool-bar-mode)
             (not (and (consp (symbol-function 'tool-bar-mode))
                       (eq (car (symbol-function 'tool-bar-mode)) 'autoload)))
             (keymapp tool-bar-map))"#,
        )
        .expect("evaluate tool-bar bootstrap probe");
    assert_eq!(
        result,
        Value::list(vec![Value::NIL, Value::T, Value::T, Value::T])
    );
}

#[test]
fn evaluator_bootstrap_binds_default_frame_scroll_bars_like_gnu_frame_c() {
    crate::test_utils::init_test_tracing();
    let eval = Context::new();
    assert_eq!(
        eval.obarray.symbol_value("default-frame-scroll-bars"),
        Some(&Value::symbol("right"))
    );
}

#[test]
fn auth_source_backend_exposes_type_slot() {
    crate::test_utils::init_test_tracing();

    let mut eval =
        create_bootstrap_evaluator_cached_with_features(&["neomacs"]).expect("bootstrap evaluator");
    eval.eval_str(r#"(load "subdirs" nil t)"#)
        .expect("load runtime subdirs.el");
    let require_error = eval
        .require_value(Value::symbol("auth-source"), None, None)
        .err();

    let result = eval.eval_str("(let ((backend (make-instance 'auth-source-backend :type 'netrc :source \"test\")))
\
           (list (slot-value backend 'type)
\
                 (slot-value backend 'source)
\
                 (mapcar #'cl--slot-descriptor-name
\
                         (eieio-class-slots (eieio-object-class backend)))))").unwrap_or_else(|err| {
        panic!(
            "evaluate auth-source backend slot probe failed after require_error={require_error:?}: {err:?}"
        )
    });
    let items = crate::emacs_core::value::list_to_vec(&result).expect("probe result list");
    assert_eq!(items.first().copied(), Some(Value::symbol("netrc")));
    assert_eq!(items.get(1).and_then(|v| v.as_utf8_str()), Some("test"));

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
    match value.kind() {
        ValueKind::Veclike(VecLikeType::Vector) => value
            .as_vector_data()
            .unwrap()
            .clone()
            .iter()
            .map(|item| match item.kind() {
                ValueKind::Fixnum(n) => n,
                other => panic!("expected int in vector, got {other:?}"),
            })
            .collect(),
        other => panic!("expected vector, got {other:?}"),
    }
}

#[test]
fn cl_callf_updates_variable_place() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    // `cl-callf` is defined in cl-macs.el, which GNU's bootstrap does
    // not preload (compiled .elc files have macro calls already
    // expanded). Any runtime eval of uncompiled `(cl-callf ...)`
    // requires a `(require 'cl-macs)` first, matching real-user
    // usage at the top level.
    let result = eval
        .eval_str(
            r#"(progn
             (require 'cl-macs)
             (let ((a '(3 2 1)))
               (cl-callf (lambda (slots) (apply #'vector (nreverse slots))) a)
               a))"#,
        )
        .expect("evaluate cl-callf variable probe");
    assert_eq!(expect_vector_ints(result), vec![1, 2, 3]);
}

#[test]
fn direct_setq_funcall_updates_variable_place() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    let result = eval
        .eval_str(
            r#"(let ((a '(3 2 1)))
           (setq a (funcall #'(lambda (slots) (apply #'vector (nreverse slots))) a))
           a)"#,
        )
        .expect("evaluate direct funcall probe");
    assert_eq!(expect_vector_ints(result), vec![1, 2, 3]);
}

#[test]
fn pdump_roundtrip_preserves_advice_remove_member_lifecycle() {
    crate::test_utils::init_test_tracing();
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
        let value = loaded.eval_str(form).expect("evaluate step");
        if let Some(expected) = expected {
            let rendered =
                crate::emacs_core::print::print_value_with_buffers(&value, &loaded.buffers);
            assert_eq!(rendered, expected, "unexpected result at step {label}");
        }
    }
}

#[test]
fn pdump_roundtrip_evaluates_full_advice_remove_member_form() {
    crate::test_utils::init_test_tracing();
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

    let value = loaded.eval_str(r#"(progn
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
        (fmakunbound 'neovm--adv-fn3b)))"#).expect("evaluate full form");
    let rendered = crate::emacs_core::print::print_value_with_buffers(&value, &loaded.buffers);
    assert_eq!(rendered, "(nil t t nil t nil)");
}

#[test]
fn cached_bootstrap_reload_evaluates_full_advice_remove_member_form() {
    crate::test_utils::init_test_tracing();
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
    let fresh_value = fresh.eval_str(form_source).expect("fresh form eval");
    assert_eq!(
        crate::emacs_core::print::print_value_with_buffers(&fresh_value, &fresh.buffers),
        "(nil t t nil t nil)"
    );
    drop(fresh);

    let mut loaded = create_bootstrap_evaluator_cached_at_path(&[], &dump_path)
        .expect("loaded cached bootstrap");
    apply_runtime_startup_state(&mut loaded).expect("loaded runtime startup");
    let loaded_value = loaded.eval_str(form_source).expect("loaded form eval");
    assert_eq!(
        crate::emacs_core::print::print_value_with_buffers(&loaded_value, &loaded.buffers),
        "(nil t t nil t nil)"
    );
}

#[test]
fn runtime_startup_state_matches_char_syntax_comprehensive_form() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let result = eval
        .eval_str(
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
        .expect("evaluate char syntax comprehensive probe");
    assert_eq!(
        crate::emacs_core::print::print_value_with_buffers(&result, &eval.buffers),
        "(119 119 119 119 95 32 32 62 40 41 40 41 95 95 95 39 60 34 95 95 95 95 39 (119 119 119 40))"
    );
}

#[test]
fn bootstrap_eieio_core_preserves_accessor_compiler_macro() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
fn bootstrap_macroexpand_all_pop_body_before_faces() {
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("faces", true);
    let rendered = eval_rendered(
        &mut eval,
        r#"
(condition-case err
    (macroexpand-all
     '(let ((tail spec))
        (let* ((entry (pop tail))
               (display (car entry))
               (attrs (cdr entry))
               thisval)
          (setq thisval
                (if (null (cdr attrs))
                    (car attrs)
                  attrs))
          thisval)))
  (error (list 'error err)))
"#,
    );

    assert!(
        rendered.starts_with("OK "),
        "bootstrap macroexpand-all on pop body should succeed before faces.el, got: {rendered}"
    );
}

#[test]
fn bootstrap_macroexpand_all_real_face_spec_choose_body_before_faces() {
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("faces", true);
    let rendered = eval_rendered(
        &mut eval,
        r#"
(condition-case err
    (macroexpand-all
     '(progn
        (unless frame
          (setq frame (selected-frame)))
        (let ((tail spec)
              result defaults match-found)
          (while tail
            (let* ((entry (pop tail))
                   (display (car entry))
                   (attrs (cdr entry))
                   thisval)
              (setq thisval
                    (if (null (cdr attrs))
                        (car attrs)
                      attrs))
              (if (eq display 'default)
                  (setq defaults thisval)
                (when (face-spec-set-match-display display frame)
                  (setq result thisval
                        tail nil
                        match-found t)))))
          (if defaults
              (append defaults result)
            (if match-found
                result
              no-match-retval)))))
  (error (list 'error err)))
"#,
    );

    assert!(
        rendered.starts_with("OK "),
        "bootstrap macroexpand-all on real face-spec-choose body should succeed before faces.el, got: {rendered}"
    );
}

#[test]
fn bootstrap_define_inline_sets_compiler_macro_properties() {
    crate::test_utils::init_test_tracing();
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
fn expanded_cache_replay_preserves_oclosure_define_class_registration() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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

    assert_eq!(rendered, "OK (t t t (let* ((v class)) (aset v 5 idx)))");
}

#[test]
fn bootstrap_eieio_core_accessor_compiler_macro_properties_visible() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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

    assert_eq!(rendered, "OK (progn (aref class 5))");
}

#[test]
fn bootstrap_runtime_funcall_interactively_marks_backtrace_frame() {
    crate::test_utils::init_test_tracing();
    let mut eval = isolated_runtime_bootstrap_eval();

    let rendered = eval_rendered(
        &mut eval,
        r#"
(progn
  (defun neovm--bt-marker-target ()
    (interactive)
    (nth 1 (backtrace-frame 1 'neovm--bt-marker-target)))
  (unwind-protect
      (list
       (funcall-interactively 'neovm--bt-marker-target)
       (call-interactively 'neovm--bt-marker-target))
    (fmakunbound 'neovm--bt-marker-target)))
"#,
    );

    assert_eq!(rendered, "OK (funcall-interactively funcall-interactively)");
}

#[test]
fn bootstrap_runtime_advice_preserves_called_interactively_stack_behavior() {
    crate::test_utils::init_test_tracing();
    let mut eval = isolated_runtime_bootstrap_eval();

    let rendered = eval_rendered(
        &mut eval,
        r#"
(progn
  (defun neovm--advice-ci-target ()
    (interactive)
    (list (called-interactively-p 'any)
          (called-interactively-p 'interactive)))
  (defun neovm--advice-ci-around (orig &rest args)
    (apply orig args))
  (advice-add 'neovm--advice-ci-target :around 'neovm--advice-ci-around)
  (unwind-protect
      (list
       (funcall-interactively 'neovm--advice-ci-target)
       (call-interactively 'neovm--advice-ci-target))
    (advice-remove 'neovm--advice-ci-target 'neovm--advice-ci-around)
    (fmakunbound 'neovm--advice-ci-around)
    (fmakunbound 'neovm--advice-ci-target)))
"#,
    );

    assert_eq!(rendered, "OK ((nil nil) (nil nil))");
}

#[test]
fn bootstrap_runtime_around_advice_preserves_advice_stack_shape() {
    crate::test_utils::init_test_tracing();
    let mut eval = isolated_runtime_bootstrap_eval();

    let rendered = eval_rendered(
        &mut eval,
        r#"
(progn
  (defun neovm--advice-stack-target ()
    (interactive)
    (list 'target
          (called-interactively-p 'any)
          (called-interactively-p 'interactive)
          (nth 1 (backtrace-frame 1 'neovm--advice-stack-target))))
  (defun neovm--advice-stack-around (orig &rest args)
    (list 'around-enter
          (called-interactively-p 'any)
          (called-interactively-p 'interactive)
          (nth 1 (backtrace-frame 1 'neovm--advice-stack-around))
          (apply orig args)))
  (advice-add 'neovm--advice-stack-target :around 'neovm--advice-stack-around)
  (unwind-protect
      (list
       (funcall-interactively 'neovm--advice-stack-target)
       (call-interactively 'neovm--advice-stack-target))
    (advice-remove 'neovm--advice-stack-target 'neovm--advice-stack-around)
    (fmakunbound 'neovm--advice-stack-around)
    (fmakunbound 'neovm--advice-stack-target)))
"#,
    );

    assert_eq!(
        rendered,
        "OK ((around-enter t nil apply (target nil nil funcall-interactively)) (around-enter t nil apply (target nil nil funcall-interactively)))"
    );
}

#[test]
fn bootstrap_runtime_before_advice_preserves_advice_stack_shape() {
    crate::test_utils::init_test_tracing();
    let mut eval = isolated_runtime_bootstrap_eval();

    let rendered = eval_rendered(
        &mut eval,
        r#"
(progn
  (defvar neovm--advice-stack-before-result nil)
  (defun neovm--advice-stack-target ()
    (interactive)
    (list 'target
          (called-interactively-p 'any)
          (called-interactively-p 'interactive)
          (nth 1 (backtrace-frame 1 'neovm--advice-stack-target))))
  (defun neovm--advice-stack-before (&rest _args)
    (setq neovm--advice-stack-before-result
          (list 'before
                (called-interactively-p 'any)
                (called-interactively-p 'interactive)
                (nth 1 (backtrace-frame 1 'neovm--advice-stack-before)))))
  (advice-add 'neovm--advice-stack-target :before 'neovm--advice-stack-before)
  (unwind-protect
      (list
       (list
        (funcall-interactively 'neovm--advice-stack-target)
        neovm--advice-stack-before-result)
       (progn
         (setq neovm--advice-stack-before-result nil)
         (list
          (call-interactively 'neovm--advice-stack-target)
          neovm--advice-stack-before-result)))
    (advice-remove 'neovm--advice-stack-target 'neovm--advice-stack-before)
    (fmakunbound 'neovm--advice-stack-before)
    (fmakunbound 'neovm--advice-stack-target)
    (makunbound 'neovm--advice-stack-before-result)))
"#,
    );

    assert_eq!(
        rendered,
        "OK (((target t nil funcall-interactively) (before t nil apply)) ((target t nil funcall-interactively) (before t nil apply)))"
    );
}

#[test]
fn runtime_add_function_and_advice_mapc_on_symbol_function_place() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"
(progn
  (defun neovm--place-target (x)
    (list 'target x))
  (defun neovm--place-around (orig x)
    (list 'around (funcall orig x)))
  (unwind-protect
      (progn
        (add-function :around (symbol-function 'neovm--place-target)
                      #'neovm--place-around
                      '((name . neovm-place-around) (depth . -50)))
        (list
         (neovm--place-target 1)
         (let (seen)
           (advice-mapc
            (lambda (f props)
              (push (list (functionp f)
                          (cdr (assq 'name props))
                          (cdr (assq 'depth props)))
                    seen))
            'neovm--place-target)
           (nreverse seen))
         (progn
           (remove-function (symbol-function 'neovm--place-target)
                            'neovm-place-around)
           (neovm--place-target 2))))
    (ignore-errors
      (remove-function (symbol-function 'neovm--place-target)
                       'neovm-place-around))
    (fmakunbound 'neovm--place-around)
    (fmakunbound 'neovm--place-target)))
"#,
    );

    assert_eq!(
        rendered,
        "OK ((around (target 1)) ((t neovm-place-around -50)) (target 2))"
    );
}

#[test]
fn runtime_add_function_on_local_place() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"
(progn
  (defvar neovm--local-place-fn nil)
  (setq-default neovm--local-place-fn
                (lambda (x) (list 'global x)))
  (defun neovm--local-place-around (orig x)
    (list 'local-around (funcall orig x)))
  (let ((other (get-buffer-create " *neovm-advice-other*")))
    (unwind-protect
        (with-temp-buffer
          (setq-local neovm--local-place-fn
                      (lambda (x) (list 'local x)))
          (add-function :around (local 'neovm--local-place-fn)
                        #'neovm--local-place-around)
          (list
           (funcall neovm--local-place-fn 1)
           (with-current-buffer other
             (funcall neovm--local-place-fn 2))
           (progn
             (remove-function (local 'neovm--local-place-fn)
                              #'neovm--local-place-around)
             (funcall neovm--local-place-fn 3))))
      (when (buffer-live-p other)
        (kill-buffer other))
      (makunbound 'neovm--local-place-fn)
      (fmakunbound 'neovm--local-place-around))))
"#,
    );

    assert_eq!(
        rendered,
        "OK ((local-around (local 1)) (global 2) (local 3))"
    );
}

#[test]
fn runtime_add_function_on_process_filter_place() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"
(progn
  (defun neovm--proc-filter-around (orig proc string)
    (list 'filter string (null (funcall orig proc string))))
  (let ((p (make-pipe-process :name "neovm-adv-filter")))
    (unwind-protect
        (progn
          (add-function :around (process-filter p)
                        #'neovm--proc-filter-around)
          (list
           (funcall (process-filter p) p "chunk")
           (progn
             (remove-function (process-filter p)
                              #'neovm--proc-filter-around)
             (funcall (process-filter p) p "chunk"))))
      (ignore-errors (delete-process p))
      (fmakunbound 'neovm--proc-filter-around))))
"#,
    );

    assert_eq!(rendered, "OK ((filter \"chunk\" t) nil)");
}

#[test]
fn runtime_add_function_on_process_sentinel_place() {
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    let rendered = eval_rendered(
        &mut eval,
        r#"
(progn
  (defun neovm--proc-sentinel-around (orig proc string)
    (list 'sentinel string (null (funcall orig proc string))))
  (let ((p (make-pipe-process :name "neovm-adv-sentinel")))
    (unwind-protect
        (progn
          (add-function :around (process-sentinel p)
                        #'neovm--proc-sentinel-around)
          (list
           (funcall (process-sentinel p) p "done")
           (progn
             (remove-function (process-sentinel p)
                              #'neovm--proc-sentinel-around)
             (funcall (process-sentinel p) p "done"))))
      (ignore-errors (delete-process p))
      (fmakunbound 'neovm--proc-sentinel-around))))
"#,
    );

    assert_eq!(rendered, "OK ((sentinel \"done\" t) nil)");
}

#[test]
fn bootstrap_cl_extra_source_vs_compiled_cl_subseq_setf() {
    crate::test_utils::init_test_tracing();
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
fn bootstrap_cl_extra_gv_expander_matches_gnu_source_and_compiled_surfaces() {
    crate::test_utils::init_test_tracing();
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
         (setter-t
          (let ((v 'placeholder-seq))
            (condition-case err
                (eval setter-form t)
              (error (car err)))))
         (setter-lex
          (let ((v 'placeholder-seq))
            (condition-case err
                (eval setter-form lexical-binding)
              (error (car err)))))
         (setter-env
          (condition-case err
              (eval setter-form '((v . placeholder-seq)))
            (error (car err)))))
    (list direct
          setter-t
          setter-lex
          (functionp setter-env)
          (closurep setter-env))))
"#;

    let source_rendered = cached_bootstrap_eval_with_loaded_file(&source_path, form);
    assert_eq!(
        source_rendered,
        "OK (invalid-function void-variable void-variable t t)"
    );

    // The checked-in compiled artifact currently surfaces
    // `(void-function gv--defsetter)` under both GNU Emacs and NeoVM.
    if compiled_path.exists() {
        let compiled_rendered = cached_bootstrap_eval_with_loaded_file(&compiled_path, form);
        assert_eq!(compiled_rendered, "ERR (void-function gv--defsetter)");
    }
}

#[test]
fn bootstrap_load_file_defun_gv_setter_declaration_evaluates_generated_form() {
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
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
    crate::test_utils::init_test_tracing();
    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap evaluator");
    // See cl_callf_updates_variable_place: `cl-callf` is a cl-macs macro
    // not preloaded at bootstrap — require it explicitly.
    let result = eval
        .eval_str(
            r#"(progn
             (require 'cl-macs)
             (let ((box (list '(3 2 1))))
               (cl-callf (lambda (slots) (apply #'vector (nreverse slots))) (car box))
               (car box)))"#,
        )
        .expect("evaluate cl-callf generalized place probe");
    assert_eq!(expect_vector_ints(result), vec![1, 2, 3]);
}

/// Minimal test: load enough files to get macroexpand-all + pcase working,
/// then try (macroexpand-all '(pcase x (1 "one") (2 "two"))) and see
/// if it terminates.
#[test]
fn macroexpand_all_pcase_terminates() {
    crate::test_utils::init_test_tracing();
    if std::env::var("NEOVM_LOADUP_TEST").as_deref() != Ok("1") {
        tracing::info!("skipping (set NEOVM_LOADUP_TEST=1)");
        return;
    }
    crate::test_utils::init_test_tracing();
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("root");
    let lisp_dir = project_root.join("lisp");
    assert!(lisp_dir.is_dir());
    let mut eval = crate::emacs_core::eval::Context::new();
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
    eval.set_variable("purify-flag", Value::NIL);
    eval.set_variable("max-lisp-eval-depth", Value::fixnum(1600));

    let load_path = get_load_path(&eval.obarray());
    let load_and_report = |eval: &mut crate::emacs_core::eval::Context,
                           name: &str,
                           load_path: &[crate::heap_types::LispString]| {
        let path = find_file_in_load_path(name, load_path).expect(name);
        load_file(eval, &path).unwrap_or_else(|e| {
            let msg = match &e {
                EvalError::Signal { symbol, data, .. } => {
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
    let form_value = crate::emacs_core::value_reader::read_all(test_form)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let mexp_fn = eval
        .obarray()
        .symbol_function("internal-macroexpand-for-load");
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
    match eval.eval_str(test_form2) {
        Ok(v) => tracing::debug!("  pcase backquote OK: {v}"),
        Err(e) => tracing::debug!("  pcase backquote ERR: {e:?}"),
    }

    tracing::debug!("All macroexpand-all pcase tests completed");
}

#[test]
fn macroexp_eager_reload_preserves_symbol_identity() {
    crate::test_utils::init_test_tracing();
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("root");
    let lisp_dir = project_root.join("lisp");
    assert!(lisp_dir.is_dir());

    let mut eval = crate::emacs_core::eval::Context::new();
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
    eval.set_variable("purify-flag", Value::NIL);
    eval.set_variable(
        "macroexp--pending-eager-loads",
        Value::list(vec![Value::symbol("skip")]),
    );

    let load_path = get_load_path(&eval.obarray());
    let load = |eval: &mut crate::emacs_core::eval::Context, name: &str| {
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

    let probe_result = eval
        .eval_str(
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
        .expect("evaluate symbol identity probe");
    let values =
        crate::emacs_core::value::list_to_vec(&probe_result).expect("probe should return list");
    assert_eq!(values, vec![Value::NIL, Value::NIL, Value::NIL, Value::T]);

    eval.set_variable("macroexp--pending-eager-loads", Value::NIL);
    load(&mut eval, "emacs-lisp/macroexp");
}

#[test]
fn eager_expand_toplevel_forms_keeps_recursive_progn_forms_alive_under_exact_gc() {
    crate::test_utils::init_test_tracing();

    let mut eval = crate::emacs_core::eval::Context::new();
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("root");
    let lisp_dir = project_root.join("lisp");
    assert!(lisp_dir.is_dir());
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
    eval.set_variable("purify-flag", Value::NIL);
    eval.set_variable("max-lisp-eval-depth", Value::fixnum(1600));

    let load_path = get_load_path(&eval.obarray());
    let load_and_report = |eval: &mut crate::emacs_core::eval::Context,
                           name: &str,
                           load_path: &[crate::heap_types::LispString]| {
        let path = find_file_in_load_path(name, load_path).expect(name);
        load_file(eval, &path).unwrap_or_else(|e| {
            let msg = match &e {
                EvalError::Signal { symbol, data, .. } => {
                    let sym = crate::emacs_core::intern::resolve_sym(*symbol);
                    let data_strs: Vec<String> = data.iter().map(|v| format!("{v}")).collect();
                    format!("({sym} {})", data_strs.join(" "))
                }
                other => format!("{other:?}"),
            };
            panic!("Failed to load {name}: {msg}");
        });
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

    eval.eval_str(
        r#"(defmacro neomacs-test-progn-macro ()
             '(progn
                (defvar neomacs-test-progn-var 42)
                (defun neomacs-test-progn-fn ()
                  neomacs-test-progn-var)))"#,
    )
    .expect("define progn macro");

    let form_value = crate::emacs_core::value_reader::read_all("(neomacs-test-progn-macro)")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let macroexpand_fn = get_eager_macroexpand_fn(&eval).expect("eager macroexpand fn");
    let mut expanded = Vec::new();
    eager_expand_toplevel_forms(
        &mut eval,
        form_value,
        macroexpand_fn,
        &mut |ctx, _original, form, _requires_eager_replay| {
            expanded.push(crate::emacs_core::print::print_value_with_buffers(
                &form,
                &ctx.buffers,
            ));
            let scope = ctx.save_specpdl_roots();
            ctx.push_specpdl_root(form);
            ctx.gc_collect_exact();
            ctx.restore_specpdl_roots(scope);
            ctx.eval_value(&form).map_err(map_flow)
        },
    )
    .expect("eager expand progn macro");

    assert_eq!(
        expanded,
        vec![
            "(defvar neomacs-test-progn-var 42)".to_string(),
            "(defalias 'neomacs-test-progn-fn #'(lambda nil neomacs-test-progn-var))".to_string(),
        ]
    );

    let result = eval
        .eval_str("(neomacs-test-progn-fn)")
        .expect("call progn fn");
    assert_eq!(result, Value::fixnum(42));
}

#[test]
fn function_get_only_exposes_cxxr_compiler_macro_on_cxxr_symbols() {
    crate::test_utils::init_test_tracing();
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("root");
    let lisp_dir = project_root.join("lisp");
    assert!(lisp_dir.is_dir());

    let mut eval = crate::emacs_core::eval::Context::new();
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

    let result = eval
        .eval_str(
            r#"(list (if (function-get 'car 'compiler-macro) t nil)
                 (if (function-get 'cdr 'compiler-macro) t nil)
                 (if (function-get 'cadr 'compiler-macro) t nil))"#,
        )
        .expect("evaluate function-get probe");
    assert_eq!(
        crate::emacs_core::value::list_to_vec(&result).expect("probe should return list"),
        vec![Value::NIL, Value::NIL, Value::T]
    );
}

/// Test pcase with integer literal patterns — reproduces the
/// "Unknown pattern '32'" error from rx.el line 1284.
#[test]
fn pcase_integer_literal_pattern() {
    crate::test_utils::init_test_tracing();
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("root");
    let lisp_dir = project_root.join("lisp");
    assert!(lisp_dir.is_dir());
    let mut eval = crate::emacs_core::eval::Context::new();
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
    eval.set_variable("purify-flag", Value::NIL);
    eval.set_variable("max-lisp-eval-depth", Value::fixnum(1600));

    let load_path = get_load_path(&eval.obarray());
    let load_and_report = |eval: &mut crate::emacs_core::eval::Context,
                           name: &str,
                           load_path: &[crate::heap_types::LispString]| {
        let path = find_file_in_load_path(name, load_path).expect(name);
        load_file(eval, &path).unwrap_or_else(|e| {
            let msg = match &e {
                EvalError::Signal { symbol, data, .. } => {
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
    match eval.eval_str(r#"(pcase 32 (32 "matched") (_ "no-match"))"#) {
        Ok(v) => tracing::info!("  Test 1 OK: {v}"),
        Err(e) => tracing::error!("  Test 1 FAILED: {e:?}"),
    }

    // Test 2: (or 'sym int) pattern — exact pattern from rx.el:1284
    tracing::info!("Test 2: pcase with (or 'sym int) — rx.el pattern");
    match eval.eval_str(r#"(pcase ?\s ((or '\? ?\s) "matched") (_ "no-match"))"#) {
        Ok(v) => tracing::info!("  Test 2 OK: {v}"),
        Err(e) => tracing::error!("  Test 2 FAILED: {e:?}"),
    }

    // Test 3: (or int int) pattern
    tracing::info!("Test 3: pcase with (or int int)");
    match eval.eval_str(r#"(pcase 32 ((or 32 63) "matched") (_ "no-match"))"#) {
        Ok(v) => tracing::info!("  Test 3 OK: {v}"),
        Err(e) => tracing::error!("  Test 3 FAILED: {e:?}"),
    }

    // Test 4: pcase inside a defun then call it (simulates rx--translate-form)
    tracing::info!("Test 4: pcase inside defun");
    match eval.eval_str(
        r#"(progn
      (defun test-pcase-int (x)
        (pcase x
          ((or '\? ?\s) "question-or-space")
          ('seq "seq")
          (_ "other")))
      (list (test-pcase-int 'seq)
            (test-pcase-int ?\s)
            (test-pcase-int '\?)
            (test-pcase-int 'foo)))"#,
    ) {
        Ok(v) => tracing::info!("  Test 4 OK: {v}"),
        Err(e) => tracing::error!("  Test 4 FAILED: {e:?}"),
    }

    // Test 5: get the actual error message
    tracing::info!("Test 5: capture error message from (or 'sym int)");
    match eval.eval_str(
        r#"(condition-case err
        (pcase ?\s ((or '\? ?\s) "matched") (_ "no-match"))
      (error (error-message-string err)))"#,
    ) {
        Ok(v) => tracing::info!("  Test 5 result: {v}"),
        Err(e) => tracing::error!("  Test 5 FAILED: {e:?}"),
    }

    // Test 6: (or 'sym 'sym) — should work fine
    tracing::info!("Test 6: (or 'sym 'sym)");
    match eval.eval_str(r#"(pcase 'foo ((or 'foo 'bar) "matched") (_ "no"))"#) {
        Ok(v) => tracing::info!("  Test 6 OK: {v}"),
        Err(e) => tracing::error!("  Test 6 FAILED: {e:?}"),
    }

    // Test 7: (or int 'sym) — reversed order
    tracing::info!("Test 7: (or int 'sym) — reversed");
    match eval.eval_str(r#"(pcase 32 ((or 32 'foo) "matched") (_ "no"))"#) {
        Ok(v) => tracing::info!("  Test 7 OK: {v}"),
        Err(e) => tracing::error!("  Test 7 FAILED: {e:?}"),
    }

    // Test 8: just macroexpand the problematic form
    tracing::info!("Test 8: macroexpand-1 the (or 'sym int) pcase");
    match eval.eval_str(r#"(macroexpand '(pcase x ((or '\? 32) "yes") (_ "no")))"#) {
        Ok(v) => tracing::info!("  Test 8 expansion: {v}"),
        Err(e) => tracing::error!("  Test 8 FAILED: {e:?}"),
    }

    // Test 9: check what pcase--macroexpand does with integer
    tracing::info!("Test 9: pcase--macroexpand on raw integer");
    match eval.eval_str(r#"(pcase--macroexpand 32)"#) {
        Ok(v) => tracing::info!("  Test 9 result: {v}"),
        Err(e) => tracing::error!("  Test 9 FAILED: {e:?}"),
    }

    // Test 10: check pcase--self-quoting-p
    tracing::info!("Test 10: pcase--self-quoting-p 32");
    match eval.eval_str(r#"(pcase--self-quoting-p 32)"#) {
        Ok(v) => tracing::info!("  Test 10 result: {v}"),
        Err(e) => tracing::error!("  Test 10 FAILED: {e:?}"),
    }

    tracing::info!("pcase integer literal tests completed");
}

#[test]
fn key_parse_modifier_bits() {
    crate::test_utils::init_test_tracing();

    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = manifest.parent().expect("project root");
    let lisp_dir = project_root.join("lisp");
    if !lisp_dir.is_dir() {
        tracing::info!("skipping key_parse_modifier_bits: no lisp/ directory");
        return;
    }

    let mut eval = crate::emacs_core::eval::Context::new();

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
    eval.set_variable("purify-flag", Value::NIL);

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
        match eval.eval_str(expr_str) {
            Ok(val) => tracing::debug!("  OK: {desc}: {expr_str} => {val}"),
            Err(e) => {
                let msg = match &e {
                    EvalError::Signal { symbol, data, .. } => {
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
    let result = eval.eval_str("(key-parse \"C-x\")");
    match &result {
        Err(EvalError::Signal { symbol, data, .. }) => {
            let sym = super::super::intern::resolve_sym(*symbol);
            let data_strs: Vec<String> = data.iter().map(|v| format!("{v}")).collect();
            panic!("key-parse \"C-x\" failed: ({sym} {})", data_strs.join(" "));
        }
        Err(e) => panic!("key-parse \"C-x\" failed: {e:?}"),
        Ok(val) => tracing::debug!("key-parse \"C-x\" => {val}"),
    }
}

#[test]
fn generated_loaddefs_replays_metadata_forms_on_bootstrap_runtime_surface() {
    crate::test_utils::init_test_tracing();
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

    let mut eval = create_bootstrap_evaluator_cached().expect("bootstrap");
    apply_runtime_startup_state(&mut eval).expect("runtime startup state");

    load_file(&mut eval, &file).unwrap_or_else(|err| {
        panic!(
            "generated loaddefs should load: {}",
            format_eval_error(&eval, &err)
        )
    });

    let autoload = eval
        .obarray()
        .symbol_function("vm-generated-fn")
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
            Value::NIL,
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
fn bootstrap_cl_generic_generalizers_t() {
    crate::test_utils::init_test_tracing();
    // Load up to AND INCLUDING cl-generic.el (stops before "simple")
    // This triggers the exact FORM[90] failure.
    // To isolate, we load up to simple which includes cl-generic.
    // The test will fail at cl-generic.el FORM[90] if the bug exists.
    let mut eval = partial_bootstrap_eval_until("simple", true);
    // If we got here, cl-generic.el loaded successfully!
    let result = eval.eval_str_each("(cl-generic-generalizers t)");
    let rendered = result
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>()
        .join(" ");
    tracing::info!("(cl-generic-generalizers t) => {rendered}");
    assert!(
        rendered.starts_with("OK"),
        "(cl-generic-generalizers t) should succeed, got: {rendered}"
    );
}

#[test]
fn bootstrap_macroexpand_all_pcase() {
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("emacs-lisp/cl-generic", true);
    // Test 1: simple pcase
    let result =
        eval.eval_str_each(r#"(macroexpand-all '(pcase x (1 "one") (2 "two") (_ "other")))"#);
    let rendered = result
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>()
        .join(" ");
    tracing::info!("macroexpand-all pcase => {rendered}");
    assert!(
        rendered.starts_with("OK"),
        "macroexpand-all pcase failed: {rendered}"
    );

    // Test 2: pcase with backquote patterns (like cl-typep uses)
    let result2 = eval
        .eval_str_each(r#"(macroexpand-all '(pcase val (`(,x) (list 'single x)) (_ 'default)))"#);
    let rendered2 = result2
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>()
        .join(" ");
    tracing::info!("macroexpand-all pcase backquote => {rendered2}");
    assert!(
        rendered2.starts_with("OK"),
        "macroexpand-all pcase backquote failed: {rendered2}"
    );
}

#[test]
fn bootstrap_macroexpand_functions_are_compiled() {
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("emacs-lisp/cl-generic", true);
    let result = eval.eval_str_each(
        r#"
(list
  (compiled-function-p (symbol-function 'macroexpand-all))
  (compiled-function-p (symbol-function 'internal-macroexpand-for-load)))
"#,
    );
    let rendered = result
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>()
        .join(" ");
    tracing::info!("compiled macroexpand functions => {rendered}");
    assert_eq!(
        rendered, "OK (t t)",
        "bootstrap macroexpand functions should be compiled, got: {rendered}"
    );
}

#[test]
fn bootstrap_load_uniquify_after_float_sup() {
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("uniquify", true);
    let load_path = get_load_path(&eval.obarray());
    let path = bootstrap_fixture_path(&load_path, "uniquify", true)
        .expect("bootstrap file not found: uniquify");
    load_file(&mut eval, &path).unwrap_or_else(|err| {
        panic!(
            "failed loading uniquify from {}: {}",
            path.display(),
            format_eval_error(&eval, &err)
        )
    });
}

#[test]
fn bootstrap_macroexpand_all_pcase_and_pred() {
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("emacs-lisp/cl-preloaded", true);
    // Test macroexpand-all on the same pcase pattern
    let result = eval.eval_str_each(
        r#"
(macroexpand-all
 '(pcase val
    ((and type (pred symbolp))
     (if (get type 'test-prop) (list 'found type) 'no-prop))
    (_ 'default)))
"#,
    );
    let rendered = result
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>()
        .join(" ");
    tracing::info!("macroexpand-all pcase and+pred => {rendered}");
    assert!(
        rendered.starts_with("OK"),
        "macroexpand-all pcase and+pred failed: {rendered}"
    );
}

#[test]
fn bootstrap_pcase_complex_and_pred_guard() {
    crate::test_utils::init_test_tracing();
    // Load enough to have pcase (stop before cl-preloaded to avoid cl-macs failure)
    let mut eval = partial_bootstrap_eval_until("emacs-lisp/cl-preloaded", true);
    // Test the exact pcase pattern that cl-typep uses
    let result = eval.eval_str_each(
        r#"
(progn
  (put 'integer 'test-prop t)
  (let ((test-fn (lambda (val)
                   (pcase val
                     ((and type (pred symbolp))
                      (if (get type 'test-prop) (list 'found type) 'no-prop))
                     (_ 'default)))))
    (list
     (funcall test-fn 'integer)
     (funcall test-fn 42))))
"#,
    );
    let rendered = result
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>()
        .join(" ");
    tracing::info!("pcase and+pred+guard => {rendered}");
    assert!(
        rendered.starts_with("OK"),
        "pcase and+pred+guard failed: {rendered}"
    );
}

#[test]
fn bootstrap_macroexpand1_vs_all_pcase() {
    crate::test_utils::init_test_tracing();
    let mut eval = partial_bootstrap_eval_until("emacs-lisp/cl-preloaded", true);
    // Get macroexpand-1 result and macroexpand-all error as strings
    let result = eval.eval_str_each(
        r#"
(list
  (prin1-to-string
    (condition-case err
      (macroexpand-1 '(pcase val
        ((and type (pred symbolp)) (list 'found type))
        (_ 'default)))
      (error (list 'expand1-error err))))
  (prin1-to-string
    (condition-case err
      (macroexpand-all '(pcase val
        ((and type (pred symbolp)) (list 'found type))
        (_ 'default)))
      (error (list 'expand-all-error err)))))
"#,
    );
    let rendered = result
        .iter()
        .map(format_eval_result)
        .collect::<Vec<_>>()
        .join(" ");
    tracing::error!("macroexpand1 vs all => {rendered}");
    assert!(
        rendered.starts_with("OK"),
        "macroexpand comparison failed: {rendered}"
    );
}
