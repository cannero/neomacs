use super::*;
use crate::emacs_core::intern::{intern, resolve_sym};

// -- Key description parsing tests --

#[test]
fn parse_plain_char() {
    crate::test_utils::init_test_tracing();
    let keys = parse_key_description("a").unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(
        keys[0],
        KeyEvent::Char {
            code: 'a',
            ctrl: false,
            meta: false,
            shift: false,
            super_: false,
            hyper: false,
            alt: false,
        }
    );
}

#[test]
fn parse_ctrl_x() {
    crate::test_utils::init_test_tracing();
    let keys = parse_key_description("C-x").unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(
        keys[0],
        KeyEvent::Char {
            code: 'x',
            ctrl: true,
            meta: false,
            shift: false,
            super_: false,
            hyper: false,
            alt: false,
        }
    );
}

#[test]
fn parse_meta_x() {
    crate::test_utils::init_test_tracing();
    let keys = parse_key_description("M-x").unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(
        keys[0],
        KeyEvent::Char {
            code: 'x',
            ctrl: false,
            meta: true,
            shift: false,
            super_: false,
            hyper: false,
            alt: false,
        }
    );
}

#[test]
fn parse_ctrl_x_ctrl_f_sequence() {
    crate::test_utils::init_test_tracing();
    let keys = parse_key_description("C-x C-f").unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(
        keys[0],
        KeyEvent::Char {
            code: 'x',
            ctrl: true,
            meta: false,
            shift: false,
            super_: false,
            hyper: false,
            alt: false,
        }
    );
    assert_eq!(
        keys[1],
        KeyEvent::Char {
            code: 'f',
            ctrl: true,
            meta: false,
            shift: false,
            super_: false,
            hyper: false,
            alt: false,
        }
    );
}

#[test]
fn parse_ret() {
    crate::test_utils::init_test_tracing();
    let keys = parse_key_description("RET").unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(
        keys[0],
        KeyEvent::Function {
            name: intern("return"),
            ctrl: false,
            meta: false,
            shift: false,
            super_: false,
            hyper: false,
            alt: false,
        }
    );
}

#[test]
fn parse_tab() {
    crate::test_utils::init_test_tracing();
    let keys = parse_key_description("TAB").unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(
        keys[0],
        KeyEvent::Function {
            name: intern("tab"),
            ctrl: false,
            meta: false,
            shift: false,
            super_: false,
            hyper: false,
            alt: false,
        }
    );
}

#[test]
fn parse_esc_as_literal_escape_char() {
    crate::test_utils::init_test_tracing();
    let keys = parse_key_description("ESC").unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(
        keys[0],
        KeyEvent::Char {
            code: '\u{1b}',
            ctrl: false,
            meta: false,
            shift: false,
            super_: false,
            hyper: false,
            alt: false,
        }
    );
}

#[test]
fn parse_spc() {
    crate::test_utils::init_test_tracing();
    let keys = parse_key_description("SPC").unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(
        keys[0],
        KeyEvent::Char {
            code: ' ',
            ctrl: false,
            meta: false,
            shift: false,
            super_: false,
            hyper: false,
            alt: false,
        }
    );
}

#[test]
fn parse_combined_modifiers() {
    crate::test_utils::init_test_tracing();
    let keys = parse_key_description("C-M-s").unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(
        keys[0],
        KeyEvent::Char {
            code: 's',
            ctrl: true,
            meta: true,
            shift: false,
            super_: false,
            hyper: false,
            alt: false,
        }
    );
}

#[test]
fn parse_function_key() {
    crate::test_utils::init_test_tracing();
    let keys = parse_key_description("f1").unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(
        keys[0],
        KeyEvent::Function {
            name: intern("f1"),
            ctrl: false,
            meta: false,
            shift: false,
            super_: false,
            hyper: false,
            alt: false,
        }
    );
}

#[test]
fn parse_ctrl_function_key() {
    crate::test_utils::init_test_tracing();
    let keys = parse_key_description("C-f12").unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(
        keys[0],
        KeyEvent::Function {
            name: intern("f12"),
            ctrl: true,
            meta: false,
            shift: false,
            super_: false,
            hyper: false,
            alt: false,
        }
    );
}

#[test]
fn parse_error_empty() {
    crate::test_utils::init_test_tracing();
    assert!(parse_key_description("").is_err());
}

