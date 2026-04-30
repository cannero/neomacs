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
pub fn convert_display_event(event: &DisplayEvent) -> Option<KbInputEvent> {
    match event {
        DisplayEvent::Key {
            keysym,
            modifiers,
            pressed,
            emacs_frame_id,
        } => {
            tracing::debug!(
                "input_bridge: key keysym=0x{:04x} mods=0x{:x} pressed={}",
                *keysym,
                *modifiers,
                *pressed
            );
            let event = keyboard::render_key_transport_to_input_event(
                *keysym,
                *modifiers,
                *pressed,
                *emacs_frame_id,
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
            let mb = match *button {
                1 => MouseButton::Left,
                2 => MouseButton::Middle,
                3 => MouseButton::Right,
                4 => MouseButton::Button4,
                5 => MouseButton::Button5,
                _ => return None,
            };
            if *pressed {
                Some(KbInputEvent::MousePress {
                    button: mb,
                    x: *x,
                    y: *y,
                    modifiers: keyboard::render_modifiers_to_modifiers(*modifiers),
                    target_frame_id: *target_frame_id,
                })
            } else {
                Some(KbInputEvent::MouseRelease {
                    button: mb,
                    x: *x,
                    y: *y,
                    target_frame_id: *target_frame_id,
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
            x: *x,
            y: *y,
            modifiers: keyboard::render_modifiers_to_modifiers(*modifiers),
            target_frame_id: *target_frame_id,
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
            delta_x: *delta_x,
            delta_y: *delta_y,
            x: *x,
            y: *y,
            modifiers: keyboard::render_modifiers_to_modifiers(*modifiers),
            target_frame_id: *target_frame_id,
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
                width: *width,
                height: *height,
                emacs_frame_id: *emacs_frame_id,
            })
        }
        DisplayEvent::WindowClose { emacs_frame_id } => Some(KbInputEvent::WindowClose {
            emacs_frame_id: *emacs_frame_id,
        }),
        DisplayEvent::WindowFocus {
            focused,
            emacs_frame_id,
        } => Some(KbInputEvent::Focus {
            focused: *focused,
            emacs_frame_id: *emacs_frame_id,
        }),
        DisplayEvent::MonitorsChanged { monitors } => Some(KbInputEvent::MonitorsChanged {
            monitors: convert_monitor_infos(monitors),
        }),
        // Ignore other events (WebKit title changes, etc.)
        _ => None,
    }
}

#[cfg(test)]
#[path = "input_bridge_test.rs"]
mod tests;
