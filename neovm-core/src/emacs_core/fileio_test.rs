use super::*;
use crate::emacs_core::eval::Context;
use crate::emacs_core::format_eval_result;
use crate::emacs_core::value::list_to_vec;
use crate::test_utils::runtime_startup_eval_all;
use std::io::Write;

fn bootstrap_eval(src: &str) -> Vec<String> {
    runtime_startup_eval_all(src)
}

thread_local! {
    /// Keep ALL test contexts alive across a single #[test] so that
    /// heap-backed return values from earlier `call_fileio_builtin!`
    /// invocations remain valid when later assertions inspect them.
    /// Previously this stored only the *last* context, which freed
    /// earlier strings and produced use-after-free panics in tests
    /// that compared results across multiple builtin calls.
    static LAST_TEST_CTX: std::cell::RefCell<Vec<Context>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

macro_rules! call_fileio_builtin {
    ($builtin:ident, $args:expr) => {{
        let mut eval = Context::new();
        let result = $builtin(&mut eval, $args);
        LAST_TEST_CTX.with(|slot| slot.borrow_mut().push(eval));
        result
    }};
}

#[cfg(unix)]
fn assert_same_file_paths(path1: &str, path2: &str) {
    use std::os::unix::fs::MetadataExt;

    let meta1 = fs::metadata(path1).expect("metadata path1");
    let meta2 = fs::metadata(path2).expect("metadata path2");
    assert_eq!(meta1.dev(), meta2.dev());
    assert_eq!(meta1.ino(), meta2.ino());
}

#[cfg(not(unix))]
fn assert_same_file_paths(path1: &str, path2: &str) {
    assert_eq!(
        fs::read(path1).expect("read path1"),
        fs::read(path2).expect("read path2")
    );
}

#[test]
fn temporary_file_directory_for_eval_accepts_raw_unibyte_string() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let raw = Value::heap_string(crate::heap_types::LispString::from_unibyte(
        b"/tmp/neomacs-\xFF".to_vec(),
    ));
    eval.obarray
        .set_symbol_value("temporary-file-directory", raw);

    assert_eq!(
        temporary_file_directory_for_eval(&eval),
        Some(crate::emacs_core::builtins::lisp_string_to_runtime_string(
            raw
        ))
    );
}

#[test]
fn make_auto_save_file_name_accepts_raw_unibyte_prefix_directory() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let raw = Value::heap_string(crate::heap_types::LispString::from_unibyte(
        b"/tmp/neomacs-\xFF/".to_vec(),
    ));
    eval.obarray
        .set_symbol_value("auto-save-list-file-prefix", raw);

    let buffer_name = eval
        .buffers
        .current_buffer()
        .expect("current buffer")
        .name_runtime_string_owned();
    let safe_name = buffer_name.replace('/', "!");
    let expected_dir = crate::emacs_core::builtins::lisp_string_to_runtime_string(raw);
    let expected = format!("{expected_dir}#*{safe_name}*#");

    let value = builtin_make_auto_save_file_name(&mut eval, vec![])
        .expect("make-auto-save-file-name should succeed");
    assert_eq!(
        value.as_runtime_string_owned().as_deref(),
        Some(expected.as_str())
    );
}

// -----------------------------------------------------------------------
// Path operations
// -----------------------------------------------------------------------

#[test]
fn test_expand_file_name_absolute() {
    crate::test_utils::init_test_tracing();
    let result = expand_file_name("/usr/bin/ls", None);
    assert_eq!(result, "/usr/bin/ls");
}

#[test]
fn test_expand_file_name_relative() {
    crate::test_utils::init_test_tracing();
    let result = expand_file_name("foo.txt", Some("/home/user"));
    assert_eq!(result, "/home/user/foo.txt");
}

#[test]
fn test_expand_file_name_tilde() {
    crate::test_utils::init_test_tracing();
    if std::env::var("HOME").is_ok() {
        let result = expand_file_name("~/test.txt", None);
        assert!(result.ends_with("/test.txt"));
        assert!(!result.starts_with("~"));
    }
}

#[test]
fn test_expand_file_name_dotdot() {
    crate::test_utils::init_test_tracing();
    let result = expand_file_name("../bar.txt", Some("/home/user/dir"));
    assert_eq!(result, "/home/user/bar.txt");
}

#[test]
fn test_expand_file_name_dot() {
    crate::test_utils::init_test_tracing();
    let result = expand_file_name("./foo.txt", Some("/home/user"));
    assert_eq!(result, "/home/user/foo.txt");
}

#[test]
fn test_expand_file_name_preserves_directory_marker() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        expand_file_name("fixtures/", Some("/tmp")),
        "/tmp/fixtures/"
    );
    assert_eq!(expand_file_name("", Some("/tmp")), "/tmp");
}

#[test]
fn test_file_truename_missing_file_and_trailing_slash() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        file_truename("/tmp/neovm-file-truename-missing", None),
        "/tmp/neovm-file-truename-missing"
    );
    assert_eq!(file_truename("/tmp/../tmp/", None), "/tmp/");
}

#[test]
fn test_file_truename_resolves_relative_default_directory() {
    crate::test_utils::init_test_tracing();
    let dir = std::env::temp_dir().join("neovm-file-truename-rel");
    let _ = fs::create_dir_all(&dir);
    let file = dir.join("alpha.txt");
    fs::write(&file, b"alpha").unwrap();

    let resolved = file_truename("alpha.txt", Some(&dir.to_string_lossy()));
    assert_eq!(resolved, file.to_string_lossy());

    let _ = fs::remove_file(file);
    let _ = fs::remove_dir(dir);
}

#[test]
fn test_file_name_directory() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        file_name_directory("/home/user/test.txt"),
        Some("/home/user/".to_string())
    );
    assert_eq!(file_name_directory("test.txt"), None);
    assert_eq!(
        file_name_directory("/home/user/dir/"),
        Some("/home/user/dir/".to_string())
    );
}

#[test]
fn test_file_name_nondirectory() {
    crate::test_utils::init_test_tracing();
    assert_eq!(file_name_nondirectory("/home/user/test.txt"), "test.txt");
    assert_eq!(file_name_nondirectory("test.txt"), "test.txt");
    assert_eq!(file_name_nondirectory("/home/user/"), "");
}

#[test]
fn test_file_name_as_directory() {
    crate::test_utils::init_test_tracing();
    assert_eq!(file_name_as_directory("/tmp"), "/tmp/");
    assert_eq!(file_name_as_directory("/tmp/"), "/tmp/");
    assert_eq!(file_name_as_directory(""), "./");
    assert_eq!(file_name_as_directory("foo"), "foo/");
    assert_eq!(file_name_as_directory("foo/"), "foo/");
    assert_eq!(file_name_as_directory("~"), "~/");
    assert_eq!(file_name_as_directory("~/"), "~/");
}

#[test]
fn test_directory_file_name() {
    crate::test_utils::init_test_tracing();
    assert_eq!(directory_file_name("/tmp/"), "/tmp");
    assert_eq!(directory_file_name("/tmp"), "/tmp");
    assert_eq!(directory_file_name("/"), "/");
    assert_eq!(directory_file_name("//"), "//");
    assert_eq!(directory_file_name("///"), "/");
    assert_eq!(directory_file_name("foo/"), "foo");
    assert_eq!(directory_file_name("foo"), "foo");
    assert_eq!(directory_file_name("a//"), "a");
    assert_eq!(directory_file_name("~/"), "~");
    assert_eq!(directory_file_name("~"), "~");
    assert_eq!(directory_file_name(""), "");
}

#[test]
fn test_file_name_concat() {
    crate::test_utils::init_test_tracing();
    assert_eq!(file_name_concat(&["foo", "bar"]), "foo/bar");
    assert_eq!(file_name_concat(&["foo", "bar", "zot"]), "foo/bar/zot");
    assert_eq!(file_name_concat(&["foo/", "bar"]), "foo/bar");
    assert_eq!(file_name_concat(&["foo/", "bar/", "zot"]), "foo/bar/zot");
    assert_eq!(file_name_concat(&["foo", "/bar"]), "foo//bar");
    assert_eq!(file_name_concat(&["foo"]), "foo");
    assert_eq!(file_name_concat(&["foo/"]), "foo/");
    assert_eq!(file_name_concat(&["foo", "", "", ""]), "foo");
    assert_eq!(file_name_concat(&[""]), "");
    assert_eq!(file_name_concat(&["", "bar"]), "bar");
    assert_eq!(file_name_concat(&[]), "");
}

#[test]
fn test_file_name_absolute_p() {
    crate::test_utils::init_test_tracing();
    assert!(file_name_absolute_p("/tmp"));
    assert!(file_name_absolute_p("~/tmp"));
    assert!(file_name_absolute_p("~"));
    assert!(file_name_absolute_p("~root"));
    assert!(!file_name_absolute_p("tmp"));
    assert!(!file_name_absolute_p("./tmp"));
}

#[test]
fn test_directory_name_p() {
    crate::test_utils::init_test_tracing();
    assert!(directory_name_p("/tmp/"));
    assert!(directory_name_p("foo/"));
    assert!(!directory_name_p("/tmp"));
    assert!(!directory_name_p("foo"));
    assert!(!directory_name_p(""));
}

#[test]
fn test_substitute_in_file_name() {
    crate::test_utils::init_test_tracing();
    let home = std::env::var("HOME").unwrap_or_default();

    assert_eq!(substitute_in_file_name("$HOME/foo"), format!("{home}/foo"));
    assert_eq!(
        substitute_in_file_name("${HOME}/foo"),
        format!("{home}/foo")
    );
    assert_eq!(substitute_in_file_name("$UNDEF/foo"), "$UNDEF/foo");
    assert_eq!(substitute_in_file_name("$$HOME"), "$HOME");
    assert_eq!(substitute_in_file_name("${}"), "${}");
    assert_eq!(substitute_in_file_name("$"), "$");
    assert_eq!(substitute_in_file_name("${HOME"), "${HOME");
    assert_eq!(substitute_in_file_name("bar/~/foo"), "~/foo");
    assert_eq!(
        substitute_in_file_name("/usr/local/$HOME/foo"),
        format!("{home}/foo")
    );
    assert_eq!(substitute_in_file_name("a//b"), "/b");
    assert_eq!(substitute_in_file_name("a///b"), "/b");
}

// -----------------------------------------------------------------------
// File predicates
// -----------------------------------------------------------------------

#[test]
fn test_file_exists_p() {
    crate::test_utils::init_test_tracing();
    assert!(file_exists_p("/tmp"));
    assert!(!file_exists_p("/nonexistent_path_12345"));
}

#[test]
fn test_file_directory_p() {
    crate::test_utils::init_test_tracing();
    assert!(file_directory_p("/tmp"));
    assert!(!file_directory_p("/nonexistent_path_12345"));
}

#[test]
fn test_file_regular_p() {
    crate::test_utils::init_test_tracing();
    // /tmp is a directory, not a regular file
    assert!(!file_regular_p("/tmp"));
    assert!(!file_regular_p("/nonexistent_path_12345"));
}

