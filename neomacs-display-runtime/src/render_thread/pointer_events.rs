//! Pointer, wheel, and hover handling for winit window events.

use super::RenderApp;
use crate::backend::wgpu::NEOMACS_SUPER_MASK;
use crate::core::frame_glyphs::FrameGlyph;
use crate::thread_comm::InputEvent;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, MouseScrollDelta};

/// Search a glyph buffer for a WebKit view at the given local coordinates.
/// Returns `(webkit_id, relative_x, relative_y)` if found.
fn webkit_glyph_hit_test(glyphs: &[FrameGlyph], x: f32, y: f32) -> Option<(u32, i32, i32)> {
    for glyph in glyphs.iter().rev() {
        if let FrameGlyph::WebKit {
            webkit_id,
            x: wx,
            y: wy,
            width,
            height,
            ..
        } = glyph
        {
            if x >= *wx && x < *wx + *width && y >= *wy && y < *wy + *height {
                return Some((*webkit_id, (x - *wx) as i32, (y - *wy) as i32));
            }
        }
    }
    None
}

impl RenderApp {
    fn glyphs_for_pointer_target(&self, target_fid: u64) -> Option<&[FrameGlyph]> {
        if target_fid != 0 {
            self.child_frames
                .frames
                .get(&target_fid)
                .map(|entry| entry.frame.glyphs.as_slice())
        } else {
            self.current_frame
                .as_ref()
                .map(|frame| frame.glyphs.as_slice())
        }
    }

    fn pointer_target_at(&self, x: f32, y: f32) -> (f32, f32, u64) {
        if let Some((fid, local_x, local_y)) = self.child_frames.hit_test(x, y) {
            (local_x, local_y, fid)
        } else {
            (x, y, 0)
        }
    }

    fn webkit_target_at(&self, target_fid: u64, ev_x: f32, ev_y: f32) -> (u32, i32, i32) {
        let mut wk_id = 0u32;
        let mut wk_rx = 0i32;
        let mut wk_ry = 0i32;

        if let Some(glyphs) = self.glyphs_for_pointer_target(target_fid) {
            if let Some((id, rx, ry)) = webkit_glyph_hit_test(glyphs, ev_x, ev_y) {
                wk_id = id;
                wk_rx = rx;
                wk_ry = ry;
            }
        }

        #[cfg(feature = "wpe-webkit")]
        if wk_id == 0 && target_fid == 0 {
            let mx = self.mouse_pos.0;
            let my = self.mouse_pos.1;
            for wk in self.floating_webkits.iter().rev() {
                if mx >= wk.x && mx < wk.x + wk.width && my >= wk.y && my < wk.y + wk.height {
                    wk_id = wk.webkit_id;
                    wk_rx = (mx - wk.x) as i32;
                    wk_ry = (my - wk.y) as i32;
                    break;
                }
            }
        }

        (wk_id, wk_rx, wk_ry)
    }

