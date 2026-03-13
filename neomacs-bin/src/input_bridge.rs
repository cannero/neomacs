//! Bridge between display runtime InputEvent and neovm-core keyboard::InputEvent.
//!
//! The display runtime sends raw keysyms and modifier bitmasks; neovm-core
//! expects structured `KeyEvent` / `InputEvent` types.  This module converts
//! between the two so that `neomacs-bin` can feed render-thread input into the
//! evaluator's command loop.

use neomacs_display_runtime::thread_comm::InputEvent as DisplayEvent;
use neovm_core::keyboard::{self, InputEvent as KbInputEvent, Modifiers, MouseButton};

// Render thread modifier bitmask constants (must match thread_comm.rs).
const RENDER_SHIFT_MASK: u32 = 1 << 0;
const RENDER_CTRL_MASK: u32 = 1 << 1;
const RENDER_META_MASK: u32 = 1 << 2;
const RENDER_SUPER_MASK: u32 = 1 << 3;

/// Convert a render-thread modifier bitmask to neovm-core `Modifiers`.
fn render_mods_to_modifiers(bits: u32) -> Modifiers {
    Modifiers {
        ctrl: bits & RENDER_CTRL_MASK != 0,
        meta: bits & RENDER_META_MASK != 0,
        shift: bits & RENDER_SHIFT_MASK != 0,
        super_: bits & RENDER_SUPER_MASK != 0,
        hyper: false,
    }
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
        } => {
            if !pressed {
                return None; // Ignore key releases
            }
            let key_event = keyboard::keysym_to_key_event(keysym, modifiers)?;
            Some(KbInputEvent::KeyPress(key_event))
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
                    modifiers: render_mods_to_modifiers(modifiers),
                })
            } else {
                Some(KbInputEvent::MouseRelease {
                    button: mb,
                    x,
                    y,
                })
            }
        }
        DisplayEvent::MouseMove {
            x, y, modifiers, ..
        } => Some(KbInputEvent::MouseMove {
            x,
            y,
            modifiers: render_mods_to_modifiers(modifiers),
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
            modifiers: render_mods_to_modifiers(modifiers),
        }),
        DisplayEvent::WindowResize { width, height, .. } => {
            Some(KbInputEvent::Resize { width, height })
        }
        DisplayEvent::WindowClose { .. } => Some(KbInputEvent::CloseRequested),
        DisplayEvent::WindowFocus { focused, .. } => Some(KbInputEvent::Focus(focused)),
        // Ignore other events (WebKit title changes, etc.)
        _ => None,
    }
}