#[test]
fn test_file_symlink_p() {
    crate::test_utils::init_test_tracing();
    // /tmp itself typically isn't a symlink
    assert!(!file_symlink_p("/nonexistent_path_12345"));
}

// -----------------------------------------------------------------------
// File read/write
// -----------------------------------------------------------------------

#[test]
fn test_read_write_file() {
    crate::test_utils::init_test_tracing();
    let dir = std::env::temp_dir().join("neovm_fileio_test");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("test_rw.txt");
    let path_str = path.to_string_lossy().to_string();

    // Write
    write_string_to_file("hello, world\n", &path_str, false).unwrap();

    // Read back
    let contents = read_file_contents(&path_str).unwrap();
    assert_eq!(contents, "hello, world\n");

    // Append
    write_string_to_file("second line\n", &path_str, true).unwrap();
    let contents = read_file_contents(&path_str).unwrap();
    assert_eq!(contents, "hello, world\nsecond line\n");

    // Overwrite
    write_string_to_file("replaced\n", &path_str, false).unwrap();
    let contents = read_file_contents(&path_str).unwrap();
    assert_eq!(contents, "replaced\n");

    // Predicates on the file we just wrote
    assert!(file_exists_p(&path_str));
    assert!(file_regular_p(&path_str));
    assert!(file_readable_p(&path_str));
    assert!(!file_directory_p(&path_str));

    // Clean up
    delete_file(&path_str).unwrap();
    assert!(!file_exists_p(&path_str));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_file_error_symbol_mapping() {
    crate::test_utils::init_test_tracing();
    assert_eq!(file_error_symbol(ErrorKind::NotFound), "file-missing");
    assert_eq!(
        file_error_symbol(ErrorKind::AlreadyExists),
        "file-already-exists"
    );
    assert_eq!(
        file_error_symbol(ErrorKind::PermissionDenied),
        "permission-denied"
    );
    assert_eq!(file_error_symbol(ErrorKind::InvalidInput), "file-error");
}

#[test]
fn test_signal_file_io_error_uses_specific_condition() {
    crate::test_utils::init_test_tracing();
    let flow = signal_file_io_error(
        std::io::Error::from(ErrorKind::PermissionDenied),
        "Writing to /tmp/neovm-probe".to_string(),
    );
    match flow {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "permission-denied");
            assert_eq!(sig.data.len(), 1);
            let Some(message) = sig.data[0].as_str() else {
                panic!("expected string error payload");
            };
            assert!(message.contains("Writing to /tmp/neovm-probe"));
        }
        other => panic!("expected signal, got {:?}", other),
    }
}

#[test]
fn test_delete_file_compat_missing_is_noop() {
    crate::test_utils::init_test_tracing();
    let path = std::env::temp_dir().join("neovm_delete_missing_noop.tmp");
    let path_str = path.to_string_lossy().to_string();
    let _ = fs::remove_file(&path);
    assert!(delete_file_compat(&path_str).is_ok());
}

#[test]
fn test_builtin_delete_file_accepts_optional_trash_arg() {
    crate::test_utils::init_test_tracing();
    let path = std::env::temp_dir().join("neovm_delete_file_trash_arg.tmp");
    let path_str = path.to_string_lossy().to_string();
    let _ = fs::remove_file(&path);
    fs::write(&path, b"x").unwrap();

    let result = call_fileio_builtin!(
        builtin_delete_file,
        vec![Value::string(&path_str), Value::T]
    )
    .unwrap();
    assert_eq!(result, Value::NIL);
    assert!(!path.exists());

    let err = call_fileio_builtin!(
        builtin_delete_file,
        vec![Value::string(&path_str), Value::NIL, Value::NIL]
    )
    .unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("delete-file"), Value::fixnum(3)]
            );
        }
        other => panic!("expected signal, got {:?}", other),
    }
}

#[test]
fn test_builtin_delete_directory_basic_and_recursive() {
    crate::test_utils::init_test_tracing();
    let root = std::env::temp_dir().join("neovm_delete_directory_test");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let root_str = root.to_string_lossy().to_string();

    // Non-recursive removal succeeds for empty directories.
    assert_eq!(
        call_fileio_builtin!(builtin_delete_directory, vec![Value::string(&root_str)]).unwrap(),
        Value::NIL
    );
    assert!(!root.exists());

    // Non-recursive removal fails for non-empty directories.
    fs::create_dir_all(&root).unwrap();
    let nested = root.join("child.txt");
    fs::write(&nested, b"x").unwrap();
    let err =
        call_fileio_builtin!(builtin_delete_directory, vec![Value::string(&root_str)]).unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "file-error");
        }
        other => panic!("expected signal, got {:?}", other),
    }

    // Recursive removal succeeds.
    assert_eq!(
        call_fileio_builtin!(
            builtin_delete_directory,
            vec![Value::string(&root_str), Value::T]
        )
        .unwrap(),
        Value::NIL
    );
    assert!(!root.exists());
}

#[test]
fn test_builtin_delete_directory_eval_resolves_default_directory() {
    crate::test_utils::init_test_tracing();
    let base = std::env::temp_dir().join("neovm-delete-dir-eval");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let mut eval = Context::new();
    eval.set_variable(
        "default-directory",
        Value::string(format!("{}/", base.to_string_lossy())),
    );

    let child = base.join("child");
    fs::create_dir_all(&child).unwrap();
    builtin_delete_directory(&mut eval, vec![Value::string("child")]).unwrap();
    assert!(!child.exists());

    let _ = fs::remove_dir_all(base);
}

#[cfg(unix)]
#[test]
fn test_builtin_make_symbolic_link_core_semantics() {
    crate::test_utils::init_test_tracing();
    let base = std::env::temp_dir().join("neovm-symlink-test");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let target = base.join("target.txt");
    let link = base.join("link.txt");
    fs::write(&target, b"x").unwrap();
    let target_str = target.to_string_lossy().to_string();
    let link_str = link.to_string_lossy().to_string();

    assert_eq!(
        call_fileio_builtin!(
            builtin_make_symbolic_link,
            vec![Value::string(&target_str), Value::string(&link_str)]
        )
        .unwrap(),
        Value::NIL
    );
    assert!(file_symlink_p(&link_str));

    let err = call_fileio_builtin!(
        builtin_make_symbolic_link,
        vec![Value::string(&target_str), Value::string(&link_str)]
    )
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "file-already-exists"),
        other => panic!("expected signal, got {:?}", other),
    }

    assert_eq!(
        call_fileio_builtin!(
            builtin_make_symbolic_link,
            vec![
                Value::string(&target_str),
                Value::string(&link_str),
                Value::T,
            ]
        )
        .unwrap(),
        Value::NIL
    );

    delete_file(&link_str).unwrap();
    delete_file(&target_str).unwrap();
    let _ = fs::remove_dir_all(base);
}

#[cfg(unix)]
#[test]
fn test_builtin_make_symbolic_link_eval_uses_default_directory() {
    crate::test_utils::init_test_tracing();
    let base = std::env::temp_dir().join("neovm-symlink-eval");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let mut eval = Context::new();
    eval.set_variable(
        "default-directory",
        Value::string(format!("{}/", base.to_string_lossy())),
    );

    fs::write(base.join("target.txt"), b"x").unwrap();
    builtin_make_symbolic_link(
        &mut eval,
        vec![Value::string("target.txt"), Value::string("link.txt")],
    )
    .unwrap();
    assert!(file_symlink_p(&base.join("link.txt").to_string_lossy()));

    delete_file(&base.join("link.txt").to_string_lossy()).unwrap();
    delete_file(&base.join("target.txt").to_string_lossy()).unwrap();
    let _ = fs::remove_dir_all(base);
}

// -----------------------------------------------------------------------
// Directory operations
// -----------------------------------------------------------------------

#[test]
fn test_make_directory_and_directory_files() {
    crate::test_utils::init_test_tracing();
    let base = std::env::temp_dir().join("neovm_dirtest");
    let _ = fs::remove_dir_all(&base);
    let base_str = base.to_string_lossy().to_string();

    // Create with parents
    let nested = base.join("a/b/c");
    let nested_str = nested.to_string_lossy().to_string();
    make_directory(&nested_str, true).unwrap();
    assert!(file_directory_p(&nested_str));

    // Create files in the base directory
    for name in &["foo.txt", "bar.txt", "baz.el"] {
        let p = base.join(name);
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(b"data").unwrap();
    }

    // List files
    let files = directory_files(&base_str, false, None, false, None).unwrap();
    assert!(files.contains(&".".to_string()));
    assert!(files.contains(&"..".to_string()));
    assert!(files.contains(&"foo.txt".to_string()));
    assert!(files.contains(&"bar.txt".to_string()));
    assert!(files.contains(&"baz.el".to_string()));

    // List with filter
    let filtered = directory_files(&base_str, false, Some("\\.el$"), false, None).unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0], "baz.el");

    // List with full paths
    let full = directory_files(&base_str, true, None, false, None).unwrap();
    for entry in &full {
        assert!(entry.starts_with(&base_str));
    }

    // Clean up
    let _ = fs::remove_dir_all(&base);
}

// -----------------------------------------------------------------------
// File management: rename, copy
// -----------------------------------------------------------------------

