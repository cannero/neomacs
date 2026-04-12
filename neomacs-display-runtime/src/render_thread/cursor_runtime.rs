use super::{ImeCursorArea, RenderApp};
use crate::core::frame_glyphs::FrameGlyph;
use crate::render_thread::cursor::CursorTarget;
use winit::dpi::{PhysicalPosition, PhysicalSize};

impl RenderApp {
    /// Compute physical IME cursor rectangle for the current cursor target.
    pub(super) fn ime_cursor_area_for_target(&self, target: &CursorTarget) -> ImeCursorArea {
        // If cursor is in a child frame, offset by the child's absolute position.
        let (ime_off_x, ime_off_y) = if target.frame_id != 0 {
            self.child_frames
                .frames
                .get(&target.frame_id)
                .map(|e| (e.abs_x as f64, e.abs_y as f64))
                .unwrap_or((0.0, 0.0))
        } else {
            (0.0, 0.0)
        };

        ImeCursorArea {
            x: ((target.x as f64 + ime_off_x) * self.scale_factor).round() as i32,
            y: ((target.y as f64 + target.height as f64 + ime_off_y) * self.scale_factor).round()
                as i32,
            width: ((target.width as f64 * self.scale_factor).max(1.0)).round() as u32,
            height: ((target.height as f64 * self.scale_factor).max(1.0)).round() as u32,
        }
    }

    /// Update IME cursor area only when IME is active and the rectangle changed.
    pub(super) fn update_ime_cursor_area_if_needed(&mut self, target: &CursorTarget) {
        if !self.ime_enabled && !self.ime_preedit_active {
            return;
        }
        let Some(ref window) = self.window else {
            return;
        };

        let area = self.ime_cursor_area_for_target(target);
        if self.last_ime_cursor_area == Some(area) {
            return;
        }

        window.set_ime_cursor_area(
            PhysicalPosition::new(area.x as f64, area.y as f64),
            PhysicalSize::new(area.width as f64, area.height as f64),
        );
        self.last_ime_cursor_area = Some(area);
    }

    /// Update cursor blink state, returns true if blink toggled.
    pub(super) fn tick_cursor_blink(&mut self) -> bool {
        if !self.cursor.blink_enabled || self.current_frame.is_none() {
            return false;
        }
        let has_cursor = self
            .current_frame
            .as_ref()
            .map(|f| {
                f.phys_cursor.is_some()
                    || f.glyphs
                        .iter()
                        .any(|g| matches!(g, FrameGlyph::Cursor { .. }))
            })
            .unwrap_or(false);
        if !has_cursor {
            return false;
        }
        let now = std::time::Instant::now();
        if now.duration_since(self.cursor.last_blink_toggle) >= self.cursor.blink_interval {
            let was_off = !self.cursor.blink_on;
            self.cursor.blink_on = !self.cursor.blink_on;
            self.cursor.last_blink_toggle = now;
            if was_off && self.cursor.blink_on && self.effects.cursor_wake.enabled {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.trigger_cursor_wake(now);
                }
            }
            true
        } else {
            false
        }
    }
}
