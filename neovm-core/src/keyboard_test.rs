use super::*;

fn reset_keyboard_test_terminals() {
    crate::emacs_core::terminal::pure::reset_terminal_thread_locals();
}

fn ensure_keyboard_test_terminal(id: u64) {
    crate::emacs_core::terminal::pure::ensure_terminal_runtime_owner(
        id,
        format!("tty-{id}"),
        crate::emacs_core::terminal::pure::TerminalRuntimeConfig::interactive(
            Some("xterm-256color".to_string()),
            256,
        ),
    );
}

#[test]
fn key_event_description() {
    crate::test_utils::init_test_tracing();
    let e = KeyEvent::char('x');
    assert_eq!(e.to_description(), "x");

    let e = KeyEvent::char_with_mods('x', Modifiers::ctrl());
    assert_eq!(e.to_description(), "C-x");

    let e = KeyEvent::char_with_mods('f', Modifiers::meta());
    assert_eq!(e.to_description(), "M-f");

    let e = KeyEvent::char_with_mods('g', Modifiers::ctrl_meta());
    assert_eq!(e.to_description(), "C-M-g");

    let e = KeyEvent::named(NamedKey::Return);
    assert_eq!(e.to_description(), "RET");
}

#[test]
fn key_event_parse() {
    crate::test_utils::init_test_tracing();
    let e = KeyEvent::from_description("C-x").unwrap();
    assert_eq!(e.key, Key::Char('x'));
    assert!(e.modifiers.ctrl);
    assert!(!e.modifiers.meta);

    let e = KeyEvent::from_description("M-f").unwrap();
    assert_eq!(e.key, Key::Char('f'));
    assert!(e.modifiers.meta);

    let e = KeyEvent::from_description("RET").unwrap();
    assert_eq!(e.key, Key::Named(NamedKey::Return));

    let e = KeyEvent::from_description("C-M-g").unwrap();
    assert!(e.modifiers.ctrl);
    assert!(e.modifiers.meta);
}

#[test]
fn key_sequence_description() {
    crate::test_utils::init_test_tracing();
    let seq = KeySequence::from_description("C-x C-f").unwrap();
    assert_eq!(seq.len(), 2);
    assert_eq!(seq.to_description(), "C-x C-f");
}

#[test]
fn prefix_arg_values() {
    crate::test_utils::init_test_tracing();
    assert_eq!(PrefixArg::None.numeric_value(), 1);
    assert_eq!(PrefixArg::Numeric(5).numeric_value(), 5);
    assert_eq!(PrefixArg::Raw(1).numeric_value(), 4);
    assert_eq!(PrefixArg::Raw(2).numeric_value(), 16);
}

#[test]
fn command_loop_enqueue_read() {
    crate::test_utils::init_test_tracing();
    let mut cl = CommandLoop::new();
    cl.enqueue_event(InputEvent::key_press(KeyEvent::char('a')));
    cl.enqueue_event(InputEvent::key_press(KeyEvent::char('b')));

    let e = cl.read_key_event().unwrap();
    assert_eq!(e, Value::fixnum('a' as i64));
    let e = cl.read_key_event().unwrap();
    assert_eq!(e, Value::fixnum('b' as i64));
    assert!(cl.read_key_event().is_none());
}

#[test]
fn unread_events_have_priority() {
    crate::test_utils::init_test_tracing();
    let mut cl = CommandLoop::new();
    cl.enqueue_event(InputEvent::key_press(KeyEvent::char('a')));
    cl.unread_key(KeyEvent::char('z'));

    let e = cl.read_key_event().unwrap();
    assert_eq!(e, Value::fixnum('z' as i64)); // unread first
    let e = cl.read_key_event().unwrap();
    assert_eq!(e, Value::fixnum('a' as i64)); // then queue
}