#[test]
fn test_rename_and_copy_file() {
    crate::test_utils::init_test_tracing();
    let dir = std::env::temp_dir().join("neovm_rename_copy_test");
    let _ = fs::create_dir_all(&dir);

    let src = dir.join("source.txt");
    let dst_rename = dir.join("renamed.txt");
    let dst_copy = dir.join("copied.txt");

    let src_str = src.to_string_lossy().to_string();
    let dst_rename_str = dst_rename.to_string_lossy().to_string();
    let dst_copy_str = dst_copy.to_string_lossy().to_string();

    // Create source
    write_string_to_file("original content", &src_str, false).unwrap();

    // Copy
    copy_file(&src_str, &dst_copy_str).unwrap();
    assert!(file_exists_p(&src_str));
    assert!(file_exists_p(&dst_copy_str));
    assert_eq!(
        read_file_contents(&dst_copy_str).unwrap(),
        "original content"
    );

    // Rename
    rename_file(&src_str, &dst_rename_str).unwrap();
    assert!(!file_exists_p(&src_str));
    assert!(file_exists_p(&dst_rename_str));
    assert_eq!(
        read_file_contents(&dst_rename_str).unwrap(),
        "original content"
    );

    // Clean up
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_builtin_rename_file_overwrite_semantics() {
    crate::test_utils::init_test_tracing();
    let dir = std::env::temp_dir().join("neovm_builtin_rename_overwrite");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let src = dir.join("src.txt");
    let dst = dir.join("dst.txt");
    fs::write(&src, b"x").unwrap();
    fs::write(&dst, b"y").unwrap();
    let src_s = src.to_string_lossy().to_string();
    let dst_s = dst.to_string_lossy().to_string();

    let err = call_fileio_builtin!(
        builtin_rename_file,
        vec![Value::string(&src_s), Value::string(&dst_s)]
    )
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "file-already-exists"),
        other => panic!("expected signal, got {:?}", other),
    }

    assert_eq!(
        call_fileio_builtin!(
            builtin_rename_file,
            vec![Value::string(&src_s), Value::string(&dst_s), Value::T]
        )
        .unwrap(),
        Value::NIL
    );
    assert!(!src.exists());
    assert!(dst.exists());

    let err = call_fileio_builtin!(
        builtin_rename_file,
        vec![
            Value::string("a"),
            Value::string("b"),
            Value::NIL,
            Value::NIL,
        ]
    )
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments, got {:?}", other),
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_builtin_copy_file_optional_arg_semantics() {
    crate::test_utils::init_test_tracing();
    let dir = std::env::temp_dir().join("neovm_builtin_copy_optional");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let src = dir.join("src.txt");
    let dst = dir.join("dst.txt");
    fs::write(&src, b"src").unwrap();
    fs::write(&dst, b"dst").unwrap();
    let src_s = src.to_string_lossy().to_string();
    let dst_s = dst.to_string_lossy().to_string();

    let err = call_fileio_builtin!(
        builtin_copy_file,
        vec![Value::string(&src_s), Value::string(&dst_s)]
    )
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "file-already-exists"),
        other => panic!("expected signal, got {:?}", other),
    }

    assert_eq!(
        call_fileio_builtin!(
            builtin_copy_file,
            vec![Value::string(&src_s), Value::string(&dst_s), Value::T]
        )
        .unwrap(),
        Value::NIL
    );

    assert_eq!(
        call_fileio_builtin!(
            builtin_copy_file,
            vec![
                Value::string(&src_s),
                Value::string(&dst_s),
                Value::T,
                Value::T,
                Value::T,
                Value::T,
            ]
        )
        .unwrap(),
        Value::NIL
    );

    let err = call_fileio_builtin!(
        builtin_copy_file,
        vec![
            Value::string("a"),
            Value::string("b"),
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL,
        ]
    )
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-number-of-arguments"),
        other => panic!("expected wrong-number-of-arguments, got {:?}", other),
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_builtin_add_name_to_file_semantics() {
    crate::test_utils::init_test_tracing();
    let dir = std::env::temp_dir().join("neovm_add_name_to_file_test");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let src = dir.join("source.txt");
    let dst = dir.join("alias.txt");
    fs::write(&src, b"x").unwrap();

    let src_str = src.to_string_lossy().to_string();
    let dst_str = dst.to_string_lossy().to_string();

    assert_eq!(
        call_fileio_builtin!(
            builtin_add_name_to_file,
            vec![Value::string(&src_str), Value::string(&dst_str)]
        )
        .unwrap(),
        Value::NIL
    );
    assert!(file_exists_p(&dst_str));
    assert_same_file_paths(&src_str, &dst_str);

    let err = call_fileio_builtin!(
        builtin_add_name_to_file,
        vec![Value::string(&src_str), Value::string(&dst_str)]
    )
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "file-already-exists"),
        other => panic!("expected signal, got {:?}", other),
    }

    assert_eq!(
        call_fileio_builtin!(
            builtin_add_name_to_file,
            vec![Value::string(&src_str), Value::string(&dst_str), Value::T,]
        )
        .unwrap(),
        Value::NIL
    );
    assert_same_file_paths(&src_str, &dst_str);

    let missing = dir.join("missing.txt").to_string_lossy().to_string();
    let dst2 = dir.join("alias2.txt").to_string_lossy().to_string();
    let err = call_fileio_builtin!(
        builtin_add_name_to_file,
        vec![Value::string(&missing), Value::string(&dst2)]
    )
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "file-missing"),
        other => panic!("expected signal, got {:?}", other),
    }

    let _ = fs::remove_dir_all(&dir);
}

// -----------------------------------------------------------------------
// File attributes
// -----------------------------------------------------------------------

#[test]
fn test_file_attributes() {
    crate::test_utils::init_test_tracing();
    let dir = std::env::temp_dir().join("neovm_attrs_test");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("attrs.txt");
    let path_str = path.to_string_lossy().to_string();

    write_string_to_file("content", &path_str, false).unwrap();

    let attrs = file_attributes(&path_str).unwrap();
    assert_eq!(attrs.size, 7); // "content" is 7 bytes
    assert!(!attrs.is_dir);
    assert!(!attrs.is_symlink);
    assert!(attrs.modified.is_some());

    // Directory attributes
    let dir_str = dir.to_string_lossy().to_string();
    let dir_attrs = file_attributes(&dir_str).unwrap();
    assert!(dir_attrs.is_dir);

    // Non-existent file
    assert!(file_attributes("/nonexistent_path_12345").is_none());

    // Clean up
    let _ = fs::remove_dir_all(&dir);
}

// -----------------------------------------------------------------------
// Builtin wrappers (Value-level)
// -----------------------------------------------------------------------

#[test]
fn test_builtin_expand_file_name() {
    crate::test_utils::init_test_tracing();
    let result = call_fileio_builtin!(
        builtin_expand_file_name,
        vec![Value::string("/usr/local/bin/emacs")]
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_str(), Some("/usr/local/bin/emacs"));

    // Emacs treats non-string DEFAULT-DIRECTORY as root.
    let result = call_fileio_builtin!(
        builtin_expand_file_name,
        vec![Value::string("a"), Value::symbol("x")]
    );
    assert_eq!(result.unwrap().as_str(), Some("/a"));

    let result = call_fileio_builtin!(
        builtin_expand_file_name,
        vec![Value::string("a"), Value::NIL, Value::NIL]
    );
    assert!(result.is_err());
}

#[test]
fn test_builtin_expand_file_name_eval_uses_default_directory() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    // `default-directory` is a SYMBOL_FORWARDED BUFFER_OBJFWD slot
    // since Phase 10C; the per-buffer slot is the source of truth.
    // Using `eval.set_variable` routes through the FORWARDED path
    // (mirroring GNU's `set_internal` for SYMBOL_FORWARDED) so the
    // current buffer's slot is updated, which is what
    // `default_directory_in_state` reads.
    eval.set_variable("default-directory", Value::string("/tmp/neovm-expand/"));

    let with_implicit = builtin_expand_file_name(&mut eval, vec![Value::string("alpha.txt")]);
    assert_eq!(
        with_implicit.unwrap().as_str(),
        Some("/tmp/neovm-expand/alpha.txt")
    );

    let with_nil = builtin_expand_file_name(&mut eval, vec![Value::string("beta.txt"), Value::NIL]);
    assert_eq!(
        with_nil.unwrap().as_str(),
        Some("/tmp/neovm-expand/beta.txt")
    );
}

#[test]
fn test_fileio_eval_prefers_current_buffer_local_default_directory() {
    crate::test_utils::init_test_tracing();
    let base =
        std::env::temp_dir().join(format!("neovm-fileio-buffer-local-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("subdir")).unwrap();
    fs::write(base.join("alpha.txt"), "alpha").unwrap();

    let mut eval = Context::new();
    eval.set_variable("default-directory", Value::string("/tmp/neovm-global/"));
    let current = eval.buffers.current_buffer_id().expect("current buffer");
    let base_str = format!("{}/", base.to_string_lossy());
    eval.buffers
        .set_buffer_local_property(current, "default-directory", Value::string(&base_str))
        .expect("buffer local default-directory should set");

    assert_eq!(
        builtin_expand_file_name(&mut eval, vec![Value::string("alpha.txt")])
            .unwrap()
            .as_str(),
        Some(base.join("alpha.txt").to_string_lossy().as_ref())
    );
    assert_eq!(
        builtin_file_truename(&mut eval, vec![Value::string("alpha.txt")])
            .unwrap()
            .as_str(),
        Some(base.join("alpha.txt").to_string_lossy().as_ref())
    );
    assert_eq!(
        builtin_file_exists_p(&mut eval, vec![Value::string("alpha.txt")]).unwrap(),
        Value::T
    );
    assert_eq!(
        builtin_file_directory_p(&mut eval, vec![Value::string("subdir")]).unwrap(),
        Value::T
    );
    assert_eq!(
        builtin_file_regular_p(&mut eval, vec![Value::string("alpha.txt")]).unwrap(),
        Value::T
    );

    let _ = fs::remove_dir_all(base);
}

#[test]
fn test_builtin_file_truename_counter_validation() {
    crate::test_utils::init_test_tracing();
    let value = call_fileio_builtin!(
        builtin_file_truename,
        vec![Value::string("/tmp"), Value::list(vec![])]
    )
    .unwrap();
    assert_eq!(value.as_str(), Some("/tmp"));

    let err = call_fileio_builtin!(
        builtin_file_truename,
        vec![Value::string("/tmp"), Value::fixnum(1)]
    )
    .unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("listp"), Value::fixnum(1)]);
        }
        other => panic!("expected signal, got {:?}", other),
    }

    let err = call_fileio_builtin!(
        builtin_file_truename,
        vec![
            Value::string("/tmp"),
            Value::list(vec![Value::symbol("visited")]),
        ]
    )
    .unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![
                    Value::symbol("number-or-marker-p"),
                    Value::symbol("visited")
                ]
            );
        }
        other => panic!("expected signal, got {:?}", other),
    }
}

#[test]
fn test_builtin_file_truename_eval_uses_default_directory() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.set_variable(
        "default-directory",
        Value::string("/tmp/neovm-file-truename/"),
    );

    let value = builtin_file_truename(&mut eval, vec![Value::string("alpha.txt")]).unwrap();
    assert_eq!(value.as_str(), Some("/tmp/neovm-file-truename/alpha.txt"));
}

#[test]
fn test_builtin_make_temp_file_core_paths() {
    crate::test_utils::init_test_tracing();
    let file =
        call_fileio_builtin!(builtin_make_temp_file, vec![Value::string("neovm-mtf-")]).unwrap();
    let file_path = file.as_str().unwrap().to_string();
    assert!(file_exists_p(&file_path));
    delete_file(&file_path).unwrap();

    let dir = call_fileio_builtin!(
        builtin_make_temp_file,
        vec![Value::string("neovm-mtf-dir-"), Value::T]
    )
    .unwrap();
    let dir_path = dir.as_str().unwrap().to_string();
    assert!(file_directory_p(&dir_path));
    fs::remove_dir(&dir_path).unwrap();

    let with_text = call_fileio_builtin!(
        builtin_make_temp_file,
        vec![
            Value::string("neovm-mtf-text-"),
            Value::NIL,
            Value::string(".txt"),
            Value::string("abc"),
        ]
    )
    .unwrap();
    let text_path = with_text.as_str().unwrap().to_string();
    assert_eq!(read_file_contents(&text_path).unwrap(), "abc");
    delete_file(&text_path).unwrap();
}

#[test]
fn test_builtin_make_temp_file_validation() {
    crate::test_utils::init_test_tracing();
    let err = call_fileio_builtin!(builtin_make_temp_file, vec![Value::fixnum(1)]).unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("sequencep"), Value::fixnum(1)]);
        }
        other => panic!("expected signal, got {:?}", other),
    }

    let err = call_fileio_builtin!(
        builtin_make_temp_file,
        vec![Value::string("neo"), Value::NIL, Value::fixnum(1)]
    )
    .unwrap_err();
    match err {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::fixnum(1)]);
        }
        other => panic!("expected signal, got {:?}", other),
    }
}

