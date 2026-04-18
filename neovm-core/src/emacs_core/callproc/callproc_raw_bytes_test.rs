use super::*;
use crate::emacs_core::Context;
use crate::heap_types::LispString;
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};

fn find_bin(name: &str) -> String {
    for dir in ["/bin", "/usr/bin", "/run/current-system/sw/bin"] {
        let path = format!("{dir}/{name}");
        if std::path::Path::new(&path).exists() {
            return path;
        }
    }
    if let Ok(output) = std::process::Command::new("which").arg(name).output() {
        if output.status.success() {
            return String::from_utf8_lossy(&output.stdout).trim().to_string();
        }
    }
    name.to_string()
}

#[cfg(unix)]
fn raw_temp_path(label: &[u8]) -> std::path::PathBuf {
    let mut bytes = std::env::temp_dir().as_os_str().as_bytes().to_vec();
    if bytes.last() != Some(&b'/') {
        bytes.push(b'/');
    }
    bytes.extend_from_slice(label);
    std::path::PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

#[cfg(unix)]
fn raw_path_value(path: &std::path::Path) -> Value {
    Value::heap_string(LispString::from_unibyte(
        path.as_os_str().as_bytes().to_vec(),
    ))
}

#[cfg(unix)]
#[test]
fn builtin_call_process_preserves_raw_unibyte_argument_and_output_path_bytes() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let printf = find_bin("printf");
    let out = raw_temp_path(b"neomacs-callproc-out-\xFF");
    let _ = std::fs::remove_file(&out);

    let result = builtin_call_process(
        &mut eval,
        vec![
            Value::string(printf),
            Value::NIL,
            Value::list(vec![Value::keyword(":file"), raw_path_value(&out)]),
            Value::NIL,
            Value::string("%s"),
            Value::heap_string(LispString::from_unibyte(vec![0xFF])),
        ],
    )
    .expect("call-process");

    assert_eq!(result.as_fixnum(), Some(0));
    assert_eq!(std::fs::read(&out).expect("read output"), vec![0xFF]);
    let _ = std::fs::remove_file(&out);
}

#[cfg(unix)]
#[test]
fn builtin_call_process_preserves_raw_unibyte_infile_path_bytes() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let cat = find_bin("cat");
    let infile = raw_temp_path(b"neomacs-callproc-in-\xFF");
    let out = raw_temp_path(b"neomacs-callproc-copy-\xFE");
    let _ = std::fs::remove_file(&infile);
    let _ = std::fs::remove_file(&out);
    std::fs::write(&infile, b"raw infile bytes").expect("write infile");

    let result = builtin_call_process(
        &mut eval,
        vec![
            Value::string(cat),
            raw_path_value(&infile),
            Value::list(vec![Value::keyword(":file"), raw_path_value(&out)]),
            Value::NIL,
        ],
    )
    .expect("call-process");

    assert_eq!(result.as_fixnum(), Some(0));
    assert_eq!(
        std::fs::read(&out).expect("read copied output"),
        b"raw infile bytes"
    );
    let _ = std::fs::remove_file(&infile);
    let _ = std::fs::remove_file(&out);
}

#[cfg(unix)]
#[test]
fn builtin_call_process_region_preserves_raw_unibyte_string_input_bytes() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let cat = find_bin("cat");
    let out = raw_temp_path(b"neomacs-callproc-region-\xFD");
    let _ = std::fs::remove_file(&out);

    let result = builtin_call_process_region(
        &mut eval,
        vec![
            Value::heap_string(LispString::from_unibyte(vec![0xFF, b'A', 0x80])),
            Value::NIL,
            Value::string(cat),
            Value::NIL,
            Value::list(vec![Value::keyword(":file"), raw_path_value(&out)]),
            Value::NIL,
        ],
    )
    .expect("call-process-region");

    assert_eq!(result.as_fixnum(), Some(0));
    assert_eq!(
        std::fs::read(&out).expect("read region output"),
        vec![0xFF, b'A', 0x80]
    );
    let _ = std::fs::remove_file(&out);
}

#[test]
fn shell_command_with_legacy_args_matches_gnu_mapconcat_shape() {
    crate::test_utils::init_test_tracing();
    let command = Value::string("printf %s");
    let extra = Value::string("a b");
    let combined =
        shell_command_with_legacy_args(&command, &[extra]).expect("shell command with args");

    assert_eq!(combined.as_bytes(), b"printf %s a b");
}

#[test]
fn shell_command_with_legacy_args_preserves_raw_unibyte_string_bytes() {
    crate::test_utils::init_test_tracing();
    let command = Value::heap_string(LispString::from_unibyte(vec![0xFF]));
    let extra = Value::heap_string(LispString::from_unibyte(vec![0xFE, b'!']));
    let combined =
        shell_command_with_legacy_args(&command, &[extra]).expect("shell command with args");

    assert!(!combined.is_multibyte());
    assert_eq!(combined.as_bytes(), &[0xFF, b' ', 0xFE, b'!']);
}