#[test]
fn parse_error_unknown_name() {
    crate::test_utils::init_test_tracing();
    assert!(parse_key_description("foobar").is_err());
}

#[test]
fn format_key_event_roundtrip() {
    crate::test_utils::init_test_tracing();
    let cases = vec![
        "C-x", "M-x", "C-M-s", "a", "SPC", "RET", "TAB", "ESC", "f1", "C-f12",
    ];
    for desc in cases {
        let keys = parse_key_description(desc).unwrap();
        assert_eq!(keys.len(), 1, "expected single key for {}", desc);
        let formatted = format_key_event(&keys[0]);
        let reparsed = parse_key_description(&formatted).unwrap();
        assert_eq!(
            keys[0], reparsed[0],
            "roundtrip mismatch for {}: formatted as {}, reparsed as {:?}",
            desc, formatted, reparsed[0]
        );
    }
}

#[test]
fn keyboard_escape_encodes_to_emacs_escape_prefix_char() {
    crate::test_utils::init_test_tracing();
    let event = KeyEvent::from(crate::keyboard::KeyEvent::named(
        crate::keyboard::NamedKey::Escape,
    ));
    assert_eq!(
        key_event_to_emacs_event(&event),
        Value::fixnum(27),
        "physical Escape should enter GNU ESC-prefix through event 27"
    );
}

#[test]
fn keyboard_escape_preserves_non_ctrl_modifiers_when_encoded() {
    crate::test_utils::init_test_tracing();
    let event = KeyEvent::from(crate::keyboard::KeyEvent::named_with_mods(
        crate::keyboard::NamedKey::Escape,
        crate::keyboard::Modifiers {
            shift: true,
            hyper: true,
            ..crate::keyboard::Modifiers::none()
        },
    ));
    assert_eq!(
        key_event_to_emacs_event(&event),
        Value::fixnum(
            27 | crate::emacs_core::keyboard::pure::KEY_CHAR_SHIFT
                | crate::emacs_core::keyboard::pure::KEY_CHAR_HYPER
        )
    );
}

#[test]
fn keyboard_return_encodes_to_emacs_carriage_return() {
    crate::test_utils::init_test_tracing();
    let event = KeyEvent::from(crate::keyboard::KeyEvent::named(
        crate::keyboard::NamedKey::Return,
    ));
    assert_eq!(
        key_event_to_emacs_event(&event),
        Value::fixnum('\r' as i64),
        "physical Return should enter GNU RET/C-m through event 13"
    );
}

#[test]
fn keyboard_meta_return_encodes_to_emacs_meta_carriage_return() {
    crate::test_utils::init_test_tracing();
    let event = KeyEvent::from(crate::keyboard::KeyEvent::named_with_mods(
        crate::keyboard::NamedKey::Return,
        crate::keyboard::Modifiers::meta(),
    ));
    assert_eq!(
        key_event_to_emacs_event(&event),
        Value::fixnum(0x08000000 | '\r' as i64),
        "Meta+Return should be encoded as meta-bit plus RET/C-m"
    );
}

#[test]
fn keyboard_tab_encodes_to_emacs_tab_char() {
    crate::test_utils::init_test_tracing();
    let event = KeyEvent::from(crate::keyboard::KeyEvent::named(
        crate::keyboard::NamedKey::Tab,
    ));
    assert_eq!(
        key_event_to_emacs_event(&event),
        Value::fixnum('\t' as i64),
        "physical Tab should enter GNU TAB through event 9"
    );
}

#[test]
fn format_key_event_renders_gnu_control_char_names() {
    crate::test_utils::init_test_tracing();
    assert_eq!(
        format_key_event(&KeyEvent::Char {
            code: '\r',
            ctrl: false,
            meta: true,
            shift: false,
            super_: false,
            hyper: false,
            alt: false,
        }),
        "M-RET"
    );
    assert_eq!(
        format_key_event(&KeyEvent::Char {
            code: '\t',
            ctrl: false,
            meta: false,
            shift: false,
            super_: false,
            hyper: false,
            alt: false,
        }),
        "TAB"
    );
    assert_eq!(
        format_key_event(&KeyEvent::Char {
            code: '\u{7f}',
            ctrl: false,
            meta: false,
            shift: false,
            super_: false,
            hyper: false,
            alt: false,
        }),
        "DEL"
    );
}