#[test]
fn keyboard_runtime_preserves_kboard_state_per_terminal() {
    crate::test_utils::init_test_tracing();
    reset_keyboard_test_terminals();
    ensure_keyboard_test_terminal(7);
    let mut runtime = KeyboardRuntime::new();
    runtime.set_input_decode_map(Value::symbol("primary-map"));
    runtime.unread_event(Value::fixnum(1));

    runtime.select_terminal(7);
    assert_eq!(runtime.active_terminal_id(), 7);
    assert_eq!(runtime.input_decode_map(), Value::NIL);
    assert!(runtime.kboard.unread_events.is_empty());

    runtime.set_input_decode_map(Value::symbol("secondary-map"));
    runtime.unread_event(Value::fixnum(2));

    runtime.select_terminal(crate::emacs_core::terminal::pure::TERMINAL_ID);
    assert_eq!(
        runtime.input_decode_map(),
        Value::symbol("primary-map"),
        "switching back should restore the original terminal kboard state"
    );
    assert_eq!(
        runtime.kboard.unread_events.pop_front(),
        Some(Value::fixnum(1)),
        "unread events should be terminal-local"
    );

    runtime.select_terminal(7);
    assert_eq!(runtime.input_decode_map(), Value::symbol("secondary-map"));
    assert_eq!(
        runtime.kboard.unread_events.pop_front(),
        Some(Value::fixnum(2))
    );
}

#[test]
fn keyboard_runtime_polls_parked_kboards_after_active_one() {
    crate::test_utils::init_test_tracing();
    reset_keyboard_test_terminals();
    ensure_keyboard_test_terminal(7);
    ensure_keyboard_test_terminal(9);
    let mut runtime = KeyboardRuntime::new();
    runtime.unread_event(Value::fixnum(1));
    runtime.select_terminal(7);
    runtime.unread_event(Value::fixnum(2));
    runtime.select_terminal(9);
    runtime.unread_event(Value::fixnum(3));
    runtime.select_terminal(crate::emacs_core::terminal::pure::TERMINAL_ID);

    assert_eq!(runtime.read_key_event(), Some(Value::fixnum(1)));
    assert_eq!(
        runtime.read_key_event(),
        Some(Value::fixnum(3)),
        "after the active kboard drains, parked terminal input should be read in GNU terminal-list order"
    );
    assert_eq!(
        runtime.read_key_event(),
        Some(Value::fixnum(2)),
        "older parked terminal input should be read after newer terminals"
    );
    assert_eq!(runtime.active_terminal_id(), 7);
}

#[test]
fn keyboard_runtime_reports_pending_input_across_parked_kboards() {
    crate::test_utils::init_test_tracing();
    reset_keyboard_test_terminals();
    ensure_keyboard_test_terminal(9);
    let mut runtime = KeyboardRuntime::new();
    runtime.select_terminal(9);
    runtime.unread_event(Value::fixnum(99));
    runtime.select_terminal(crate::emacs_core::terminal::pure::TERMINAL_ID);

    assert!(
        runtime.has_pending_kboard_input(),
        "parked terminal unread input should still count as pending"
    );
}

#[test]
fn keyboard_macro_recording() {
    crate::test_utils::init_test_tracing();
    let mut cl = CommandLoop::new();
    cl.start_kbd_macro();

    cl.enqueue_event(InputEvent::key_press(KeyEvent::char('h')));
    cl.enqueue_event(InputEvent::key_press(KeyEvent::char('i')));

    cl.read_key_event(); // 'h' — recorded
    cl.read_key_event(); // 'i' — recorded

    cl.finalize_kbd_macro_chars();
    let recorded = cl.end_kbd_macro();
    assert_eq!(recorded.len(), 2);
    assert_eq!(cl.keyboard.kboard.kbd_macro_end, 2);

    // Replay.
    cl.begin_executing_kbd_macro(recorded);
    let e1 = cl.read_key_event().unwrap();
    assert_eq!(e1, Value::fixnum('h' as i64));
    let e2 = cl.read_key_event().unwrap();
    assert_eq!(e2, Value::fixnum('i' as i64));
}

#[test]
fn quit_flag() {
    crate::test_utils::init_test_tracing();
    let mut cl = CommandLoop::new();
    assert!(!cl.check_quit());

    cl.signal_quit();
    assert!(cl.check_quit());
    assert!(!cl.check_quit()); // cleared
}

#[test]
fn interactive_spec_parsing() {
    crate::test_utils::init_test_tracing();
    let codes = parse_interactive_spec("sSearch for: \nnCount: ");
    assert_eq!(codes.len(), 2);
    assert!(
        matches!(&codes[0], InteractiveCode::StringArg(p) if p.as_utf8_str() == Some("Search for: "))
    );
    assert!(
        matches!(&codes[1], InteractiveCode::NumberArg(p) if p.as_utf8_str() == Some("Count: "))
    );
}

