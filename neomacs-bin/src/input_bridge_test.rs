use super::*;

#[test]
fn key_release_is_dropped_by_core_transport_owner() {
    let event = convert_display_event(DisplayEvent::Key {
        keysym: keyboard::XK_RETURN,
        modifiers: 0,
        pressed: false,
        emacs_frame_id: 0,
    });
    assert!(event.is_none());
}

#[test]
fn key_transport_preserves_source_frame_identity() {
    let event = convert_display_event(DisplayEvent::Key {
        keysym: 'a' as u32,
        modifiers: keyboard::RENDER_CTRL_MASK,
        pressed: true,
        emacs_frame_id: 42,
    });

    match event {
        Some(KbInputEvent::KeyPress {
            key,
            emacs_frame_id,
        }) => {
            assert_eq!(
                key,
                keyboard::KeyEvent::char_with_mods('a', keyboard::Modifiers::ctrl())
            );
            assert_eq!(emacs_frame_id, 42);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn mouse_modifiers_use_core_transport_mapping() {
    let event = convert_display_event(DisplayEvent::MouseMove {
        x: 1.0,
        y: 2.0,
        modifiers: keyboard::RENDER_SHIFT_MASK | keyboard::RENDER_CTRL_MASK,
        target_frame_id: 7,
    });

    match event {
        Some(KbInputEvent::MouseMove {
            modifiers,
            target_frame_id,
            ..
        }) => {
            assert!(modifiers.shift);
            assert!(modifiers.ctrl);
            assert!(!modifiers.meta);
            assert_eq!(target_frame_id, 7);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn mouse_button_preserves_target_frame_for_keyboard_owner() {
    let event = convert_display_event(DisplayEvent::MouseButton {
        button: 1,
        x: 10.0,
        y: 20.0,
        pressed: true,
        modifiers: 0,
        target_frame_id: 42,
        webkit_id: 0,
        webkit_rel_x: 0,
        webkit_rel_y: 0,
    });

    match event {
        Some(KbInputEvent::MousePress {
            target_frame_id: 42,
            ..
        }) => {}
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn window_focus_preserves_frame_id_for_keyboard_owner() {
    let event = convert_display_event(DisplayEvent::WindowFocus {
        focused: true,
        emacs_frame_id: 42,
    });

    match event {
        Some(KbInputEvent::Focus {
            focused: true,
            emacs_frame_id: 42,
        }) => {}
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn window_close_preserves_frame_id_for_keyboard_owner() {
    let event = convert_display_event(DisplayEvent::WindowClose { emacs_frame_id: 42 });

    match event {
        Some(KbInputEvent::WindowClose { emacs_frame_id: 42 }) => {}
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn monitor_changes_convert_to_core_monitor_snapshot() {
    let event = convert_display_event(DisplayEvent::MonitorsChanged {
        monitors: vec![DisplayMonitorInfo {
            x: 10,
            y: 20,
            width: 1920,
            height: 1080,
            scale: 1.5,
            width_mm: 510,
            height_mm: 290,
            name: Some("DP-1".to_string()),
        }],
    });

    match event {
        Some(KbInputEvent::MonitorsChanged { monitors }) => {
            assert_eq!(monitors.len(), 1);
            assert_eq!(monitors[0].name.as_deref(), Some("DP-1"));
            assert_eq!(monitors[0].width, 1920);
            assert_eq!(monitors[0].height, 1080);
            assert_eq!(monitors[0].scale, 1.5);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}