    pub(super) fn handle_mouse_input(&mut self, state: ElementState, button: MouseButton) {
        if state == ElementState::Pressed {
            tracing::debug!(
                "MouseInput: {:?} at ({:.1}, {:.1}), menu_bar_h={}, popup={}",
                button,
                self.mouse_pos.0,
                self.mouse_pos.1,
                self.menu_bar_height,
                self.popup_menu.is_some()
            );
        }

        if let Some(ref mut menu) = self.popup_menu {
            if state == ElementState::Pressed && button == MouseButton::Left {
                if self.menu_bar_height > 0.0 && self.mouse_pos.1 < self.menu_bar_height {
                    if let Some(idx) = self.menu_bar_hit_test(self.mouse_pos.0, self.mouse_pos.1) {
                        self.comms
                            .send_input(InputEvent::MenuSelection { index: -1 });
                        self.popup_menu = None;
                        self.menu_bar_active = Some(idx);
                        self.comms
                            .send_input(InputEvent::MenuBarClick { index: idx as i32 });
                        self.frame_dirty = true;
                    } else {
                        self.comms
                            .send_input(InputEvent::MenuSelection { index: -1 });
                        self.popup_menu = None;
                        self.menu_bar_active = None;
                        self.frame_dirty = true;
                    }
                } else {
                    let idx = menu.hit_test(self.mouse_pos.0, self.mouse_pos.1);
                    if idx >= 0 {
                        self.comms
                            .send_input(InputEvent::MenuSelection { index: idx });
                        self.popup_menu = None;
                        self.menu_bar_active = None;
                        self.frame_dirty = true;
                    } else {
                        let (depth, local_idx) =
                            menu.hit_test_all(self.mouse_pos.0, self.mouse_pos.1);
                        if depth >= 0 && local_idx >= 0 {
                            let panel = if depth == 0 {
                                &menu.root_panel
                            } else {
                                &menu.submenu_panels[(depth - 1) as usize]
                            };
                            let global_idx = panel.item_indices[local_idx as usize];
                            if menu.all_items[global_idx].submenu {
                                self.frame_dirty = true;
                            } else {
                                self.comms
                                    .send_input(InputEvent::MenuSelection { index: -1 });
                                self.popup_menu = None;
                                self.menu_bar_active = None;
                                self.frame_dirty = true;
                            }
                        } else {
                            self.comms
                                .send_input(InputEvent::MenuSelection { index: -1 });
                            self.popup_menu = None;
                            self.menu_bar_active = None;
                            self.frame_dirty = true;
                        }
                    }
                }
            } else if state == ElementState::Pressed {
                self.comms
                    .send_input(InputEvent::MenuSelection { index: -1 });
                self.popup_menu = None;
                self.menu_bar_active = None;
                self.frame_dirty = true;
            }
            return;
        }

        if state == ElementState::Pressed
            && button == MouseButton::Left
            && self.chrome.resize_edge.is_some()
        {
            if let (Some(dir), Some(ref window)) = (self.chrome.resize_edge, self.window.as_ref()) {
                let _ = window.drag_resize_window(dir);
            }
            return;
        }

        if state == ElementState::Pressed
            && button == MouseButton::Left
            && self.titlebar_hit_test(self.mouse_pos.0, self.mouse_pos.1) > 0
        {
            match self.titlebar_hit_test(self.mouse_pos.0, self.mouse_pos.1) {
                1 => {
                    let now = std::time::Instant::now();
                    if now
                        .duration_since(self.chrome.last_titlebar_click)
                        .as_millis()
                        < 400
                    {
                        if let Some(ref window) = self.window {
                            window.set_maximized(!window.is_maximized());
                        }
                    } else if let Some(ref window) = self.window {
                        let _ = window.drag_window();
                    }
                    self.chrome.last_titlebar_click = now;
                }
                2 => {
                    self.comms
                        .send_input(InputEvent::WindowClose { emacs_frame_id: 0 });
                }
                3 => {
                    if let Some(ref window) = self.window {
                        if window.is_maximized() {
                            window.set_maximized(false);
                        } else {
                            window.set_maximized(true);
                        }
                    }
                }
                4 => {
                    if let Some(ref window) = self.window {
                        window.set_minimized(true);
                    }
                }
                _ => {}
            }
            return;
        }

        if state == ElementState::Pressed
            && button == MouseButton::Left
            && !self.chrome.decorations_enabled
            && (self.modifiers & NEOMACS_SUPER_MASK) != 0
        {
            if let Some(ref window) = self.window {
                let _ = window.drag_window();
            }
            return;
        }

        if state == ElementState::Pressed
            && button == MouseButton::Left
            && self.menu_bar_height > 0.0
            && self.mouse_pos.1 < self.menu_bar_height
        {
            tracing::debug!(
                "Menu bar click at ({:.1}, {:.1}), menu_bar_height={}",
                self.mouse_pos.0,
                self.mouse_pos.1,
                self.menu_bar_height
            );
            if let Some(idx) = self.menu_bar_hit_test(self.mouse_pos.0, self.mouse_pos.1) {
                if self.menu_bar_active == Some(idx) {
                    self.menu_bar_active = None;
                } else {
                    self.menu_bar_active = Some(idx);
                    self.comms
                        .send_input(InputEvent::MenuBarClick { index: idx as i32 });
                }
                self.frame_dirty = true;
            }
            return;
        }

        // Tab bar click (between menu bar and toolbar)
        if state == ElementState::Pressed
            && button == MouseButton::Left
            && self.tab_bar_height > 0.0
            && self.mouse_pos.1 >= self.tab_bar_y
            && self.mouse_pos.1 < self.tab_bar_y + self.tab_bar_height
        {
            if let Some(idx) = self.tab_bar_hit_test(self.mouse_pos.0, self.mouse_pos.1) {
                self.tab_bar_pressed = Some(idx);
                self.comms
                    .send_input(InputEvent::TabBarClick { index: idx as i32 });
                self.frame_dirty = true;
            }
            return;
        }

        if state == ElementState::Released
            && button == MouseButton::Left
            && self.tab_bar_pressed.is_some()
        {
            self.tab_bar_pressed = None;
            self.frame_dirty = true;
            return;
        }

        if state == ElementState::Pressed
            && button == MouseButton::Left
            && self.toolbar_height > 0.0
            && self.mouse_pos.1 < self.toolbar_y_origin() + self.toolbar_height
            && self.mouse_pos.1 >= self.toolbar_y_origin()
        {
            if let Some(idx) =
                self.toolbar_hit_test(self.mouse_pos.0, self.mouse_pos.1 - self.toolbar_y_origin())
            {
                self.toolbar_pressed = Some(idx);
                self.comms
                    .send_input(InputEvent::ToolBarClick { index: idx as i32 });
                self.frame_dirty = true;
            }
            return;
        }

        if state == ElementState::Released
            && button == MouseButton::Left
            && self.toolbar_pressed.is_some()
        {
            self.toolbar_pressed = None;
            self.frame_dirty = true;
            return;
        }

        if state == ElementState::Pressed && button == MouseButton::Left {
            tracing::trace!(
                "Left click at ({:.1}, {:.1}) NOT in menu bar (h={}) or toolbar (h={})",
                self.mouse_pos.0,
                self.mouse_pos.1,
                self.menu_bar_height,
                self.toolbar_height
            );
        }

        let btn = match button {
            MouseButton::Left => 1,
            MouseButton::Middle => 2,
            MouseButton::Right => 3,
            MouseButton::Back => 4,
            MouseButton::Forward => 5,
            MouseButton::Other(n) => n as u32,
        };

        let (ev_x, ev_y, target_fid) = if let Some((fid, lx, ly)) = self
            .child_frames
            .hit_test(self.mouse_pos.0, self.mouse_pos.1)
        {
            if let Some(entry) = self.child_frames.frames.get(&fid) {
                tracing::trace!(
                    "Child frame hit: fid={} abs=({:.1},{:.1}) size=({:.1}x{:.1}) mouse=({:.1},{:.1}) local=({:.1},{:.1})",
                    fid,
                    entry.abs_x,
                    entry.abs_y,
                    entry.frame.width,
                    entry.frame.height,
                    self.mouse_pos.0,
                    self.mouse_pos.1,
                    lx,
                    ly
                );
            }
            (lx, ly, fid)
        } else {
            (self.mouse_pos.0, self.mouse_pos.1, 0)
        };

        let (wk_id, wk_rx, wk_ry) = if state == ElementState::Pressed {
            let (id, rx, ry) = self.webkit_target_at(target_fid, ev_x, ev_y);
            if id != 0 {
                tracing::trace!("WebKit hit: id={} rel=({},{})", id, rx, ry);
            }
            (id, rx, ry)
        } else {
            (0, 0, 0)
        };

        if state == ElementState::Pressed {
            tracing::trace!(
                "MouseButton: btn={} ev=({:.1},{:.1}) target_fid={} wk_id={} wk_rel=({},{})",
                btn,
                ev_x,
                ev_y,
                target_fid,
                wk_id,
                wk_rx,
                wk_ry
            );
        }

        self.comms.send_input(InputEvent::MouseButton {
            button: btn,
            x: ev_x,
            y: ev_y,
            pressed: state == ElementState::Pressed,
            modifiers: self.modifiers,
            target_frame_id: target_fid,
            webkit_id: wk_id,
            webkit_rel_x: wk_rx,
            webkit_rel_y: wk_ry,
        });

        if state == ElementState::Pressed && self.effects.click_halo.enabled {
            if let Some(renderer) = self.renderer.as_mut() {
                renderer.trigger_click_halo(
                    self.mouse_pos.0,
                    self.mouse_pos.1,
                    std::time::Instant::now(),
                );
            }
            self.frame_dirty = true;
        }
    }