#[test]
fn test_builtin_make_temp_file_eval_honors_temp_directory() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let dir = std::env::temp_dir().join("neovm-mtf-eval");
    let _ = fs::create_dir_all(&dir);
    eval.obarray.set_symbol_value(
        "temporary-file-directory",
        Value::string(format!("{}/", dir.to_string_lossy())),
    );

    let value = builtin_make_temp_file(&mut eval, vec![Value::string("eval-neo-")]).unwrap();
    let path = value.as_str().unwrap().to_string();
    assert!(path.starts_with(&dir.to_string_lossy().to_string()));
    assert!(file_exists_p(&path));
    delete_file(&path).unwrap();
    let _ = fs::remove_dir(&dir);
}

#[test]
fn test_builtin_make_nearby_temp_file_core_semantics() {
    crate::test_utils::init_test_tracing();
    let path = call_fileio_builtin!(
        builtin_make_nearby_temp_file,
        vec![Value::string("neovm-nearby-")]
    )
    .unwrap();
    let path_str = path.as_str().unwrap().to_string();
    assert!(file_exists_p(&path_str));
    delete_file(&path_str).unwrap();

    let dir = call_fileio_builtin!(
        builtin_make_nearby_temp_file,
        vec![Value::string("neovm-nearby-dir-"), Value::T]
    )
    .unwrap();
    let dir_str = dir.as_str().unwrap().to_string();
    assert!(file_directory_p(&dir_str));
    fs::remove_dir(&dir_str).unwrap();

    let base = std::env::temp_dir().join("neovm-nearby-parent");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let prefix = base.join("child-").to_string_lossy().to_string();
    let nearby =
        call_fileio_builtin!(builtin_make_nearby_temp_file, vec![Value::string(&prefix)]).unwrap();
    let nearby_str = nearby.as_str().unwrap().to_string();
    assert_eq!(
        file_name_directory(&nearby_str),
        file_name_directory(&prefix),
    );
    assert!(file_exists_p(&nearby_str));
    delete_file(&nearby_str).unwrap();
    fs::remove_dir_all(&base).unwrap();
}

#[test]
fn test_builtin_make_nearby_temp_file_eval_relative_prefix_uses_temp_dir() {
    crate::test_utils::init_test_tracing();
    let base = std::env::temp_dir().join("neovm-nearby-eval");
    let sub = base.join("sub");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&sub).unwrap();
    let mut eval = Context::new();
    eval.set_variable(
        "default-directory",
        Value::string(format!("{}/", base.to_string_lossy())),
    );

    let err =
        builtin_make_nearby_temp_file(&mut eval, vec![Value::string("sub/child-")]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "file-missing"),
        other => panic!("expected signal, got {:?}", other),
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn test_builtin_file_predicates() {
    crate::test_utils::init_test_tracing();
    let result = call_fileio_builtin!(builtin_file_exists_p, vec![Value::string("/tmp")]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_truthy());

    let result = call_fileio_builtin!(builtin_file_directory_p, vec![Value::string("/tmp")]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_truthy());

    let result = call_fileio_builtin!(
        builtin_file_exists_p,
        vec![Value::string("/no_such_file_xyz")]
    );
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn test_builtin_access_file_semantics() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        call_fileio_builtin!(
            builtin_access_file,
            vec![Value::string("/tmp"), Value::string("read")]
        )
        .unwrap(),
        Value::NIL
    );

    let missing = call_fileio_builtin!(
        builtin_access_file,
        vec![
            Value::string("/definitely-not-here-neovm"),
            Value::string("read"),
        ]
    )
    .expect_err("missing file should signal");
    match missing {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "file-missing");
            assert_eq!(sig.data.first(), Some(&Value::string("read")));
            assert_eq!(
                sig.data.last(),
                Some(&Value::string("/definitely-not-here-neovm"))
            );
        }
        other => panic!("expected file-missing signal, got {:?}", other),
    }

    let file_type = call_fileio_builtin!(
        builtin_access_file,
        vec![Value::fixnum(1), Value::string("read")]
    )
    .expect_err("FILE should require string");
    match file_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::fixnum(1)]);
        }
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }

    let op_type = call_fileio_builtin!(
        builtin_access_file,
        vec![Value::string("/tmp"), Value::fixnum(1)]
    )
    .expect_err("OPERATION should require string");
    match op_type {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::fixnum(1)]);
        }
        other => panic!("expected wrong-type-argument, got {:?}", other),
    }
}

#[test]
fn test_builtin_file_modes_semantics() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        call_fileio_builtin!(
            builtin_file_modes,
            vec![Value::string("/tmp/neovm-file-modes-missing")]
        )
        .unwrap(),
        Value::NIL
    );

    let path = call_fileio_builtin!(
        builtin_make_temp_file,
        vec![Value::string("neovm-file-modes-")]
    )
    .unwrap();
    let path_str = path.as_str().unwrap().to_string();
    let mode = call_fileio_builtin!(builtin_file_modes, vec![Value::string(&path_str)]).unwrap();
    assert!(mode.is_fixnum());
    let with_flag =
        call_fileio_builtin!(builtin_file_modes, vec![Value::string(&path_str), Value::T]).unwrap();
    assert!(with_flag.is_fixnum());
    delete_file(&path_str).unwrap();
}

#[test]
fn test_builtin_file_modes_eval_respects_default_directory() {
    crate::test_utils::init_test_tracing();
    let base = std::env::temp_dir().join("neovm-file-modes-eval");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let file = base.join("alpha.txt");
    fs::write(&file, b"x").unwrap();

    let mut eval = Context::new();
    eval.set_variable(
        "default-directory",
        Value::string(format!("{}/", base.to_string_lossy())),
    );
    let mode = builtin_file_modes(&mut eval, vec![Value::string("alpha.txt")]).unwrap();
    assert!(mode.is_fixnum());

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn test_builtin_set_file_modes_semantics() {
    crate::test_utils::init_test_tracing();
    let path = call_fileio_builtin!(
        builtin_make_temp_file,
        vec![Value::string("neovm-set-file-modes-")]
    )
    .unwrap();
    let path_str = path.as_str().unwrap().to_string();

    assert_eq!(
        call_fileio_builtin!(
            builtin_set_file_modes,
            vec![Value::string(&path_str), Value::fixnum(0o600)]
        )
        .unwrap(),
        Value::NIL
    );
    assert_eq!(
        call_fileio_builtin!(
            builtin_set_file_modes,
            vec![Value::string(&path_str), Value::fixnum(0o640), Value::T]
        )
        .unwrap(),
        Value::NIL
    );
    assert_eq!(
        call_fileio_builtin!(builtin_file_modes, vec![Value::string(&path_str)])
            .unwrap()
            .as_int(),
        Some(0o640)
    );

    delete_file(&path_str).unwrap();
}

#[test]
fn test_builtin_set_file_modes_eval_respects_default_directory() {
    crate::test_utils::init_test_tracing();
    let base = std::env::temp_dir().join("neovm-set-file-modes-eval");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let file = base.join("alpha.txt");
    fs::write(&file, b"x").unwrap();

    let mut eval = Context::new();
    eval.set_variable(
        "default-directory",
        Value::string(format!("{}/", base.to_string_lossy())),
    );
    builtin_set_file_modes(
        &mut eval,
        vec![Value::string("alpha.txt"), Value::fixnum(0o600)],
    )
    .unwrap();
    assert_eq!(
        call_fileio_builtin!(
            builtin_file_modes,
            vec![Value::string(file.to_string_lossy().to_string())]
        )
        .unwrap()
        .as_int(),
        Some(0o600)
    );

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn test_builtin_directory_files_args() {
    crate::test_utils::init_test_tracing();
    let dir = std::env::temp_dir().join("neovm_dirfiles_builtin");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_string_lossy().to_string();
    let file = dir.join("beta.el");
    fs::write(&file, "").unwrap();
    fs::write(dir.join("alpha.txt"), "").unwrap();
    fs::write(dir.join(".hidden"), "").unwrap();

    let result = call_fileio_builtin!(
        builtin_directory_files,
        vec![
            Value::string(&dir_str),
            Value::NIL,
            Value::string("\\.el$"),
            Value::NIL,
            Value::fixnum(1),
        ]
    )
    .unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].as_str(), Some("beta.el"));

    let unsorted = call_fileio_builtin!(
        builtin_directory_files,
        vec![Value::string(&dir_str), Value::NIL, Value::NIL, Value::T,]
    )
    .unwrap();
    let unsorted_items = list_to_vec(&unsorted).unwrap();

    let unsorted_limited = call_fileio_builtin!(
        builtin_directory_files,
        vec![
            Value::string(&dir_str),
            Value::NIL,
            Value::NIL,
            Value::T,
            Value::fixnum(2),
        ]
    )
    .unwrap();
    let unsorted_limited_items = list_to_vec(&unsorted_limited).unwrap();
    let tail = &unsorted_items[unsorted_items.len() - 2..];
    assert_eq!(unsorted_limited_items.as_slice(), tail);

    let sorted_limited = call_fileio_builtin!(
        builtin_directory_files,
        vec![
            Value::string(&dir_str),
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::fixnum(2),
        ]
    )
    .unwrap();
    let mut sorted_from_unsorted = unsorted_limited_items.clone();
    sorted_from_unsorted.sort_by(|a, b| {
        let a = a.as_str().unwrap_or_default();
        let b = b.as_str().unwrap_or_default();
        a.cmp(b)
    });
    assert_eq!(list_to_vec(&sorted_limited).unwrap(), sorted_from_unsorted);

    let result = call_fileio_builtin!(
        builtin_directory_files,
        vec![
            Value::string(&dir_str),
            Value::NIL,
            Value::NIL,
            Value::T,
            Value::fixnum(0),
        ]
    )
    .unwrap();
    assert!(list_to_vec(&result).unwrap().is_empty());

    let result = call_fileio_builtin!(
        builtin_directory_files,
        vec![
            Value::string(&dir_str),
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::fixnum(-1),
        ]
    );
    assert!(result.is_err());

    let result = call_fileio_builtin!(
        builtin_directory_files,
        vec![
            Value::string(&dir_str),
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::fixnum(0),
            Value::NIL,
        ]
    );
    assert!(result.is_err());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_builtin_directory_files_eval_respects_default_directory() {
    crate::test_utils::init_test_tracing();
    let base = std::env::temp_dir().join("neovm_dirfiles_eval_builtin");
    let fixture = base.join("fixtures");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&fixture).unwrap();
    fs::write(fixture.join("alpha.txt"), "").unwrap();
    fs::write(fixture.join("beta.el"), "").unwrap();

    let mut eval = Context::new();
    let base_str = format!("{}/", base.to_string_lossy());
    eval.set_variable("default-directory", Value::string(&base_str));

    let result = builtin_directory_files(
        &mut eval,
        vec![
            Value::string("fixtures"),
            Value::NIL,
            Value::string("\\.el$"),
        ],
    )
    .unwrap();
    let items = list_to_vec(&result).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].as_str(), Some("beta.el"));

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn test_builtin_directory_files_nonexistent_signals_file_missing() {
    crate::test_utils::init_test_tracing();
    let result = call_fileio_builtin!(
        builtin_directory_files,
        vec![Value::string("/nonexistent_dir_xyz_12345")]
    );
    match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "file-missing"),
        other => panic!("expected file-missing signal, got {:?}", other),
    }
}