#[test]
fn modifier_bits_round_trip() {
    crate::test_utils::init_test_tracing();
    let m = Modifiers {
        ctrl: true,
        meta: true,
        shift: false,
        super_: false,
        hyper: false,
    };
    let bits = m.to_bits();
    let m2 = Modifiers::from_bits(bits);
    assert_eq!(m, m2);
}

#[test]
fn modifier_bits_round_trip_all_combinations() {
    crate::test_utils::init_test_tracing();
    // Test each individual modifier
    for (field, expected_bit) in [
        ("ctrl", 1u32 << 26),
        ("meta", 1u32 << 27),
        ("shift", 1u32 << 25),
        ("super", 1u32 << 23),
        ("hyper", 1u32 << 24),
    ] {
        let m = match field {
            "ctrl" => Modifiers {
                ctrl: true,
                ..Modifiers::none()
            },
            "meta" => Modifiers {
                meta: true,
                ..Modifiers::none()
            },
            "shift" => Modifiers {
                shift: true,
                ..Modifiers::none()
            },
            "super" => Modifiers {
                super_: true,
                ..Modifiers::none()
            },
            "hyper" => Modifiers {
                hyper: true,
                ..Modifiers::none()
            },
            _ => unreachable!(),
        };
        assert_eq!(m.to_bits(), expected_bit, "bit mismatch for {}", field);
        assert_eq!(
            Modifiers::from_bits(m.to_bits()),
            m,
            "round-trip failed for {}",
            field
        );
    }

    // All modifiers set
    let all = Modifiers {
        ctrl: true,
        meta: true,
        shift: true,
        super_: true,
        hyper: true,
    };
    assert_eq!(Modifiers::from_bits(all.to_bits()), all);

    // No modifiers
    assert_eq!(Modifiers::none().to_bits(), 0);
    assert_eq!(Modifiers::from_bits(0), Modifiers::none());
}

#[test]
fn prefix_string_various() {
    crate::test_utils::init_test_tracing();
    assert_eq!(Modifiers::none().prefix_string(), "");
    assert_eq!(Modifiers::ctrl().prefix_string(), "C-");
    assert_eq!(Modifiers::meta().prefix_string(), "M-");
    assert_eq!(Modifiers::ctrl_meta().prefix_string(), "C-M-");

    let all = Modifiers {
        ctrl: true,
        meta: true,
        shift: true,
        super_: true,
        hyper: true,
    };
    // Order: H- s- C- M- S-
    assert_eq!(all.prefix_string(), "H-s-C-M-S-");
}

#[test]
fn modifiers_is_empty() {
    crate::test_utils::init_test_tracing();
    assert!(Modifiers::none().is_empty());
    assert!(!Modifiers::ctrl().is_empty());
    assert!(!Modifiers::meta().is_empty());
}

#[test]
fn key_event_from_description_all_named_keys() {
    crate::test_utils::init_test_tracing();
    let cases = [
        ("RET", Key::Named(NamedKey::Return)),
        ("TAB", Key::Named(NamedKey::Tab)),
        ("ESC", Key::Named(NamedKey::Escape)),
        ("DEL", Key::Named(NamedKey::Backspace)),
        ("SPC", Key::Char(' ')),
        ("<delete>", Key::Named(NamedKey::Delete)),
        ("<insert>", Key::Named(NamedKey::Insert)),
        ("<home>", Key::Named(NamedKey::Home)),
        ("<end>", Key::Named(NamedKey::End)),
        ("<prior>", Key::Named(NamedKey::PageUp)),
        ("<next>", Key::Named(NamedKey::PageDown)),
        ("<left>", Key::Named(NamedKey::Left)),
        ("<right>", Key::Named(NamedKey::Right)),
        ("<up>", Key::Named(NamedKey::Up)),
        ("<down>", Key::Named(NamedKey::Down)),
        ("<f1>", Key::Named(NamedKey::F(1))),
        ("<f12>", Key::Named(NamedKey::F(12))),
    ];
    for (desc, expected_key) in cases {
        let e =
            KeyEvent::from_description(desc).unwrap_or_else(|| panic!("failed to parse: {}", desc));
        assert_eq!(e.key, expected_key, "mismatch for {}", desc);
        assert!(e.modifiers.is_empty(), "unexpected modifiers for {}", desc);
    }
}