    pub(super) fn handle_cursor_moved(&mut self, position: PhysicalPosition<f64>) {
        let lx = (position.x / self.scale_factor) as f32;
        let ly = (position.y / self.scale_factor) as f32;
        self.mouse_pos = (lx, ly);

        if self.effects.idle_dim.enabled {
            self.last_activity_time = std::time::Instant::now();
        }

        if self.mouse_hidden_for_typing {
            if let Some(ref window) = self.window {
                window.set_cursor_visible(true);
            }
            self.mouse_hidden_for_typing = false;
        }

        let edge = self.detect_resize_edge(lx, ly);
        if edge != self.chrome.resize_edge {
            self.chrome.resize_edge = edge;
            if let Some(ref window) = self.window {
                use winit::window::CursorIcon;
                let icon = match edge {
                    Some(dir) => CursorIcon::from(dir),
                    None => CursorIcon::Default,
                };
                window.set_cursor(icon);
            }
        }

        if !self.chrome.decorations_enabled {
            let new_hover = self.titlebar_hit_test(lx, ly);
            if new_hover != self.chrome.titlebar_hover {
                self.chrome.titlebar_hover = new_hover;
                self.frame_dirty = true;
                if self.chrome.resize_edge.is_none() {
                    if let Some(ref window) = self.window {
                        use winit::window::CursorIcon;
                        let icon = match new_hover {
                            2 | 3 | 4 => CursorIcon::Pointer,
                            _ => CursorIcon::Default,
                        };
                        window.set_cursor(icon);
                    }
                }
            }
        }

        if self.menu_bar_height > 0.0 {
            let old_hover = self.menu_bar_hovered;
            if ly < self.menu_bar_height {
                let new_hover = self.menu_bar_hit_test(lx, ly);
                self.menu_bar_hovered = new_hover;
                if let (Some(active), Some(hov)) = (self.menu_bar_active, new_hover) {
                    if hov != active {
                        self.menu_bar_active = Some(hov);
                        self.comms
                            .send_input(InputEvent::MenuBarClick { index: hov as i32 });
                    }
                }
            } else {
                self.menu_bar_hovered = None;
            }
            if self.menu_bar_hovered != old_hover {
                self.frame_dirty = true;
            }
        }

        if self.tab_bar_height > 0.0 {
            let old_hover = self.tab_bar_hovered;
            if ly >= self.tab_bar_y && ly < self.tab_bar_y + self.tab_bar_height {
                self.tab_bar_hovered = self.tab_bar_hit_test(lx, ly);
            } else {
                self.tab_bar_hovered = None;
            }
            if self.tab_bar_hovered != old_hover {
                self.frame_dirty = true;
            }
        }

        if self.toolbar_height > 0.0 {
            let old_hover = self.toolbar_hovered;
            let toolbar_y = self.toolbar_y_origin();
            if ly < toolbar_y + self.toolbar_height && ly >= toolbar_y {
                self.toolbar_hovered = self.toolbar_hit_test(lx, ly - toolbar_y);
            } else {
                self.toolbar_hovered = None;
            }
            if self.toolbar_hovered != old_hover {
                self.frame_dirty = true;
            }
        }

        if let Some(ref mut menu) = self.popup_menu {
            let (hit_depth, hit_local) = menu.hit_test_all(lx, ly);
            if hit_depth >= 0 {
                let target_depth = hit_depth as usize;
                while menu.submenu_panels.len() > target_depth {
                    menu.submenu_panels.pop();
                    self.frame_dirty = true;
                }
                let panel = if target_depth == 0 {
                    &mut menu.root_panel
                } else {
                    &mut menu.submenu_panels[target_depth - 1]
                };
                if hit_local != panel.hover_index {
                    panel.hover_index = hit_local;
                    self.frame_dirty = true;
                    if hit_local >= 0 && (hit_local as usize) < panel.item_indices.len() {
                        let global_idx = panel.item_indices[hit_local as usize];
                        if menu.all_items[global_idx].submenu {
                            menu.open_submenu();
                        }
                    }
                }
            }
            return;
        }

        let (ev_x, ev_y, target_fid) = self.pointer_target_at(lx, ly);
        self.comms.send_input(InputEvent::MouseMove {
            x: ev_x,
            y: ev_y,
            modifiers: self.modifiers,
            target_frame_id: target_fid,
        });
    }

    pub(super) fn handle_mouse_wheel(&mut self, delta: MouseScrollDelta) {
        let (dx, dy, pixel_precise) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (x, y, false),
            MouseScrollDelta::PixelDelta(pos) => (
                (pos.x / self.scale_factor) as f32,
                (pos.y / self.scale_factor) as f32,
                true,
            ),
        };

        let (ev_x, ev_y, target_fid) = self.pointer_target_at(self.mouse_pos.0, self.mouse_pos.1);
        let (wk_id, wk_rx, wk_ry) = self.webkit_target_at(target_fid, ev_x, ev_y);

        self.comms.send_input(InputEvent::MouseScroll {
            delta_x: dx,
            delta_y: dy,
            x: ev_x,
            y: ev_y,
            modifiers: self.modifiers,
            pixel_precise,
            target_frame_id: target_fid,
            webkit_id: wk_id,
            webkit_rel_x: wk_rx,
            webkit_rel_y: wk_ry,
        });
    }
}