#[test]
fn format_key_sequence_roundtrip() {
    crate::test_utils::init_test_tracing();
    let desc = "C-x C-f";
    let keys = parse_key_description(desc).unwrap();
    let formatted = format_key_sequence(&keys);
    assert_eq!(formatted, "C-x C-f");
}

#[test]
fn parse_arrow_keys() {
    crate::test_utils::init_test_tracing();
    for name in &["up", "down", "left", "right"] {
        let keys = parse_key_description(name).unwrap();
        assert_eq!(keys.len(), 1);
        match &keys[0] {
            KeyEvent::Function { name: n, .. } => assert_eq!(resolve_sym(*n), *name),
            other => panic!("expected Function for {}, got {:?}", name, other),
        }
    }
}

#[test]
fn parse_modifier_with_named_key() {
    crate::test_utils::init_test_tracing();
    let keys = parse_key_description("C-RET").unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(
        keys[0],
        KeyEvent::Function {
            name: intern("return"),
            ctrl: true,
            meta: false,
            shift: false,
            super_: false,
            hyper: false,
            alt: false,
        }
    );
}

// -- List keymap tests --

#[test]
fn list_keymap_create_and_check() {
    crate::test_utils::init_test_tracing();
    let km = make_list_keymap();
    assert!(is_list_keymap(&km));
    let sparse = make_sparse_list_keymap();
    assert!(is_list_keymap(&sparse));
    assert!(!is_list_keymap(&Value::NIL));
    assert!(!is_list_keymap(&Value::fixnum(42)));
}

#[test]
fn list_keymap_define_and_lookup() {
    crate::test_utils::init_test_tracing();
    let km = make_sparse_list_keymap();
    let event = Value::symbol("return");
    list_keymap_define(km, event, Value::symbol("newline"));
    let result = list_keymap_lookup_one(&km, &event);
    assert_eq!(result.as_symbol_name(), Some("newline"));
}

#[test]
fn list_keymap_parent_chain() {
    crate::test_utils::init_test_tracing();
    let parent = make_sparse_list_keymap();
    let child = make_sparse_list_keymap();
    list_keymap_set_parent(child, parent);
    assert!(is_list_keymap(&list_keymap_parent(&child)));

    // Binding in parent is found via child
    let event = Value::fixnum(97); // 'a'
    list_keymap_define(parent, event, Value::symbol("cmd-a"));
    let result = list_keymap_lookup_one(&child, &event);
    assert_eq!(result.as_symbol_name(), Some("cmd-a"));
}

#[test]
fn list_keymap_child_overrides_parent() {
    crate::test_utils::init_test_tracing();
    let parent = make_sparse_list_keymap();
    let child = make_sparse_list_keymap();
    list_keymap_set_parent(child, parent);

    let event = Value::fixnum(120); // 'x'
    list_keymap_define(parent, event, Value::symbol("parent-cmd"));
    list_keymap_define(child, event, Value::symbol("child-cmd"));
    let result = list_keymap_lookup_one(&child, &event);
    assert_eq!(result.as_symbol_name(), Some("child-cmd"));
}

#[test]
fn list_keymap_set_parent_replaces_direct_sparse_parent_without_mutating_old_parent() {
    crate::test_utils::init_test_tracing();
    let parent_one = make_sparse_list_keymap();
    let parent_two = make_sparse_list_keymap();
    let child = make_sparse_list_keymap();

    list_keymap_define(
        parent_one,
        Value::fixnum('a' as i64),
        Value::symbol("parent-one"),
    );
    list_keymap_define(
        parent_two,
        Value::fixnum('b' as i64),
        Value::symbol("parent-two"),
    );

    list_keymap_set_parent(child, parent_one);
    assert!(keymap_value_eq(&list_keymap_parent(&child), &parent_one));

    list_keymap_set_parent(child, parent_two);
    assert!(keymap_value_eq(&list_keymap_parent(&child), &parent_two));
    assert!(list_keymap_parent(&parent_one).is_nil());
    assert_eq!(
        list_keymap_lookup_one(&parent_one, &Value::fixnum('a' as i64)).as_symbol_name(),
        Some("parent-one")
    );
    assert!(list_keymap_lookup_one(&child, &Value::fixnum('a' as i64)).is_nil());
    assert_eq!(
        list_keymap_lookup_one(&child, &Value::fixnum('b' as i64)).as_symbol_name(),
        Some("parent-two")
    );
}

