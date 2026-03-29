//! Bridge between display runtime InputEvent and neovm-core keyboard::InputEvent.
//!
//! The display runtime sends raw keysyms and modifier bitmasks; neovm-core
//! expects structured `KeyEvent` / `InputEvent` types.  This module converts
//! between the two so that `neomacs-bin` can feed render-thread input into the
//! evaluator's command loop.

use neomacs_display_runtime::thread_comm::InputEvent as DisplayEvent;
use neovm_core::keyboard::{self, InputEvent as KbInputEvent, MouseButton};

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
        } => {
            tracing::debug!(
                "input_bridge: key keysym=0x{:04x} mods=0x{:x} pressed={}",
                keysym,
                modifiers,
                pressed
            );
            let event = keyboard::render_key_transport_to_input_event(keysym, modifiers, pressed)?;
            tracing::debug!("input_bridge: converted to {:?}", event);
            Some(event)
        }
        DisplayEvent::MouseButton {
            button,
            x,
            y,
            pressed,
            modifiers,
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
                })
            } else {
                Some(KbInputEvent::MouseRelease { button: mb, x, y })
            }
        }
        DisplayEvent::MouseMove {
            x, y, modifiers, ..
        } => Some(KbInputEvent::MouseMove {
            x,
            y,
            modifiers: keyboard::render_modifiers_to_modifiers(modifiers),
        }),
        DisplayEvent::MouseScroll {
            delta_x,
            delta_y,
            x,
            y,
            modifiers,
            ..
        } => Some(KbInputEvent::MouseScroll {
            delta_x,
            delta_y,
            x,
            y,
            modifiers: keyboard::render_modifiers_to_modifiers(modifiers),
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
        DisplayEvent::WindowClose { .. } => Some(KbInputEvent::CloseRequested),
        DisplayEvent::WindowFocus {
            focused,
            emacs_frame_id,
        } => Some(KbInputEvent::Focus {
            focused,
            emacs_frame_id,
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
        });
        assert!(event.is_none());
    }

    #[test]
    fn mouse_modifiers_use_core_transport_mapping() {
        let event = convert_display_event(DisplayEvent::MouseMove {
            x: 1.0,
            y: 2.0,
            modifiers: keyboard::RENDER_SHIFT_MASK | keyboard::RENDER_CTRL_MASK,
            target_frame_id: 0,
        });

        match event {
            Some(KbInputEvent::MouseMove { modifiers, .. }) => {
                assert!(modifiers.shift);
                assert!(modifiers.ctrl);
                assert!(!modifiers.meta);
            }
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
}
