//! Bridge between display runtime InputEvent and neovm-core keyboard::InputEvent.
//!
//! The display runtime sends raw keysyms and modifier bitmasks; neovm-core
//! expects structured `KeyEvent` / `InputEvent` types.  This module converts
//! between the two so that `neomacs-bin` can feed render-thread input into the
//! evaluator's command loop.

use neomacs_display_runtime::thread_comm::{
    InputEvent as DisplayEvent, MonitorInfo as DisplayMonitorInfo,
};
use neovm_core::emacs_core::builtins::NeomacsMonitorInfo;
use neovm_core::keyboard::{self, InputEvent as KbInputEvent, MouseButton};

pub(crate) fn convert_monitor_infos(monitors: &[DisplayMonitorInfo]) -> Vec<NeomacsMonitorInfo> {
    monitors
        .iter()
        .map(|monitor| NeomacsMonitorInfo {
            x: monitor.x,
            y: monitor.y,
            width: monitor.width,
            height: monitor.height,
            scale: monitor.scale,
            width_mm: monitor.width_mm,
            height_mm: monitor.height_mm,
            name: monitor.name.clone(),
        })
        .collect()
}

/// Convert a display runtime input event to a neovm-core keyboard input event.
///
/// Returns `None` for events that should be silently dropped (e.g. key
/// releases, modifier-only keys).
pub fn convert_display_event(event: DisplayEvent) -> Option<KbInputEvent> {
    match event {
        DisplayEvent::Key {
            keysym,
            modifiers,
            pressed,
            emacs_frame_id,
        } => {
            tracing::debug!(
                "input_bridge: key keysym=0x{:04x} mods=0x{:x} pressed={}",
                keysym,
                modifiers,
                pressed
            );
            let event = keyboard::render_key_transport_to_input_event(
                keysym,
                modifiers,
                pressed,
                emacs_frame_id,
            )?;
            tracing::debug!("input_bridge: converted to {:?}", event);
            Some(event)
        }
        DisplayEvent::MouseButton {
            button,
            x,
            y,
            pressed,
            modifiers,
            target_frame_id,
            ..
        } => {
            let mb = match button {
                1 => MouseButton::Left,
                2 => MouseButton::Middle,
                3 => MouseButton::Right,
                4 => MouseButton::Button4,
                5 => MouseButton::Button5,
                _ => return None,
            };
            if pressed {
                Some(KbInputEvent::MousePress {
                    button: mb,
                    x,
                    y,
                    modifiers: keyboard::render_modifiers_to_modifiers(modifiers),
                    target_frame_id,
                })
            } else {
                Some(KbInputEvent::MouseRelease {
                    button: mb,
                    x,
                    y,
                    target_frame_id,
                })
            }
        }
        DisplayEvent::MouseMove {
            x,
            y,
            modifiers,
            target_frame_id,
            ..
        } => Some(KbInputEvent::MouseMove {
            x,
            y,
            modifiers: keyboard::render_modifiers_to_modifiers(modifiers),
            target_frame_id,
        }),
        DisplayEvent::MouseScroll {
            delta_x,
            delta_y,
            x,
            y,
            modifiers,
            target_frame_id,
            ..
        } => Some(KbInputEvent::MouseScroll {
            delta_x,
            delta_y,
            x,
            y,
            modifiers: keyboard::render_modifiers_to_modifiers(modifiers),
            target_frame_id,
        }),
        DisplayEvent::WindowResize {
            width,
            height,
            emacs_frame_id,
        } => {
            tracing::debug!(
                "input_bridge: resize {}x{} emacs_frame_id=0x{:x}",
                width,
                height,
                emacs_frame_id
            );
            Some(KbInputEvent::Resize {
                width,
                height,
                emacs_frame_id,
            })
        }
        DisplayEvent::WindowClose { emacs_frame_id } => {
            Some(KbInputEvent::WindowClose { emacs_frame_id })
        }
        DisplayEvent::WindowFocus {
            focused,
            emacs_frame_id,
        } => Some(KbInputEvent::Focus {
            focused,
            emacs_frame_id,
        }),
        DisplayEvent::MonitorsChanged { monitors } => Some(KbInputEvent::MonitorsChanged {
            monitors: convert_monitor_infos(&monitors),
        }),
        // Ignore other events (WebKit title changes, etc.)
        _ => None,
    }
}

#[cfg(test)]
mod tests {
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
}
