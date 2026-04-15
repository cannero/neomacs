//! GUI menu-bar and tool-bar item collection.
//!
//! Mirrors the existing TTY menu-bar walk, but produces GUI overlay
//! payloads for the render thread from the active Lisp keymaps.

use neomacs_display_protocol::{MenuBarItem, ToolBarItem};
use neovm_core::emacs_core::intern::intern;
use neovm_core::emacs_core::keymap::list_keymap_for_each_binding;
use neovm_core::emacs_core::{Context, Value};

use crate::tty_menu_bar::collect_tty_menu_bar_items;

pub fn collect_gui_menu_bar_items(eval: &Context) -> Vec<MenuBarItem> {
    collect_tty_menu_bar_items(eval)
        .into_iter()
        .enumerate()
        .map(|(index, item)| MenuBarItem {
            index: index as u32,
            label: item.label,
            key: item.key,
        })
        .collect()
}

pub fn collect_gui_tool_bar_items(eval: &Context) -> Vec<ToolBarItem> {
    let raw_map = current_tool_bar_map(eval);
    let Some(keymap) = resolve_keymap(eval, &raw_map) else {
        return Vec::new();
    };

    let mut items = Vec::new();
    list_keymap_for_each_binding(&keymap, |key, def| {
        let key_name = key_symbol_name(&key);
        let def = normalize_binding_def(&def);
        let Some(item) = parse_tool_bar_item(&key_name, &def, items.len() as u32) else {
            return;
        };
        items.push(item);
    });
    items
}

fn normalize_binding_def(def: &Value) -> Value {
    if def.is_cons() && def.cons_cdr().is_nil() {
        return def.cons_car();
    }
    *def
}

fn current_tool_bar_map(eval: &Context) -> Value {
    if let Some(buffer) = eval.buffer_manager().current_buffer()
        && let Some(local) = buffer.buffer_local_value("tool-bar-map")
    {
        return local;
    }
    eval.obarray()
        .default_value_id(intern("tool-bar-map"))
        .copied()
        .unwrap_or(Value::NIL)
}

fn parse_tool_bar_item(key_name: &str, def: &Value, index: u32) -> Option<ToolBarItem> {
    if key_name.starts_with("separator") || def.as_symbol_name() == Some("menu-bar-separator") {
        return Some(ToolBarItem {
            index,
            icon_name: String::new(),
            label: String::new(),
            help: String::new(),
            enabled: false,
            selected: false,
            is_separator: true,
        });
    }

    let (mut label, plist) = extract_menu_item_label_and_plist(def)?;
    if label.is_empty() {
        label = plist_lookup(&plist, ":label")
            .and_then(|value| value.as_runtime_string_owned())
            .unwrap_or_default();
    }
    let icon_name = plist_lookup(&plist, ":image")
        .and_then(|image| first_image_file_stem(&image))
        .unwrap_or_default();
    let help = plist_lookup(&plist, ":help")
        .and_then(|value| value.as_runtime_string_owned())
        .unwrap_or_default();
    let enabled = plist_lookup(&plist, ":enable")
        .map(|value| !value.is_nil())
        .unwrap_or(true);

    Some(ToolBarItem {
        index,
        icon_name,
        label,
        help,
        enabled,
        selected: false,
        is_separator: false,
    })
}

fn extract_menu_item_label_and_plist(def: &Value) -> Option<(String, Value)> {
    if !def.is_cons() {
        return None;
    }
    let car = def.cons_car();
    let cdr = def.cons_cdr();

    if car.as_symbol_name() == Some("menu-item") && cdr.is_cons() {
        let label = cdr.cons_car().as_runtime_string_owned().unwrap_or_default();
        let mut rest = cdr.cons_cdr();
        if !rest.is_cons() {
            return None;
        }
        rest = rest.cons_cdr();
        return Some((label, rest));
    }

    let label = car.as_runtime_string_owned()?;
    Some((label, cdr))
}

fn plist_lookup(plist: &Value, wanted: &str) -> Option<Value> {
    let mut tail = *plist;
    while tail.is_cons() {
        let key = tail.cons_car();
        tail = tail.cons_cdr();
        if !tail.is_cons() {
            break;
        }
        let value = tail.cons_car();
        if key.as_symbol_name() == Some(wanted) {
            return Some(value);
        }
        tail = tail.cons_cdr();
    }
    None
}

fn first_image_file_stem(value: &Value) -> Option<String> {
    if let Some(path) = value.as_runtime_string_owned()
        && let Some(stem) = icon_stem_from_path(&path)
    {
        return Some(stem);
    }
    if value.is_cons() {
        if let Some(stem) = first_image_file_stem(&value.cons_car()) {
            return Some(stem);
        }
        return first_image_file_stem(&value.cons_cdr());
    }
    None
}

fn icon_stem_from_path(path: &str) -> Option<String> {
    let filename = path.rsplit('/').next()?;
    let (stem, _) = filename.rsplit_once('.').unwrap_or((filename, ""));
    if stem.is_empty() {
        None
    } else {
        Some(stem.to_string())
    }
}

fn resolve_keymap(eval: &Context, value: &Value) -> Option<Value> {
    if is_keymap(value) {
        return Some(*value);
    }
    if let Some(name) = value.as_symbol_name()
        && let Some(symbol_value) = eval.obarray().symbol_value(name)
        && is_keymap(symbol_value)
    {
        return Some(*symbol_value);
    }
    None
}

fn is_keymap(value: &Value) -> bool {
    value.is_cons() && value.cons_car().as_symbol_name() == Some("keymap")
}

fn key_symbol_name(key: &Value) -> String {
    key.as_symbol_name()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("{key:?}"))
}

#[cfg(test)]
mod tests {
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
}
