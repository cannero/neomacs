//! TTY menu bar item collection.
//!
//! Mirrors GNU Emacs `keyboard.c:menu_bar_items` (around line 8605):
//! walks the active keymaps' `[menu-bar]` prefix, collects each
//! top-level item label, and reorders so items in `menu-bar-final-items`
//! (typically `(help-menu)`) move to the end of the list. The result is
//! handed to `display_menu_bar` (`xdisp.c:27444`) for rasterization.
//!
//! Neomacs implementation differences from GNU:
//!
//! * GNU stores items in the frame's `menu_bar_items_vector` with four
//!   slots per item (key, string, def, hpos).  We carry only the
//!   user-visible fields (label + key) in a `Vec<TtyMenuBarItem>` since
//!   the TTY rasterizer doesn't need `def`, and `hpos` is computed on
//!   the way out by the rasterizer.
//! * Menu items whose label resolves to nil or whose definition is nil
//!   are skipped, mirroring `menu_bar_item`'s `STRINGP (string)` /
//!   `CONSP (def)` guards.
//! * `current-minor-mode-map-alist` is not walked yet — minor-mode
//!   menu-bar additions are TODO. Major-mode (`current-local-map`) and
//!   global maps are walked, which matches GNU's most common menus
//!   like `Lisp-Interaction` in `*scratch*`.

use neomacs_display_protocol::glyph_matrix::TtyMenuBarItem;
use neovm_core::emacs_core::Context;
use neovm_core::emacs_core::Value;
use neovm_core::emacs_core::keymap::{list_keymap_for_each_binding, list_keymap_lookup_one};

/// Walk the active `[menu-bar]` keymap(s) and return the items to draw.
///
/// Returns an empty vec if there is no menu bar (e.g. `global-map` has
/// no `[menu-bar]` binding) or if the binding doesn't resolve to a
/// keymap.  The returned items are in display order (left-to-right),
/// with `menu-bar-final-items` (default: `help-menu`) moved to the end
/// like GNU `keyboard.c:8697-8716`.
///
/// Walks `current-global-map` first, then the current buffer's
/// `current-local-map` (the major-mode map). This is enough to
/// reproduce GNU's behaviour for the common case where the major mode
/// adds menu items (e.g. `Lisp-Interaction`); minor-mode menu-bar
/// additions are TODO.
pub fn collect_tty_menu_bar_items(eval: &Context) -> Vec<TtyMenuBarItem> {
    let mut items: Vec<TtyMenuBarItem> = Vec::new();

    // 1. global-map: foundation set of items.
    if let Some(global_map) = eval.obarray().symbol_value("global-map").copied() {
        collect_from_keymap(eval, &global_map, &mut items);
    }

    // 2. current-local-map: the major-mode map for the selected
    //    window's buffer. GNU walks this AFTER the global map (well,
    //    technically before in the maps[] array but it iterates
    //    backwards), and items found here append to the existing
    //    vector — duplicates are filtered by `key` so a local entry
    //    with the same key as a global entry doesn't duplicate the
    //    label, matching GNU `menu_bar_item`'s dedup-by-key behaviour.
    let local_map = eval.buffer_manager().current_local_map();
    if !local_map.is_nil() {
        collect_from_keymap(eval, &local_map, &mut items);
    }

    move_final_items_to_end(eval, &mut items);
    items
}

/// Walk a single keymap looking for its `[menu-bar]` sub-keymap and
/// append any new items into `items` (deduping by key).
fn collect_from_keymap(eval: &Context, keymap: &Value, items: &mut Vec<TtyMenuBarItem>) {
    let menu_bar_sym = Value::symbol("menu-bar");
    let raw_binding = list_keymap_lookup_one(keymap, &menu_bar_sym);
    if raw_binding.is_nil() {
        return;
    }

    let menu_bar_keymap = match resolve_keymap(eval, &raw_binding) {
        Some(km) => km,
        None => return,
    };

    list_keymap_for_each_binding(&menu_bar_keymap, |key, def| {
        let key_str = key_symbol_name(&key);
        if let Some(label) = extract_menu_label(&def) {
            // Dedup-by-key: GNU's `menu_bar_item` calls `Fmemq (key,
            // menu_bar_one_keymap_changed_items)` to skip a key it has
            // already seen for the *current* keymap. Here we apply the
            // same idea across the union of keymaps so that a major
            // mode that re-binds an existing top-level menu (rare)
            // doesn't produce a duplicate label. The first occurrence
            // wins, mirroring the natural reverse-insertion-order walk
            // (newest binding first within each map).
            if !items.iter().any(|item| item.key == key_str) {
                items.push(TtyMenuBarItem {
                    label,
                    key: key_str,
                    hpos: 0,
                });
            }
        }
    });
}

