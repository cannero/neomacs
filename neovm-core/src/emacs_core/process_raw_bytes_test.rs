use super::*;
use crate::emacs_core::Context;
use crate::emacs_core::value::list_to_vec;
use crate::heap_types::LispString;
use std::time::Duration;

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

#[test]
fn builtin_getenv_internal_preserves_raw_unibyte_process_environment_value() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    eval.obarray.set_symbol_value(
        "process-environment",
        Value::list(vec![Value::heap_string(LispString::from_unibyte(
            b"NEOMACS_RAW_ENV=\xFF".to_vec(),
        ))]),
    );

    let result = builtin_getenv_internal(
        &mut eval,
        vec![Value::heap_string(LispString::from_unibyte(
            b"NEOMACS_RAW_ENV".to_vec(),
        ))],
    )
    .expect("getenv-internal should succeed");

    let value = result.as_lisp_string().expect("string result");
    assert!(!value.is_multibyte());
    assert_eq!(value.as_bytes(), &[0xFF]);
}

#[test]
fn builtin_start_process_preserves_raw_unibyte_command_argument_storage() {
    crate::test_utils::init_test_tracing();
    let mut eval = Context::new();
    let echo = find_bin("echo");
    let pid = builtin_start_process(
        &mut eval,
        vec![
            Value::string("raw-start-process"),
            Value::NIL,
            Value::string(echo),
            Value::heap_string(LispString::from_unibyte(vec![0xFF])),
        ],
    )
    .expect("start-process should succeed")
    .as_fixnum()
    .expect("pid");

    let proc = eval
        .processes
        .get(pid as ProcessId)
        .expect("process should exist");
    let command = list_to_vec(&proc.command).expect("process command list");
    let arg = command
        .get(1)
        .and_then(|value| value.as_lisp_string())
        .expect("raw argument should be stored");
    assert!(!arg.is_multibyte());
    assert_eq!(arg.as_bytes(), &[0xFF]);
}

#[test]
fn spawn_child_with_environment_uses_process_environment_list() {
    crate::test_utils::init_test_tracing();
    let shell = find_bin("sh");
    let mut processes = ProcessManager::new();
    let pid = processes.create_process_lisp(
        LispString::from_utf8("raw-env-child"),
        Value::NIL,
        LispString::from_utf8(&shell),
        vec![
            LispString::from_utf8("-c"),
            LispString::from_utf8("printf %s \"$NEOMACS_CHILD_ENV\""),
        ],
    );
    let env = Value::list(vec![Value::heap_string(LispString::from_unibyte(
        b"NEOMACS_CHILD_ENV=from-lisp".to_vec(),
    ))]);

    processes
        .spawn_child_with_environment(pid, false, Some(env))
        .expect("spawn child");

    for _ in 0..20 {
        let ready = processes.wait_for_output(Duration::from_millis(20));
        if ready.contains(&pid) {
            let _ = processes.read_process_output(pid);
        }
        if processes
            .get_output(pid)
            .is_some_and(|output| output == "from-lisp")
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(processes.get_output(pid), Some("from-lisp"));
}
