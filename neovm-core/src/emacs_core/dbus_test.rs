use super::*;

#[test]
fn dbus_init_bus_contract() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        builtin_dbus_init_bus(vec![Value::keyword(":session")]).unwrap(),
        Value::fixnum(2)
    );
    let err = builtin_dbus_init_bus(vec![Value::fixnum(1)]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn dbus_get_unique_name_errors_without_connection() {
    crate::test_utils::init_test_tracing();
    let err = builtin_dbus_get_unique_name(vec![Value::keyword(":system")]).unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "dbus-error"),
        other => panic!("expected signal, got {other:?}"),
    }
}

#[test]
fn dbus_message_internal_validates_first_arg() {
    crate::test_utils::init_test_tracing();
    let err = builtin_dbus_message_internal(vec![
        Value::keyword(":session"),
        Value::string("/"),
        Value::string("org.example"),
        Value::string("Ping"),
    ])
    .unwrap_err();
    match err {
        Flow::Signal(sig) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
        other => panic!("expected signal, got {other:?}"),
    }
}