#[test]
fn list_keymap_for_each_binding_stops_before_direct_sparse_parent() {
    crate::test_utils::init_test_tracing();
    let parent = make_sparse_list_keymap();
    let child = make_sparse_list_keymap();

    list_keymap_define(
        parent,
        Value::fixnum('a' as i64),
        Value::symbol("parent-cmd"),
    );
    list_keymap_define(child, Value::fixnum('x' as i64), Value::symbol("child-cmd"));
    list_keymap_set_parent(child, parent);

    let mut seen = Vec::new();
    list_keymap_for_each_binding(&child, |event, def| seen.push((event, def)));

    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, Value::fixnum('x' as i64));
    assert_eq!(seen[0].1.as_symbol_name(), Some("child-cmd"));
}

#[test]
fn list_keymap_accessible_does_not_descend_into_direct_sparse_parent() {
    crate::test_utils::init_test_tracing();
    let parent = make_sparse_list_keymap();
    let prefix_map = make_sparse_list_keymap();
    let child = make_sparse_list_keymap();

    list_keymap_define(parent, Value::fixnum('a' as i64), prefix_map);
    list_keymap_set_parent(child, parent);

    let mut prefix = Vec::new();
    let mut out = Vec::new();
    let mut seen = Vec::new();
    list_keymap_accessible(&child, &mut prefix, &mut out, &mut seen);

    assert_eq!(out.len(), 1);
}

#[test]
fn list_keymap_copy_preserves_direct_sparse_parent_without_inlining_parent_bindings() {
    crate::test_utils::init_test_tracing();
    let parent = make_sparse_list_keymap();
    let child = make_sparse_list_keymap();

    list_keymap_define(
        parent,
        Value::fixnum('a' as i64),
        Value::symbol("parent-cmd"),
    );
    list_keymap_define(child, Value::fixnum('x' as i64), Value::symbol("child-cmd"));
    list_keymap_set_parent(child, parent);

    let copy = list_keymap_copy(&child);

    assert!(keymap_value_eq(&list_keymap_parent(&copy), &parent));
    assert_eq!(
        list_keymap_lookup_one(&copy, &Value::fixnum('x' as i64)).as_symbol_name(),
        Some("child-cmd")
    );
    assert_eq!(
        list_keymap_lookup_one(&copy, &Value::fixnum('a' as i64)).as_symbol_name(),
        Some("parent-cmd")
    );

    let mut seen = Vec::new();
    list_keymap_for_each_binding(&copy, |event, def| seen.push((event, def)));
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, Value::fixnum('x' as i64));
}

#[test]
fn store_in_keymap_preserves_string_prompt_when_prepending_binding() {
    // Regression: doom-dashboard / evil-collection lose their
    // `[normal-state]` aux keymap prompt because the first define-key
    // on a keymap with a string prompt used to insert the new binding
    // BEFORE the prompt, hiding it from `keymap-prompt`. After this
    // fix, prompts survive define-key.
    crate::test_utils::init_test_tracing();
    let prompt = Value::string("Auxiliary keymap for Normal state");
    let map = Value::list(vec![Value::symbol("keymap"), prompt]);

    list_keymap_define(map, Value::fixnum('x' as i64), Value::symbol("foo"));

    // Prompt must still be the cadr of the keymap.
    let cdr = map.cons_cdr();
    assert!(cdr.is_cons(), "expected non-empty cdr after define-key");
    let head = cdr.cons_car();
    assert!(
        head.is_string(),
        "expected first cdr element to remain the prompt string after \
         define-key, got {head:?}"
    );
    assert_eq!(
        head.as_utf8_str(),
        Some("Auxiliary keymap for Normal state"),
        "prompt string was clobbered or replaced"
    );

    // The new binding must still exist and be reachable.
    let bound = list_keymap_lookup_one(&map, &Value::fixnum('x' as i64));
    assert_eq!(bound.as_symbol_name(), Some("foo"));
}

#[test]
fn list_keymap_event_conversion_roundtrip() {
    crate::test_utils::init_test_tracing();
    let key = KeyEvent::Char {
        code: 'x',
        ctrl: true,
        meta: false,
        shift: false,
        super_: false,
        hyper: false,
        alt: false,
    };
    let emacs_event = key_event_to_emacs_event(&key);
    let roundtrip = emacs_event_to_key_event(&emacs_event).unwrap();
    assert_eq!(key, roundtrip);
}