#[test]
fn key_event_description_round_trip() {
    crate::test_utils::init_test_tracing();
    let descriptions = [
        "C-x", "M-f", "C-M-g", "S-<f1>", "H-s-a", "RET", "TAB", "SPC", "<left>",
    ];
    for desc in descriptions {
        let event = KeyEvent::from_description(desc).unwrap();
        let back = event.to_description();
        let reparsed = KeyEvent::from_description(&back).unwrap();
        assert_eq!(event, reparsed, "round-trip failed for {}", desc);
    }
}

#[test]
fn key_event_to_event_int() {
    crate::test_utils::init_test_tracing();
    // Plain 'a' = 97
    let e = KeyEvent::char('a');
    assert_eq!(e.to_event_int(), 97);

    // C-a = 97 | (1 << 26)
    let e = KeyEvent::char_with_mods('a', Modifiers::ctrl());
    assert_eq!(e.to_event_int(), 97 | (1 << 26));

    // RET = 13
    let e = KeyEvent::named(NamedKey::Return);
    assert_eq!(e.to_event_int(), 13);
}

#[test]
fn prefix_arg_to_value() {
    crate::test_utils::init_test_tracing();
    assert_eq!(PrefixArg::None.to_value(), Value::NIL);
    assert_eq!(PrefixArg::Numeric(3).to_value(), Value::fixnum(3));
    // Raw(1) = C-u once = (4)
    let raw1 = PrefixArg::Raw(1).to_value();
    assert!(raw1.is_cons());
}

#[test]
fn key_sequence_from_description_multi() {
    crate::test_utils::init_test_tracing();
    let seq = KeySequence::from_description("C-x C-s").unwrap();
    assert_eq!(seq.len(), 2);
    assert_eq!(seq.events[0], KeyEvent::from_description("C-x").unwrap());
    assert_eq!(seq.events[1], KeyEvent::from_description("C-s").unwrap());
}

#[test]
fn key_sequence_empty() {
    crate::test_utils::init_test_tracing();
    let seq = KeySequence::new();
    assert!(seq.is_empty());
    assert_eq!(seq.to_description(), "");
}

#[test]
fn read_key_sequence_state_tracks_raw_and_translated_events() {
    crate::test_utils::init_test_tracing();
    let mut state = ReadKeySequenceState::new();
    state.push_input_event(Value::fixnum('A' as i64));
    state.push_input_event(Value::fixnum('B' as i64));
    state.replace_translated_events(vec![Value::fixnum('a' as i64)]);

    let (translated, raw) = state.snapshot();
    assert_eq!(translated, vec![Value::fixnum('a' as i64)]);
    assert_eq!(
        raw,
        vec![Value::fixnum('A' as i64), Value::fixnum('B' as i64)]
    );
}

#[test]
fn key_sequence_translation_events_normalizes_vector_string_and_scalar() {
    crate::test_utils::init_test_tracing();
    let vector = Value::vector(vec![Value::fixnum('x' as i64), Value::fixnum('y' as i64)]);
    assert_eq!(
        key_sequence_translation_events(vector),
        Some(vec![Value::fixnum('x' as i64), Value::fixnum('y' as i64)])
    );
    assert_eq!(
        key_sequence_translation_events(Value::string("ab")),
        Some(vec![Value::fixnum('a' as i64), Value::fixnum('b' as i64)])
    );
    assert_eq!(
        key_sequence_translation_events(Value::symbol("f1")),
        Some(vec![Value::symbol("f1")])
    );
    assert_eq!(key_sequence_translation_events(Value::NIL), None);
}

#[test]
fn parse_interactive_spec_all_codes() {
    crate::test_utils::init_test_tracing();
    let codes = parse_interactive_spec("d");
    assert!(matches!(&codes[0], InteractiveCode::Point));

    let codes = parse_interactive_spec("m");
    assert!(matches!(&codes[0], InteractiveCode::Mark));

    let codes = parse_interactive_spec("r");
    assert!(matches!(&codes[0], InteractiveCode::Region));

    let codes = parse_interactive_spec("p");
    assert!(matches!(&codes[0], InteractiveCode::PrefixNumeric));

    let codes = parse_interactive_spec("P");
    assert!(matches!(&codes[0], InteractiveCode::PrefixRaw));

    let codes = parse_interactive_spec("fFile: ");
    assert!(matches!(&codes[0], InteractiveCode::FileName(p) if p.as_utf8_str() == Some("File: ")));

    let codes = parse_interactive_spec("DDirectory: ");
    assert!(
        matches!(&codes[0], InteractiveCode::DirectoryName(p) if p.as_utf8_str() == Some("Directory: "))
    );
}