#[test]
fn test_builtin_directory_files_invalid_regexp_signals_invalid_regexp() {
    crate::test_utils::init_test_tracing();
    let dir = std::env::temp_dir().join("neovm_dirfiles_invalid_regexp");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_string_lossy().to_string();

    let result = call_fileio_builtin!(
        builtin_directory_files,
        vec![
            Value::string(&dir_str),
            Value::NIL,
            Value::string("[invalid"),
        ]
    );
    match result {
        Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "invalid-regexp"),
        other => panic!("expected invalid-regexp signal, got {:?}", other),
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_builtin_file_ops_eval_respects_default_directory() {
    crate::test_utils::init_test_tracing();
    let base = std::env::temp_dir().join("neovm_fileops_eval_builtin");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    fs::write(base.join("alpha.txt"), "x").unwrap();

    let mut eval = Context::new();
    let base_str = format!("{}/", base.to_string_lossy());
    eval.set_variable("default-directory", Value::string(&base_str));

    builtin_copy_file(
        &mut eval,
        vec![Value::string("alpha.txt"), Value::string("beta.txt")],
    )
    .unwrap();
    assert!(base.join("beta.txt").exists());

    builtin_rename_file(
        &mut eval,
        vec![Value::string("beta.txt"), Value::string("gamma.txt")],
    )
    .unwrap();
    assert!(!base.join("beta.txt").exists());
    assert!(base.join("gamma.txt").exists());

    builtin_delete_file(&mut eval, vec![Value::string("gamma.txt")]).unwrap();
    assert!(!base.join("gamma.txt").exists());

    builtin_add_name_to_file(
        &mut eval,
        vec![Value::string("alpha.txt"), Value::string("delta.txt")],
    )
    .unwrap();
    assert!(base.join("delta.txt").exists());
    assert_same_file_paths(
        &base.join("alpha.txt").to_string_lossy(),
        &base.join("delta.txt").to_string_lossy(),
    );
    builtin_delete_file(&mut eval, vec![Value::string("delta.txt")]).unwrap();

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn test_builtin_rename_file_eval_overwrite_semantics() {
    crate::test_utils::init_test_tracing();
    let base = std::env::temp_dir().join("neovm_rename_eval_overwrite");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    fs::write(base.join("src.txt"), "x").unwrap();
    fs::write(base.join("dst.txt"), "y").unwrap();

    let mut eval = Context::new();
    let base_str = format!("{}/", base.to_string_lossy());
    eval.set_variable("default-directory", Value::string(&base_str));

    let err = builtin_rename_file(
        &mut eval,
        vec![Value::string("src.txt"), Value::string("dst.txt")],
    )
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "file-already-exists"),
        other => panic!("expected signal, got {:?}", other),
    }

    assert_eq!(
        builtin_rename_file(
            &mut eval,
            vec![Value::string("src.txt"), Value::string("dst.txt"), Value::T],
        )
        .unwrap(),
        Value::NIL
    );
    assert!(!base.join("src.txt").exists());
    assert!(base.join("dst.txt").exists());

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn test_builtin_copy_file_eval_optional_arg_semantics() {
    crate::test_utils::init_test_tracing();
    let base = std::env::temp_dir().join("neovm_copy_eval_optional");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    fs::write(base.join("src.txt"), "src").unwrap();
    fs::write(base.join("dst.txt"), "dst").unwrap();

    let mut eval = Context::new();
    let base_str = format!("{}/", base.to_string_lossy());
    eval.set_variable("default-directory", Value::string(&base_str));

    let err = builtin_copy_file(
        &mut eval,
        vec![Value::string("src.txt"), Value::string("dst.txt")],
    )
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "file-already-exists"),
        other => panic!("expected signal, got {:?}", other),
    }

    assert_eq!(
        builtin_copy_file(
            &mut eval,
            vec![Value::string("src.txt"), Value::string("dst.txt"), Value::T],
        )
        .unwrap(),
        Value::NIL
    );

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn test_builtin_file_name_ops() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let result = builtin_file_name_directory(&mut ev, vec![Value::string("/home/user/test.el")]);
    assert_eq!(result.unwrap().as_str(), Some("/home/user/"));

    let result = builtin_file_name_nondirectory(&mut ev, vec![Value::string("/home/user/test.el")]);
    assert_eq!(result.unwrap().as_str(), Some("test.el"));

    let result = builtin_file_name_as_directory(&mut ev, vec![Value::string("/home/user")]);
    assert_eq!(result.unwrap().as_str(), Some("/home/user/"));

    let result = builtin_directory_file_name(&mut ev, vec![Value::string("/home/user/")]);
    assert_eq!(result.unwrap().as_str(), Some("/home/user"));

    let result = builtin_file_name_concat(vec![
        Value::string("foo"),
        Value::string(""),
        Value::NIL,
        Value::string("bar"),
    ]);
    assert_eq!(result.unwrap().as_str(), Some("foo/bar"));
}

#[test]
fn test_builtin_file_name_ops_strict_types() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    assert!(builtin_file_name_directory(&mut ev, vec![Value::symbol("x")]).is_err());
    assert!(builtin_file_name_nondirectory(&mut ev, vec![Value::symbol("x")]).is_err());
    assert!(builtin_file_name_as_directory(&mut ev, vec![Value::symbol("x")]).is_err());
    assert!(builtin_directory_file_name(&mut ev, vec![Value::symbol("x")]).is_err());
}

