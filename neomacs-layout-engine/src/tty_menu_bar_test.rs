use super::*;
use neovm_core::emacs_core::Context;
use neovm_core::heap_types::LispString;

#[test]
fn extract_menu_label_preserves_raw_unibyte_strings() {
    let mut eval = Context::new();
    eval.setup_thread_locals();
    let raw = Value::heap_string(LispString::from_unibyte(vec![0xFF]));
    let expected = raw
        .as_runtime_string_owned()
        .expect("runtime string for raw label");

    let plain = Value::cons(raw, Value::symbol("ignore"));
    assert_eq!(extract_menu_label(&plain), Some(expected.clone()));

    let menu_item = Value::list(vec![
        Value::symbol("menu-item"),
        raw,
        Value::symbol("ignore"),
    ]);
    assert_eq!(extract_menu_label(&menu_item), Some(expected));
}