/// Resolve a keymap reference: either a `(keymap ...)` cons or a symbol
/// whose value is such a cons.
fn resolve_keymap(eval: &Context, value: &Value) -> Option<Value> {
    if is_keymap(value) {
        return Some(*value);
    }
    if let Some(name) = value.as_symbol_name() {
        if let Some(symbol_value) = eval.obarray().symbol_value(name) {
            if is_keymap(symbol_value) {
                return Some(*symbol_value);
            }
        }
    }
    None
}

/// Reorder `items` so that any whose key matches an entry in
/// `menu-bar-final-items` is moved to the end of the list, preserving
/// relative order. Mirrors `keyboard.c:8697-8716`.
fn move_final_items_to_end(eval: &Context, items: &mut Vec<TtyMenuBarItem>) {
    let final_items = match eval.obarray().symbol_value("menu-bar-final-items") {
        Some(value) => *value,
        None => return,
    };
    if final_items.is_nil() {
        return;
    }

    // Collect the symbol names referenced by `menu-bar-final-items`.
    let mut tail = final_items;
    let mut final_keys: Vec<String> = Vec::new();
    while tail.is_cons() {
        let head = tail.cons_car();
        if let Some(name) = head.as_symbol_name() {
            final_keys.push(name.to_string());
        }
        tail = tail.cons_cdr();
    }
    if final_keys.is_empty() {
        return;
    }

    // Stable partition: keep non-final items first, then final items.
    let mut non_final: Vec<TtyMenuBarItem> = Vec::with_capacity(items.len());
    let mut moved: Vec<TtyMenuBarItem> = Vec::new();
    for item in items.drain(..) {
        if final_keys.iter().any(|k| k == &item.key) {
            moved.push(item);
        } else {
            non_final.push(item);
        }
    }
    *items = non_final;
    items.extend(moved);
}

/// Extract the user-visible label from a menu-bar binding.
///
/// Handles the two shapes GNU's menu_bar_item recognises:
///
/// * `(STRING . CMD-OR-SUBMAP)` — simple binding from
///   `(define-key map [menu-bar foo] (cons "Foo" cmd))`. Label is `STRING`.
/// * `(menu-item LABEL CMD …)` — extended menu-item form. Label is `LABEL`,
///   which can be a string or a Lisp form that evaluates to a string. We
///   only honour string labels for the MVP; deferred Lisp evaluation of
///   dynamic labels is a TODO matching GNU's `Feval (label, ...)` path.
fn extract_menu_label(def: &Value) -> Option<String> {
    if !def.is_cons() {
        return None;
    }
    let car = def.cons_car();
    let cdr = def.cons_cdr();

    // (menu-item LABEL ...)
    if car.as_symbol_name() == Some("menu-item") && cdr.is_cons() {
        let label = cdr.cons_car();
        if let Some(s) = label.as_str_owned() {
            return Some(s);
        }
        return None;
    }

    // (STRING . CMD-OR-SUBMAP)
    if let Some(s) = car.as_str_owned() {
        return Some(s);
    }

    None
}

/// True if `value` looks like a keymap (`(keymap ...)`).
fn is_keymap(value: &Value) -> bool {
    if !value.is_cons() {
        return false;
    }
    value.cons_car().as_symbol_name() == Some("keymap")
}

/// Render a menu-bar key value as a printable identifier.
fn key_symbol_name(key: &Value) -> String {
    if let Some(name) = key.as_symbol_name() {
        return name.to_string();
    }
    format!("{:?}", key)
}