#[test]
fn file_name_with_extension_bootstrap_matches_gnu_elisp() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval(
        r#"
        (file-name-with-extension "foo" "el")
        (file-name-with-extension "foo.el" "txt")
        (file-name-with-extension "foo" ".el")
        (condition-case err (file-name-with-extension "foo" "") (error (car err)))
        (condition-case err (file-name-with-extension "/tmp/dir/" "el") (error (car err)))
        (condition-case err (file-name-with-extension 'x "el") (error (car err)))
        (condition-case err (file-name-with-extension "x" 'el) (error (car err)))
        "#,
    );
    assert_eq!(results[0], r#"OK "foo.el""#);
    assert_eq!(results[1], r#"OK "foo.txt""#);
    assert_eq!(results[2], r#"OK "foo.el""#);
    assert_eq!(results[3], "OK error");
    assert_eq!(results[4], "OK error");
    assert_eq!(results[5], "OK wrong-type-argument");
    assert_eq!(results[6], "OK wrong-type-argument");
}

#[test]
fn file_name_splitters_bootstrap_match_gnu_files_el() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval(
        r#"
        (list (subrp (symbol-function 'file-name-extension))
              (subrp (symbol-function 'file-name-sans-extension))
              (subrp (symbol-function 'file-name-base))
              (subrp (symbol-function 'file-name-parent-directory))
              (subrp (symbol-function 'file-name-split)))
        (file-name-extension "/home/user/test.el")
        (file-name-extension "/home/user/test.el" t)
        (file-name-extension "no_ext" t)
        (file-name-sans-extension "/home/user/test.el")
        (file-name-base "/home/user/test.el")
        (file-name-parent-directory "/foo/bar")
        (file-name-parent-directory "/foo/")
        (file-name-parent-directory "/")
        (file-name-parent-directory "foo/bar")
        (file-name-parent-directory "foo")
        (file-name-parent-directory "//usr")
        (file-name-split "/foo/bar")
        (file-name-split "/")
        (file-name-split "foo/")
        (file-name-split "")
        "#,
    );
    assert_eq!(results[0], "OK (nil nil nil nil nil)");
    assert_eq!(results[1], r#"OK "el""#);
    assert_eq!(results[2], r#"OK ".el""#);
    assert_eq!(results[3], r#"OK """#);
    assert_eq!(results[4], r#"OK "/home/user/test""#);
    assert_eq!(results[5], r#"OK "test""#);
    assert_eq!(results[6], r#"OK "/foo/""#);
    assert_eq!(results[7], r#"OK "/""#);
    assert_eq!(results[8], "OK nil");
    assert_eq!(results[9], r#"OK "foo/""#);
    assert_eq!(results[10], r#"OK "./""#);
    assert_eq!(results[11], r#"OK "/""#);
    assert_eq!(results[12], r#"OK ("" "foo" "bar")"#);
    assert_eq!(results[13], r#"OK ("" "" "")"#);
    assert_eq!(results[14], r#"OK ("foo" "")"#);
    assert_eq!(results[15], "OK nil");
}

#[test]
fn file_name_splitters_bootstrap_error_shapes_match_gnu_files_el() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval(
        r#"
        (condition-case err (file-name-extension 'x) (error (car err)))
        (condition-case err (file-name-extension "x" nil nil) (error (car err)))
        (condition-case err (file-name-sans-extension 'x) (error (car err)))
        (condition-case err (file-name-base 'x) (error (car err)))
        (condition-case err (file-name-parent-directory 'x) (error (car err)))
        (condition-case err (file-name-split 'x) (error (car err)))
        "#,
    );
    assert_eq!(results[0], "OK wrong-type-argument");
    assert_eq!(results[1], "OK wrong-number-of-arguments");
    assert_eq!(results[2], "OK wrong-type-argument");
    assert_eq!(results[3], "OK wrong-type-argument");
    assert_eq!(results[4], "OK wrong-type-argument");
    assert_eq!(results[5], "OK wrong-type-argument");
}

#[test]
fn file_name_sans_versions_bootstrap_matches_gnu_files_el() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval(
        r#"
        (subrp (symbol-function 'file-name-sans-versions))
        (file-name-sans-versions "foo.~12~")
        (file-name-sans-versions "foo.~12~.~3~")
        (file-name-sans-versions "foo.~~")
        (file-name-sans-versions "foo.~12~" t)
        "#,
    );
    assert_eq!(results[0], "OK nil");
    assert_eq!(results[1], r#"OK "foo""#);
    assert_eq!(results[2], r#"OK "foo.~12~""#);
    assert_eq!(results[3], r#"OK "foo.~""#);
    assert_eq!(results[4], r#"OK "foo.~12~""#);
}

#[test]
fn file_name_sans_versions_bootstrap_error_shapes_match_gnu_files_el() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval(
        r#"
        (condition-case err (file-name-sans-versions 'x) (error (car err)))
        (condition-case err (file-name-sans-versions "x" nil nil) (error (car err)))
        "#,
    );
    assert_eq!(results[0], "OK wrong-type-argument");
    assert_eq!(results[1], "OK wrong-number-of-arguments");
}

#[test]
fn file_name_misc_bootstrap_matches_gnu_files_el() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval(
        r##"
        (list (subrp (symbol-function 'convert-standard-filename))
              (subrp (symbol-function 'backup-file-name-p))
              (subrp (symbol-function 'auto-save-file-name-p))
              (subrp (symbol-function 'abbreviate-file-name)))
        (backup-file-name-p "foo.~12~")
        (backup-file-name-p "foo.txt")
        (auto-save-file-name-p "#foo#")
        (auto-save-file-name-p "foo.txt")
        (let* ((home (expand-file-name "~"))
               (under (concat home "/project")))
          (list (equal (abbreviate-file-name home) "~")
                (equal (abbreviate-file-name under) "~/project")
                (abbreviate-file-name "/tmp/x")))
        (convert-standard-filename "/tmp/x")
        (convert-standard-filename 'x)
        (convert-standard-filename 42)
        "##,
    );
    assert_eq!(results[0], "OK (nil nil nil nil)");
    assert_eq!(results[1], "OK 7");
    assert_eq!(results[2], "OK nil");
    assert_eq!(results[3], "OK 0");
    assert_eq!(results[4], "OK nil");
    assert_eq!(results[5], r#"OK (t t "/tmp/x")"#);
    assert_eq!(results[6], r#"OK "/tmp/x""#);
    assert_eq!(results[7], "OK x");
    assert_eq!(results[8], "OK 42");
}

#[test]
fn file_name_misc_bootstrap_error_shapes_match_gnu_files_el() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval(
        r#"
        (condition-case err (backup-file-name-p 'x) (error (car err)))
        (condition-case err (auto-save-file-name-p 'x) (error (car err)))
        (condition-case err (abbreviate-file-name 'x) (error (car err)))
        (condition-case err (convert-standard-filename) (error (car err)))
        (condition-case err (convert-standard-filename nil nil) (error (car err)))
        "#,
    );
    assert_eq!(results[0], "OK wrong-type-argument");
    assert_eq!(results[1], "OK wrong-type-argument");
    assert_eq!(results[2], "OK wrong-type-argument");
    assert_eq!(results[3], "OK wrong-number-of-arguments");
    assert_eq!(results[4], "OK wrong-number-of-arguments");
}

#[test]
fn test_builtin_file_name_concat_strict_types() {
    crate::test_utils::init_test_tracing();
    let result = builtin_file_name_concat(vec![Value::NIL, Value::string("bar")]);
    assert_eq!(result.unwrap().as_str(), Some("bar"));

    let result = builtin_file_name_concat(vec![Value::symbol("foo"), Value::string("bar")]);
    assert!(result.is_err());
}

#[test]
fn test_builtin_path_predicates() {
    crate::test_utils::init_test_tracing();
    let result = builtin_file_name_absolute_p(vec![Value::string("/tmp")]);
    assert_eq!(result.unwrap(), Value::T);

    let result = builtin_file_name_absolute_p(vec![Value::string("tmp")]);
    assert_eq!(result.unwrap(), Value::NIL);

    let result = builtin_directory_name_p(vec![Value::string("foo/")]);
    assert_eq!(result.unwrap(), Value::T);

    let result = builtin_directory_name_p(vec![Value::string("foo")]);
    assert_eq!(result.unwrap(), Value::NIL);

    let base = std::env::temp_dir().join("neovm_builtin_directory_empty_p");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let file = base.join("entry");
    fs::write(&file, "x").unwrap();

    fs::remove_file(&file).unwrap();

    fs::remove_dir_all(&base).unwrap();
}

#[test]
fn test_builtin_path_predicates_strict_types() {
    crate::test_utils::init_test_tracing();
    let result = builtin_file_name_absolute_p(vec![Value::symbol("foo")]);
    assert!(result.is_err());

    let result = builtin_directory_name_p(vec![Value::NIL]);
    assert!(result.is_err());
}

#[test]
fn test_builtin_file_predicates_strict_types() {
    crate::test_utils::init_test_tracing();
    assert!(call_fileio_builtin!(builtin_file_exists_p, vec![Value::NIL]).is_err());
    assert!(call_fileio_builtin!(builtin_file_readable_p, vec![Value::NIL]).is_err());
    assert!(call_fileio_builtin!(builtin_file_writable_p, vec![Value::NIL]).is_err());
    assert!(call_fileio_builtin!(builtin_file_directory_p, vec![Value::NIL]).is_err());
    assert!(call_fileio_builtin!(builtin_file_regular_p, vec![Value::NIL]).is_err());
    assert!(call_fileio_builtin!(builtin_file_symlink_p, vec![Value::NIL]).is_err());
    assert!(call_fileio_builtin!(builtin_file_name_case_insensitive_p, vec![Value::NIL]).is_err());
    assert!(
        call_fileio_builtin!(
            builtin_file_newer_than_file_p,
            vec![Value::NIL, Value::string("/tmp")]
        )
        .is_err()
    );
    assert!(
        call_fileio_builtin!(
            builtin_file_newer_than_file_p,
            vec![Value::string("/tmp"), Value::NIL]
        )
        .is_err()
    );
}

#[test]
fn test_eval_file_predicates_respect_default_directory() {
    crate::test_utils::init_test_tracing();
    let dir = std::env::temp_dir().join("neovm_fileio_eval_default_dir");
    let subdir = dir.join("subdir");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&subdir).expect("create test subdir");

    let mut eval = Context::new();
    eval.set_variable("default-directory", Value::string(dir.to_string_lossy()));

    let is_dir = builtin_file_directory_p(&mut eval, vec![Value::string("subdir")])
        .expect("file-directory-p eval");
    assert!(is_dir.is_truthy());

    let exists = builtin_file_exists_p(&mut eval, vec![Value::string("subdir")])
        .expect("file-exists-p eval");
    assert!(exists.is_truthy());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_file_name_case_insensitive_eval_respects_default_directory() {
    crate::test_utils::init_test_tracing();
    let dir = std::env::temp_dir().join("neovm_fileio_case_insensitive_eval");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create test dir");
    let file = dir.join("alpha.txt");
    fs::write(&file, b"x").expect("create test file");

    let absolute = call_fileio_builtin!(
        builtin_file_name_case_insensitive_p,
        vec![Value::string(file.to_string_lossy())]
    )
    .expect("absolute case-insensitive query");

    let mut eval = Context::new();
    eval.set_variable(
        "default-directory",
        Value::string(format!("{}/", dir.to_string_lossy())),
    );
    let relative =
        builtin_file_name_case_insensitive_p(&mut eval, vec![Value::string("alpha.txt")])
            .expect("relative case-insensitive query");
    assert_eq!(relative, absolute);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_builtin_file_newer_than_file_p_semantics() {
    crate::test_utils::init_test_tracing();
    let dir = std::env::temp_dir().join("neovm-file-newer-than-file-p");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create test dir");

    let old = dir.join("old.txt");
    let new = dir.join("new.txt");
    let missing = dir.join("missing.txt");

    fs::write(&old, b"old").expect("write old file");
    std::thread::sleep(std::time::Duration::from_millis(1200));
    fs::write(&new, b"new").expect("write new file");

    assert_eq!(
        call_fileio_builtin!(
            builtin_file_newer_than_file_p,
            vec![
                Value::string(new.to_string_lossy()),
                Value::string(old.to_string_lossy()),
            ]
        )
        .expect("newer"),
        Value::T
    );
    assert_eq!(
        call_fileio_builtin!(
            builtin_file_newer_than_file_p,
            vec![
                Value::string(old.to_string_lossy()),
                Value::string(new.to_string_lossy()),
            ]
        )
        .expect("older"),
        Value::NIL
    );
    assert_eq!(
        call_fileio_builtin!(
            builtin_file_newer_than_file_p,
            vec![
                Value::string(missing.to_string_lossy()),
                Value::string(old.to_string_lossy()),
            ]
        )
        .expect("missing first"),
        Value::NIL
    );
    assert_eq!(
        call_fileio_builtin!(
            builtin_file_newer_than_file_p,
            vec![
                Value::string(old.to_string_lossy()),
                Value::string(missing.to_string_lossy()),
            ]
        )
        .expect("missing second"),
        Value::T
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_file_newer_than_file_p_eval_respects_default_directory() {
    crate::test_utils::init_test_tracing();
    let dir = std::env::temp_dir().join("neovm-file-newer-than-file-p-eval");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create test dir");

    let old = dir.join("old.txt");
    let new = dir.join("new.txt");
    fs::write(&old, b"old").expect("write old file");
    std::thread::sleep(std::time::Duration::from_millis(1200));
    fs::write(&new, b"new").expect("write new file");

    let mut eval = Context::new();
    eval.set_variable(
        "default-directory",
        Value::string(format!("{}/", dir.to_string_lossy())),
    );

    let result = builtin_file_newer_than_file_p(
        &mut eval,
        vec![Value::string("new.txt"), Value::string("old.txt")],
    )
    .expect("relative newer check");
    assert_eq!(result, Value::T);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_builtin_set_file_times_semantics() {
    crate::test_utils::init_test_tracing();
    let dir = std::env::temp_dir().join("neovm-set-file-times");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create test dir");

    let older = dir.join("older.txt");
    let newer = dir.join("newer.txt");
    fs::write(&older, b"older").expect("write older");
    fs::write(&newer, b"newer").expect("write newer");

    assert_eq!(
        call_fileio_builtin!(
            builtin_set_file_times,
            vec![Value::string(older.to_string_lossy()), Value::fixnum(0),]
        )
        .expect("set-file-times"),
        Value::T
    );
    assert_eq!(
        call_fileio_builtin!(
            builtin_set_file_times,
            vec![Value::string(newer.to_string_lossy()), Value::NIL, Value::T,]
        )
        .expect("set-file-times with flag"),
        Value::T
    );
    assert_eq!(
        call_fileio_builtin!(
            builtin_file_newer_than_file_p,
            vec![
                Value::string(newer.to_string_lossy()),
                Value::string(older.to_string_lossy()),
            ]
        )
        .expect("newer-than"),
        Value::T
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_set_file_times_eval_respects_default_directory() {
    crate::test_utils::init_test_tracing();
    let dir = std::env::temp_dir().join("neovm-set-file-times-eval");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create test dir");
    let file = dir.join("alpha.txt");
    fs::write(&file, b"alpha").expect("write file");

    let mut eval = Context::new();
    eval.set_variable(
        "default-directory",
        Value::string(format!("{}/", dir.to_string_lossy())),
    );

    assert_eq!(
        builtin_set_file_times(
            &mut eval,
            vec![Value::string("alpha.txt"), Value::fixnum(0)],
        )
        .expect("eval set-file-times"),
        Value::T
    );
    let mtime = fs::metadata(&file)
        .expect("metadata")
        .modified()
        .expect("modified")
        .duration_since(UNIX_EPOCH)
        .expect("epoch")
        .as_secs();
    assert_eq!(mtime, 0);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_visited_file_modtime_state_builtins_use_current_buffer_file_name() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let current = eval.buffers.current_buffer_id().expect("current buffer");

    assert_eq!(
        builtin_verify_visited_file_modtime(&mut eval, vec![Value::make_buffer(current)])
            .expect("verify-visited-file-modtime"),
        Value::T
    );

    let missing = builtin_set_visited_file_modtime(&mut eval, vec![Value::NIL])
        .expect_err("missing visited file should signal");
    match missing {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(sig.data, vec![Value::symbol("stringp"), Value::NIL]);
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    eval.buffers
        .set_buffer_file_name(current, Some("/tmp/neovm-visited-file.txt".to_string()))
        .expect("buffer file name should set");
    assert_eq!(
        builtin_set_visited_file_modtime(&mut eval, vec![Value::NIL])
            .expect("set-visited-file-modtime"),
        Value::NIL
    );
}

#[test]
fn test_default_file_modes_round_trip() {
    crate::test_utils::init_test_tracing();
    let original = builtin_default_file_modes(vec![])
        .expect("default-file-modes")
        .as_int()
        .expect("default-file-modes int");
    assert_eq!(
        builtin_set_default_file_modes(vec![Value::fixnum(0o700)]).expect("set-default-file-modes"),
        Value::NIL
    );
    assert_eq!(
        builtin_default_file_modes(vec![])
            .expect("default-file-modes after set")
            .as_int(),
        Some(0o700)
    );
    let _ = builtin_set_default_file_modes(vec![Value::fixnum(original)]);
}

#[test]
fn test_default_file_modes_argument_errors() {
    crate::test_utils::init_test_tracing();
    assert!(builtin_set_default_file_modes(vec![]).is_err());
    assert!(builtin_default_file_modes(vec![Value::fixnum(1)]).is_err());
    assert!(builtin_set_default_file_modes(vec![Value::NIL]).is_err());
}

#[test]
fn test_builtin_substitute_in_file_name() {
    crate::test_utils::init_test_tracing();
    let home = std::env::var("HOME").unwrap_or_default();
    let mut ev = Context::new();
    let result =
        builtin_substitute_in_file_name(&mut ev, vec![Value::string("$HOME/foo")]).unwrap();
    assert_eq!(result.as_str(), Some(format!("{home}/foo").as_str()));
}

#[test]
fn test_builtin_substitute_in_file_name_strict_type() {
    crate::test_utils::init_test_tracing();
    let mut ev = Context::new();
    let result = builtin_substitute_in_file_name(&mut ev, vec![Value::symbol("foo")]);
    assert!(result.is_err());
}

#[test]
fn test_builtin_wrong_arg_count() {
    crate::test_utils::init_test_tracing();
    // expand-file-name needs at least 1 arg
    let result = call_fileio_builtin!(builtin_expand_file_name, vec![]);
    assert!(result.is_err());

    // file-exists-p needs exactly 1 arg
    let result = call_fileio_builtin!(builtin_file_exists_p, vec![]);
    assert!(result.is_err());
}

// -----------------------------------------------------------------------
// Context-dependent builtins
// -----------------------------------------------------------------------

#[test]
fn test_insert_file_contents_and_write_region() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let dir = std::env::temp_dir().join("neovm_eval_fileio_test");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("eval_test.txt");
    let path_str = path.to_string_lossy().to_string();

    // Write a test file to disk
    write_string_to_file("hello from file", &path_str, false).unwrap();

    let mut eval = Context::new();

    // insert-file-contents
    let result = builtin_insert_file_contents(&mut eval, vec![Value::string(&path_str)]);
    assert!(result.is_ok());

    // Check that the buffer now contains the text
    let buf = eval.buffers.current_buffer().unwrap();
    assert_eq!(buf.buffer_string(), "hello from file");

    // write-region: write entire buffer to a new file
    let out_path = dir.join("output.txt");
    let out_str = out_path.to_string_lossy().to_string();
    let result = builtin_write_region(
        &mut eval,
        vec![Value::NIL, Value::NIL, Value::string(&out_str)],
    );
    assert!(result.is_ok());

    let written = read_file_contents(&out_str).unwrap();
    assert_eq!(written, "hello from file");

    // Clean up
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_insert_file_contents_visit_sets_file_name_and_clears_modified() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let dir = std::env::temp_dir().join("neovm_eval_insert_file_contents_visit");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let path = dir.join("visit.txt");
    let path_str = path.to_string_lossy().to_string();
    write_string_to_file("visited text", &path_str, false).unwrap();

    let mut eval = Context::new();
    let result = builtin_insert_file_contents(&mut eval, vec![Value::string(&path_str), Value::T])
        .expect("insert-file-contents with visit should succeed");
    let parts = list_to_vec(&result).expect("insert-file-contents should return list");
    assert_eq!(parts[0].as_str(), Some(path_str.as_str()));

    let buf = eval.buffers.current_buffer().expect("current buffer");
    assert_eq!(buf.buffer_string(), "visited text");
    assert_eq!(buf.file_name_owned().as_deref(), Some(path_str.as_str()));
    assert!(!buf.is_modified());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_insert_file_contents_visit_rejects_partial_and_nonempty_visits() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let dir = std::env::temp_dir().join("neovm_eval_insert_file_contents_visit_errors");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let path = dir.join("visit.txt");
    let path_str = path.to_string_lossy().to_string();
    write_string_to_file("visited text", &path_str, false).unwrap();

    let mut eval_partial = Context::new();
    let partial = builtin_insert_file_contents(
        &mut eval_partial,
        vec![Value::string(&path_str), Value::T, Value::fixnum(0)],
    )
    .expect_err("visit with BEG should reject");
    match partial {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string("Attempt to visit less than an entire file")]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let mut eval_nonempty = Context::new();
    eval_nonempty
        .buffers
        .current_buffer_mut()
        .expect("current buffer")
        .insert("x");
    let nonempty =
        builtin_insert_file_contents(&mut eval_nonempty, vec![Value::string(&path_str), Value::T])
            .expect_err("visit in non-empty buffer without replace should reject");
    match nonempty {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "error");
            assert_eq!(
                sig.data,
                vec![Value::string(
                    "Cannot do file visiting in a non-empty buffer"
                )]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn insert_file_contents_visit_decodes_text_enriched_formats() {
    crate::test_utils::init_test_tracing();

    let dir = std::env::temp_dir().join("neovm_eval_insert_file_contents_text_enriched");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let path = dir.join("hello.enriched");
    fs::write(
        &path,
        concat!(
            "Content-Type: text/enriched\n",
            "\n",
            "<x-color><param>orange red</param>hello</x-color>\n",
        ),
    )
    .unwrap();

    let path_str = path.to_string_lossy().to_string();

    let mut eval = Context::new();
    eval.eval_str(
        r#"(progn
             (defalias 'format-decode
               (lambda (_format len _visit)
                 (delete-region (point-min) (point-max))
                 (insert "hello\n")
                 (setq buffer-file-format '(text/enriched))
                 6))
             (setq after-insert-file-functions
                   (list (lambda (len)
                           (setq enriched-mode t)
                           len))))"#,
    )
    .expect("stub format decode setup");

    builtin_insert_file_contents(&mut eval, vec![Value::string(&path_str), Value::T])
        .expect("insert-file-contents should decode text/enriched");

    assert_eq!(
        format_eval_result(&eval.eval_str("buffer-file-format")),
        "OK (text/enriched)"
    );
    assert_eq!(format_eval_result(&eval.eval_str("enriched-mode")), "OK t");
    let buf = eval.buffers.current_buffer().expect("current buffer");
    assert_eq!(buf.buffer_string(), "hello\n");
    assert_eq!(buf.file_name_owned().as_deref(), Some(path_str.as_str()));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_insert_file_contents_beg_end_semantics() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let dir = std::env::temp_dir().join("neovm_eval_insert_file_contents_beg_end");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("slice.txt");
    let path_str = path.to_string_lossy().to_string();
    write_string_to_file("abcdef", &path_str, false).unwrap();

    let mut eval_slice = Context::new();
    let inserted = builtin_insert_file_contents(
        &mut eval_slice,
        vec![
            Value::string(&path_str),
            Value::NIL,
            Value::fixnum(2),
            Value::fixnum(4),
        ],
    )
    .expect("insert-file-contents 2..4 should succeed");
    assert_eq!(
        list_to_vec(&inserted).unwrap()[1],
        Value::fixnum(2),
        "inserted char count should match slice length"
    );
    assert_eq!(
        eval_slice.buffers.current_buffer().unwrap().buffer_string(),
        "cd",
        "slice 2..4 should insert 'cd'"
    );

    let mut eval_empty = Context::new();
    let inserted_zero = builtin_insert_file_contents(
        &mut eval_empty,
        vec![
            Value::string(&path_str),
            Value::NIL,
            Value::fixnum(4),
            Value::fixnum(2),
        ],
    )
    .expect("insert-file-contents start>end should succeed with empty insertion");
    assert_eq!(list_to_vec(&inserted_zero).unwrap()[1], Value::fixnum(0));
    assert_eq!(
        eval_empty.buffers.current_buffer().unwrap().buffer_string(),
        ""
    );

    let mut eval_tail = Context::new();
    let inserted_tail = builtin_insert_file_contents(
        &mut eval_tail,
        vec![
            Value::string(&path_str),
            Value::NIL,
            Value::fixnum(2),
            Value::fixnum(99),
        ],
    )
    .expect("insert-file-contents end beyond file should clamp");
    assert_eq!(list_to_vec(&inserted_tail).unwrap()[1], Value::fixnum(4));
    assert_eq!(
        eval_tail.buffers.current_buffer().unwrap().buffer_string(),
        "cdef"
    );

    let mut eval_bad = Context::new();
    let bad_offset = builtin_insert_file_contents(
        &mut eval_bad,
        vec![
            Value::string(&path_str),
            Value::NIL,
            Value::fixnum(-1),
            Value::fixnum(2),
        ],
    )
    .expect_err("negative BEG should reject with file-offset predicate");
    match bad_offset {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-type-argument");
            assert_eq!(
                sig.data,
                vec![Value::symbol("file-offset"), Value::fixnum(-1)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_insert_file_contents_and_write_region_arity_bounds() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let dir = std::env::temp_dir().join("neovm_eval_fileio_arity_bounds");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let file_path = dir.join("arity.txt");
    let file_str = file_path.to_string_lossy().to_string();
    write_string_to_file("", &file_str, false).unwrap();

    let mut eval_insert_ok = Context::new();
    let insert_ok = builtin_insert_file_contents(
        &mut eval_insert_ok,
        vec![
            Value::string(&file_str),
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL,
        ],
    )
    .expect("5-arg insert-file-contents should succeed");
    assert_eq!(list_to_vec(&insert_ok).unwrap()[1], Value::fixnum(0));

    let mut eval_insert_bad = Context::new();
    let insert_bad = builtin_insert_file_contents(
        &mut eval_insert_bad,
        vec![
            Value::string(&file_str),
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL,
        ],
    )
    .expect_err("6-arg insert-file-contents should fail");
    match insert_bad {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("insert-file-contents"), Value::fixnum(6)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let out_path = dir.join("arity-out.txt");
    let out_str = out_path.to_string_lossy().to_string();

    let mut eval_write_ok = Context::new();
    eval_write_ok
        .buffers
        .current_buffer_mut()
        .unwrap()
        .insert("x");
    builtin_write_region(
        &mut eval_write_ok,
        vec![
            Value::NIL,
            Value::NIL,
            Value::string(&out_str),
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL,
        ],
    )
    .expect("7-arg write-region should succeed");

    let mut eval_write_bad = Context::new();
    eval_write_bad
        .buffers
        .current_buffer_mut()
        .unwrap()
        .insert("x");
    let write_bad = builtin_write_region(
        &mut eval_write_bad,
        vec![
            Value::NIL,
            Value::NIL,
            Value::string(&out_str),
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL,
        ],
    )
    .expect_err("8-arg write-region should fail");
    match write_bad {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("write-region"), Value::fixnum(8)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_find_file_noselect_arity_bounds() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let dir = std::env::temp_dir().join("neovm_eval_find_file_noselect_arity");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let file_path = dir.join("arity.txt");
    let file_str = file_path.to_string_lossy().to_string();
    write_string_to_file("", &file_str, false).unwrap();

    let mut eval_ok = Context::new();
    let ok = builtin_find_file_noselect(
        &mut eval_ok,
        vec![Value::string(&file_str), Value::NIL, Value::NIL, Value::NIL],
    )
    .expect("4-arg find-file-noselect should succeed");
    assert!(ok.is_buffer());

    let mut eval_bad = Context::new();
    let bad = builtin_find_file_noselect(
        &mut eval_bad,
        vec![
            Value::string(&file_str),
            Value::NIL,
            Value::NIL,
            Value::NIL,
            Value::NIL,
        ],
    )
    .expect_err("5-arg find-file-noselect should fail");
    match bad {
        Flow::Signal(sig) => {
            assert_eq!(sig.symbol_name(), "wrong-number-of-arguments");
            assert_eq!(
                sig.data,
                vec![Value::symbol("find-file-noselect"), Value::fixnum(5)]
            );
        }
        other => panic!("unexpected flow: {other:?}"),
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_eval_fileio_relative_paths_respect_default_directory() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let dir = std::env::temp_dir().join("neovm_eval_fileio_relative");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let alpha_path = dir.join("alpha.txt");
    fs::write(&alpha_path, "alpha\n").unwrap();
    let alpha_str = alpha_path.to_string_lossy().to_string();
    let out_path = dir.join("out.txt");
    let out_str = out_path.to_string_lossy().to_string();
    let default_dir = format!("{}/", dir.to_string_lossy());

    let mut eval_insert = Context::new();
    eval_insert.set_variable("default-directory", Value::string(&default_dir));
    let inserted =
        builtin_insert_file_contents(&mut eval_insert, vec![Value::string("alpha.txt")]).unwrap();
    let inserted_parts = list_to_vec(&inserted).unwrap();
    assert_eq!(inserted_parts[0].as_str(), Some(alpha_str.as_str()));
    let ibuf = eval_insert.buffers.current_buffer().unwrap();
    assert_eq!(ibuf.buffer_string(), "alpha\n");

    let mut eval_write = Context::new();
    eval_write.set_variable("default-directory", Value::string(&default_dir));
    eval_write
        .buffers
        .current_buffer_mut()
        .unwrap()
        .insert("neo");
    builtin_write_region(
        &mut eval_write,
        vec![Value::NIL, Value::NIL, Value::string("out.txt")],
    )
    .unwrap();
    assert_eq!(read_file_contents(&out_str).unwrap(), "neo");

    let mut eval_find = Context::new();
    eval_find.set_variable("default-directory", Value::string(&default_dir));
    let found =
        builtin_find_file_noselect(&mut eval_find, vec![Value::string("alpha.txt")]).unwrap();
    if !found.is_buffer() {
        panic!("expected Buffer");
    };
    let buf_id = found.as_buffer_id().unwrap();
    let fbuf = eval_find.buffers.get(buf_id).unwrap();
    assert_eq!(fbuf.buffer_string(), "alpha\n");
    assert_eq!(fbuf.file_name_owned().as_deref(), Some(alpha_str.as_str()));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_write_region_bounds_and_order_semantics() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let dir = std::env::temp_dir().join("neovm_eval_write_region_bounds");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let out_path = dir.join("out.txt");
    let out_str = out_path.to_string_lossy().to_string();

    let mut eval = Context::new();
    eval.buffers.current_buffer_mut().unwrap().insert("abc");
    let current = Value::make_buffer(eval.buffers.current_buffer().unwrap().id);

    builtin_write_region(
        &mut eval,
        vec![Value::fixnum(3), Value::fixnum(1), Value::string(&out_str)],
    )
    .expect("write-region should accept reversed in-range bounds");
    assert_eq!(read_file_contents(&out_str).unwrap(), "ab");

    for (start, end) in [(-1, 2), (1, -1), (1, 9)] {
        let err = builtin_write_region(
            &mut eval,
            vec![
                Value::fixnum(start),
                Value::fixnum(end),
                Value::string(&out_str),
            ],
        )
        .expect_err("out-of-range bounds should signal");
        match err {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "args-out-of-range");
                assert_eq!(
                    sig.data,
                    vec![current, Value::fixnum(start), Value::fixnum(end)]
                );
            }
            other => panic!("unexpected flow: {other:?}"),
        }
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_write_region_visit_sets_file_name_and_clears_modified() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let dir = std::env::temp_dir().join("neovm_eval_write_region_visit");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let out_path = dir.join("visited.txt");
    let out_str = out_path.to_string_lossy().to_string();

    let mut eval = Context::new();
    eval.buffers.current_buffer_mut().unwrap().insert("neo");
    assert!(eval.buffers.current_buffer().unwrap().is_modified());

    builtin_write_region(
        &mut eval,
        vec![
            Value::NIL,
            Value::NIL,
            Value::string(&out_str),
            Value::NIL,
            Value::T,
        ],
    )
    .expect("write-region with visit should succeed");

    let buf = eval.buffers.current_buffer().expect("current buffer");
    assert_eq!(buf.file_name_owned().as_deref(), Some(out_str.as_str()));
    assert!(!buf.is_modified());
    assert_eq!(read_file_contents(&out_str).unwrap(), "neo");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_write_region_string_start_numeric_append_and_visit_string_semantics() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let dir = std::env::temp_dir().join("neovm_eval_write_region_string_append");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let out_path = dir.join("out.txt");
    let out_str = out_path.to_string_lossy().to_string();
    let visit_path = dir.join("visit.txt");
    let visit_str = visit_path.to_string_lossy().to_string();
    write_string_to_file("abcde", &out_str, false).unwrap();

    let mut eval = Context::new();
    eval.buffers
        .current_buffer_mut()
        .unwrap()
        .insert("buffer text");
    assert!(eval.buffers.current_buffer().unwrap().is_modified());

    builtin_write_region(
        &mut eval,
        vec![
            Value::string("XY"),
            Value::NIL,
            Value::string(&out_str),
            Value::fixnum(2),
            Value::string(&visit_str),
        ],
    )
    .expect("write-region string start with numeric append should succeed");

    assert_eq!(read_file_contents(&out_str).unwrap(), "abXYe");
    let buf = eval.buffers.current_buffer().expect("current buffer");
    assert_eq!(buf.file_name_owned().as_deref(), Some(visit_str.as_str()));
    assert!(!buf.is_modified());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_write_region_preserves_unibyte_raw_bytes() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let dir = std::env::temp_dir().join("neovm_eval_write_region_raw_bytes");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let out_path = dir.join("buffer.bin");
    let out_str = out_path.to_string_lossy().to_string();
    let out2_path = dir.join("string.bin");
    let out2_str = out2_path.to_string_lossy().to_string();

    let mut eval = Context::new();
    {
        let buf = eval.buffers.current_buffer_mut().expect("current buffer");
        buf.set_multibyte_value(false);
        buf.insert_lisp_string(&crate::heap_types::LispString::from_unibyte(vec![0xFF]));
    }

    builtin_write_region(
        &mut eval,
        vec![Value::NIL, Value::NIL, Value::string(&out_str)],
    )
    .expect("write-region should preserve raw buffer bytes");
    assert_eq!(fs::read(&out_path).unwrap(), vec![0xFF]);

    builtin_write_region(
        &mut eval,
        vec![
            Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![
                0xFE, 0xFF,
            ])),
            Value::NIL,
            Value::string(&out2_str),
        ],
    )
    .expect("write-region string payload should preserve raw bytes");
    assert_eq!(fs::read(&out2_path).unwrap(), vec![0xFE, 0xFF]);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_find_file_noselect() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;

    let dir = std::env::temp_dir().join("neovm_findfile_test");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("findme.txt");
    let path_str = path.to_string_lossy().to_string();

    write_string_to_file("file content here", &path_str, false).unwrap();

    let mut eval = Context::new();

    // find-file-noselect
    let result = builtin_find_file_noselect(&mut eval, vec![Value::string(&path_str)]);
    assert!(result.is_ok());
    let buf_val = result.unwrap();
    match buf_val.kind() {
        ValueKind::Veclike(VecLikeType::Buffer) => {
            let buf = eval.buffers.get(buf_val.as_buffer_id().unwrap()).unwrap();
            assert_eq!(buf.buffer_string(), "file content here");
            assert!(buf.file_name_value().is_string());
            assert!(!buf.is_modified());
        }
        other => panic!("Expected Buffer, got {:?}", buf_val),
    }

    // Calling again with the same file should return the same buffer
    let result2 = builtin_find_file_noselect(&mut eval, vec![Value::string(&path_str)]);
    assert!(result2.is_ok());
    let buf_val2 = result2.unwrap();
    assert!(buf_val.is_buffer() && buf_val2.is_buffer());
    assert_eq!(buf_val, buf_val2);

    // Clean up
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_find_file_noselect_nonexistent() {
    crate::test_utils::init_test_tracing();
    use super::super::eval::Context;
    use crate::emacs_core::value::{ValueKind, VecLikeType};

    let mut eval = Context::new();
    let result = builtin_find_file_noselect(
        &mut eval,
        vec![Value::string("/tmp/neovm_nonexistent_file_xyz.txt")],
    );
    assert!(result.is_ok());
    let nonexistent_buf = result.unwrap();
    match nonexistent_buf.kind() {
        ValueKind::Veclike(VecLikeType::Buffer) => {
            let buf = eval
                .buffers
                .get(nonexistent_buf.as_buffer_id().unwrap())
                .unwrap();
            // Buffer should be empty for a nonexistent file
            assert_eq!(buf.buffer_string(), "");
            assert!(buf.file_name_value().is_string());
        }
        other => panic!("Expected Buffer, got {:?}", nonexistent_buf),
    }
}

#[test]
fn file_local_name_bootstrap_matches_gnu_files_el() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval(
        r#"
        (subrp (symbol-function 'file-local-name))
        (file-local-name "/tmp/local")
        (file-local-name "/ssh:user@host#22:/tmp/file")
        "#,
    );
    assert_eq!(results[0], "OK nil");
    assert_eq!(results[1], r#"OK "/tmp/local""#);
    assert_eq!(results[2], r#"OK "/ssh:user@host#22:/tmp/file""#);
}

#[test]
fn file_local_name_bootstrap_error_shapes_match_gnu_files_el() {
    crate::test_utils::init_test_tracing();
    let results = bootstrap_eval(
        r#"
        (condition-case err (file-local-name nil) (error (car err)))
        "#,
    );
    assert_eq!(results[0], "OK wrong-type-argument");
}