#[test]
fn parse_interactive_spec_empty() {
    crate::test_utils::init_test_tracing();
    let codes = parse_interactive_spec("");
    assert_eq!(codes.len(), 1);
    assert!(matches!(&codes[0], InteractiveCode::None));
}

#[test]
fn inhibit_quit_blocks_signal() {
    crate::test_utils::init_test_tracing();
    let mut cl = CommandLoop::new();
    cl.inhibit_quit = true;
    cl.signal_quit();
    assert!(!cl.quit_flag); // should not be set when inhibited
}

// ===================================================================
// keysym_to_key_event — control characters
// ===================================================================

#[test]
fn keysym_ctrl_x_from_control_char() {
    crate::test_utils::init_test_tracing();
    // Ctrl+x → winit gives keysym 0x18 (control character)
    let event = keysym_to_key_event(0x18, RENDER_CTRL_MASK).unwrap();
    assert_eq!(event.key, Key::Char('x'));
    assert!(event.modifiers.ctrl);
}

#[test]
fn keysym_ctrl_a_from_control_char() {
    crate::test_utils::init_test_tracing();
    let event = keysym_to_key_event(0x01, RENDER_CTRL_MASK).unwrap();
    assert_eq!(event.key, Key::Char('a'));
    assert!(event.modifiers.ctrl);
}

#[test]
fn keysym_ctrl_z_from_control_char() {
    crate::test_utils::init_test_tracing();
    let event = keysym_to_key_event(0x1A, RENDER_CTRL_MASK).unwrap();
    assert_eq!(event.key, Key::Char('z'));
    assert!(event.modifiers.ctrl);
}

#[test]
fn keysym_ctrl_g_from_control_char_no_modifier() {
    crate::test_utils::init_test_tracing();
    // Even without explicit ctrl modifier bit, control char implies ctrl
    let event = keysym_to_key_event(0x07, 0).unwrap();
    assert_eq!(event.key, Key::Char('g'));
    assert!(event.modifiers.ctrl);
}

#[test]
fn keysym_ctrl_x_from_printable_with_modifier() {
    crate::test_utils::init_test_tracing();
    // Ctrl+x when winit gives keysym 0x78 ('x') with ctrl modifier
    let event = keysym_to_key_event(0x78, RENDER_CTRL_MASK).unwrap();
    assert_eq!(event.key, Key::Char('x'));
    assert!(event.modifiers.ctrl);
}

#[test]
fn keysym_shifted_uppercase_char_drops_shift_modifier() {
    crate::test_utils::init_test_tracing();
    let event = keysym_to_key_event('A' as u32, RENDER_SHIFT_MASK).unwrap();
    assert_eq!(event.key, Key::Char('A'));
    assert!(!event.modifiers.shift);
}

#[test]
fn keysym_unicode_scalar_maps_to_character_event() {
    crate::test_utils::init_test_tracing();
    let event = keysym_to_key_event('中' as u32, 0).unwrap();
    assert_eq!(event.key, Key::Char('中'));
    assert!(event.modifiers.is_empty());
}

#[test]
fn keysym_ctrl_shift_x_drops_shift_modifier() {
    crate::test_utils::init_test_tracing();
    let event = keysym_to_key_event(0x18, RENDER_CTRL_MASK | RENDER_SHIFT_MASK).unwrap();
    assert_eq!(event.key, Key::Char('x'));
    assert!(event.modifiers.ctrl);
    assert!(!event.modifiers.shift);
}

#[test]
fn render_modifiers_helper_matches_transport_bit_layout() {
    crate::test_utils::init_test_tracing();
    let mods =
        render_modifiers_to_modifiers(RENDER_SHIFT_MASK | RENDER_CTRL_MASK | RENDER_META_MASK);
    assert!(mods.shift);
    assert!(mods.ctrl);
    assert!(mods.meta);
    assert!(!mods.super_);
    assert!(!mods.hyper);
}

#[test]
fn render_key_transport_drops_key_releases() {
    crate::test_utils::init_test_tracing();
    assert!(render_key_transport_to_input_event(XK_RETURN, 0, false, 0).is_none());
}
