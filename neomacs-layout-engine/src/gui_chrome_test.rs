use super::*;
use neovm_core::emacs_core::Context;
use neovm_core::emacs_core::load::create_bootstrap_evaluator_cached_with_features;
use neovm_core::heap_types::LispString;

#[test]
fn parse_tool_bar_item_preserves_raw_unibyte_label_and_help() {
    let mut eval = Context::new();
    eval.setup_thread_locals();
    let raw = Value::heap_string(LispString::from_unibyte(vec![0xFF]));
    let expected = raw
        .as_runtime_string_owned()
        .expect("runtime string for raw label");
    let def = Value::list(vec![
        Value::symbol("menu-item"),
        raw,
        Value::symbol("ignore"),
        Value::symbol(":help"),
        raw,
    ]);

    let item = parse_tool_bar_item("raw-item", &def, 0).expect("tool-bar item");
    assert_eq!(item.label, expected);
    assert_eq!(item.help, expected);
}

#[test]
fn collect_gui_menu_bar_items_bootstrap_has_help_menu() {
    let eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("bootstrap evaluator");
    let items = collect_gui_menu_bar_items(&eval);
    assert!(!items.is_empty());
    assert!(items.iter().any(|item| item.key == "help-menu"));
}

#[test]
fn collect_gui_tool_bar_items_after_setup_has_search_item_and_separator() {
    let mut eval = create_bootstrap_evaluator_cached_with_features(&["neomacs"])
        .expect("bootstrap evaluator");
    eval.eval_str("(tool-bar-setup)")
        .expect("run GNU tool-bar setup");
    let items = collect_gui_tool_bar_items(&eval);
    assert!(
        items.iter().any(|item| item.icon_name == "search"),
        "tool-bar items: {items:#?}"
    );
    assert!(items.iter().any(|item| item.is_separator));
}
