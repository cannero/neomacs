//! Frame ingestion and cursor target extraction.

use super::RenderApp;
use crate::core::types::CursorAnimStyle;
use crate::render_thread::cursor::{CursorState, CursorTarget};

impl RenderApp {
    /// Get latest frame from Emacs (non-blocking).
    pub(super) fn poll_frame(&mut self) {
        self.child_frames.tick();
        while let Ok(display_state) = self.comms.frame_rx.try_recv() {
            let frame_id = display_state.frame_id;
            let parent_id = display_state.parent_id;
            let gui_menu_bar = display_state.gui_menu_bar.clone();
            let gui_tool_bar = display_state.gui_tool_bar.clone();

            // Materialize FrameDisplayState → FrameGlyphBuffer for the
            // existing rendering code.  The layout engine populates
            // the grid and non-grid items; materialize() converts the
            // grid into pixel-positioned glyphs and appends non-grid items.
            let frame = display_state.materialize();

            if frame_id != 0 && parent_id == 0 && self.multi_windows.windows.contains_key(&frame_id)
            {
                self.multi_windows.route_frame(frame);
                continue;
            }
            if parent_id != 0 && self.multi_windows.windows.contains_key(&parent_id) {
                self.multi_windows.route_frame(frame);
                continue;
            }

            if parent_id != 0 {
                self.child_frames.update_frame(frame);
            } else {
                self.current_frame = Some(frame);
                if let Some(menu_bar) = gui_menu_bar {
                    self.menu_bar_items = menu_bar.items;
                    self.menu_bar_height = menu_bar.height;
                    self.menu_bar_fg = menu_bar.fg;
                    self.menu_bar_bg = menu_bar.bg;
                } else {
                    self.menu_bar_items.clear();
                    self.menu_bar_height = 0.0;
                    self.menu_bar_hovered = None;
                    self.menu_bar_active = None;
                }
                if let Some(tool_bar) = gui_tool_bar {
                    self.ensure_toolbar_icon_textures(&tool_bar.items);
                    self.toolbar_items = tool_bar.items;
                    self.toolbar_height = tool_bar.height;
                    self.toolbar_fg = tool_bar.fg;
                    self.toolbar_bg = tool_bar.bg;
                } else {
                    self.toolbar_items.clear();
                    self.toolbar_height = 0.0;
                    self.toolbar_hovered = None;
                    self.toolbar_pressed = None;
                }
                if let Some(tab_bar) = self
                    .current_frame
                    .as_ref()
                    .and_then(|frame| frame.tab_bar.as_ref())
                {
                    self.tab_bar_items = tab_bar.items.clone();
                    self.tab_bar_y = tab_bar.y;
                    self.tab_bar_height = tab_bar.height;
                } else {
                    self.tab_bar_items.clear();
                    self.tab_bar_y = 0.0;
                    self.tab_bar_height = 0.0;
                    self.tab_bar_hovered = None;
                    self.tab_bar_pressed = None;
                }
                self.cursor.reset_blink();
            }
            self.frame_dirty = true;
        }

        let mut active_cursor: Option<CursorTarget> =
            self.current_frame.as_ref().and_then(|frame| {
                frame.phys_cursor.as_ref().map(|cursor| CursorTarget {
                    window_id: cursor.window_id,
                    x: cursor.x,
                    y: cursor.y,
                    width: cursor.width,
                    height: cursor.height,
                    style: cursor.style,
                    color: cursor.color,
                    frame_id: 0,
                })
            });

        if active_cursor.is_none() {
            for (_, entry) in &self.child_frames.frames {
                if let Some(cursor) = entry.frame.phys_cursor.as_ref() {
                    active_cursor = Some(CursorTarget {
                        window_id: cursor.window_id,
                        x: cursor.x,
                        y: cursor.y,
                        width: cursor.width,
                        height: cursor.height,
                        style: cursor.style,
                        color: cursor.color,
                        frame_id: entry.frame_id,
                    });
                    break;
                }
            }
        }

        if let Some(new_target) = active_cursor {
            let had_target = self.cursor.target.is_some();
            let target_moved = self.cursor.target.as_ref().map_or(true, |old| {
                (old.x - new_target.x).abs() > 0.5
                    || (old.y - new_target.y).abs() > 0.5
                    || (old.width - new_target.width).abs() > 0.5
                    || (old.height - new_target.height).abs() > 0.5
            });

            if !had_target || !self.cursor.anim_enabled {
                self.cursor.current_x = new_target.x;
                self.cursor.current_y = new_target.y;
                self.cursor.current_w = new_target.width;
                self.cursor.current_h = new_target.height;
                self.cursor.animating = false;
                let corners = CursorState::target_corners(&new_target);
                for (spring, (x, y)) in self.cursor.corner_springs.iter_mut().zip(corners) {
                    spring.x = x;
                    spring.y = y;
                    spring.vx = 0.0;
                    spring.vy = 0.0;
                    spring.target_x = x;
                    spring.target_y = y;
                }
                self.cursor.prev_target_cx = new_target.x + new_target.width / 2.0;
                self.cursor.prev_target_cy = new_target.y + new_target.height / 2.0;
            } else if target_moved {
                let now = std::time::Instant::now();
                self.cursor.animating = true;
                self.cursor.last_anim_time = now;
                self.cursor.start_x = self.cursor.current_x;
                self.cursor.start_y = self.cursor.current_y;
                self.cursor.start_w = self.cursor.current_w;
                self.cursor.start_h = self.cursor.current_h;
                self.cursor.anim_start_time = now;
                self.cursor.velocity_x = 0.0;
                self.cursor.velocity_y = 0.0;
                self.cursor.velocity_w = 0.0;
                self.cursor.velocity_h = 0.0;

                if self.cursor.anim_style == CursorAnimStyle::CriticallyDampedSpring {
                    let new_corners = CursorState::target_corners(&new_target);
                    let new_cx = new_target.x + new_target.width / 2.0;
                    let new_cy = new_target.y + new_target.height / 2.0;
                    let old_cx = self.cursor.prev_target_cx;
                    let old_cy = self.cursor.prev_target_cy;

                    let dx = new_cx - old_cx;
                    let dy = new_cy - old_cy;
                    let len = (dx * dx + dy * dy).sqrt();
                    let (dir_x, dir_y) = if len > 0.001 {
                        (dx / len, dy / len)
                    } else {
                        (1.0, 0.0)
                    };

                    let corner_dirs: [(f32, f32); 4] =
                        [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)];

                    let mut dots: [(f32, usize); 4] = corner_dirs
                        .iter()
                        .enumerate()
                        .map(|(i, (cx, cy))| (cx * dir_x + cy * dir_y, i))
                        .collect::<Vec<_>>()
                        .try_into()
                        .unwrap();
                    dots.sort_by(|a, b| a.0.total_cmp(&b.0));

                    let base_dur = self.cursor.anim_duration;
                    for (rank, &(_dot, corner_idx)) in dots.iter().enumerate() {
                        let factor = 1.0 - self.cursor.trail_size * (rank as f32 / 3.0);
                        let duration_i = (base_dur * factor).max(0.01);
                        let omega_i = 4.0 / duration_i;

                        self.cursor.corner_springs[corner_idx].target_x = new_corners[corner_idx].0;
                        self.cursor.corner_springs[corner_idx].target_y = new_corners[corner_idx].1;
                        self.cursor.corner_springs[corner_idx].omega = omega_i;
                    }

                    self.cursor.prev_target_cx = new_cx;
                    self.cursor.prev_target_cy = new_cy;
                }
            }

            if target_moved && had_target && self.effects.typing_ripple.enabled {
                if let Some(renderer) = self.renderer.as_mut() {
                    let cx = new_target.x + new_target.width / 2.0;
                    let cy = new_target.y + new_target.height / 2.0;
                    renderer.spawn_ripple(cx, cy);
                }
            }

            if target_moved && had_target && self.effects.cursor_trail_fade.enabled {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.record_cursor_trail(
                        self.cursor.current_x,
                        self.cursor.current_y,
                        self.cursor.current_w,
                        self.cursor.current_h,
                    );
                }
            }

            self.update_ime_cursor_area_if_needed(&new_target);

            if self.cursor.size_transition_enabled {
                let dw = (new_target.width - self.cursor.size_target_w).abs();
                let dh = (new_target.height - self.cursor.size_target_h).abs();
                if dw > 2.0 || dh > 2.0 {
                    self.cursor.size_animating = true;
                    self.cursor.size_start_w = self.cursor.current_w;
                    self.cursor.size_start_h = self.cursor.current_h;
                    self.cursor.size_anim_start = std::time::Instant::now();
                }
                self.cursor.size_target_w = new_target.width;
                self.cursor.size_target_h = new_target.height;
            }

            self.cursor.target = Some(new_target);
        }
    }
}
