use super::RenderApp;
use crate::backend::wgpu::{
    NEOMACS_CTRL_MASK, NEOMACS_META_MASK, NEOMACS_SHIFT_MASK, NEOMACS_SUPER_MASK,
};
use crate::core::frame_glyphs::FrameGlyph;
use crate::thread_comm::InputEvent;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::WindowId;

/// Search a glyph buffer for a WebKit view at the given local coordinates.
/// Returns (webkit_id, relative_x, relative_y) if found.
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
    pub(super) fn handle_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                tracing::info!("Window close requested");
                let emacs_fid = self
                    .multi_windows
                    .emacs_frame_for_winit(_window_id)
                    .unwrap_or(0);
                self.comms.send_input(InputEvent::WindowClose {
                    emacs_frame_id: emacs_fid,
                });
                if emacs_fid == 0 {
                    // Primary window closing — exit
                    event_loop.exit();
                } else {
                    // Secondary window closing — just remove it
                    self.multi_windows.request_destroy(emacs_fid);
                }
            }

            WindowEvent::Resized(size) => {
                tracing::info!("WindowEvent::Resized: {}x{}", size.width, size.height);

                let emacs_fid = self
                    .multi_windows
                    .emacs_frame_for_winit(_window_id)
                    .unwrap_or(0);
                if emacs_fid == 0 {
                    // Primary window resize
                    self.handle_resize(size.width, size.height);
                    let logical_w = (size.width as f64 / self.scale_factor) as u32;
                    let logical_h = (size.height as f64 / self.scale_factor) as u32;
                    tracing::info!(
                        "Sending WindowResize event to Emacs: {}x{} (logical)",
                        logical_w,
                        logical_h
                    );
                    self.comms.send_input(InputEvent::WindowResize {
                        width: logical_w,
                        height: logical_h,
                        emacs_frame_id: 0,
                    });
                } else if let Some(device) = self.device.clone() {
                    // Secondary window resize
                    if let Some(ws) = self.multi_windows.get_mut(emacs_fid) {
                        ws.handle_resize(&device, size.width, size.height);
                        let scale = ws.scale_factor;
                        let logical_w = (size.width as f64 / scale) as u32;
                        let logical_h = (size.height as f64 / scale) as u32;
                        self.comms.send_input(InputEvent::WindowResize {
                            width: logical_w,
                            height: logical_h,
                            emacs_frame_id: emacs_fid,
                        });
                    }
                }
            }

            WindowEvent::Focused(focused) => {
                let emacs_fid = self
                    .multi_windows
                    .emacs_frame_for_winit(_window_id)
                    .unwrap_or(0);
                self.comms.send_input(InputEvent::WindowFocus {
                    focused,
                    emacs_frame_id: emacs_fid,
                });
            }

            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state,
                        text,
                        physical_key,
                        ..
                    },
                ..
            } => {
                if state == ElementState::Pressed {
                    tracing::debug!(
                        "KeyboardInput: logical_key={:?} physical_key={:?} text={:?} mods={} ime={}",
                        logical_key,
                        physical_key,
                        text,
                        self.modifiers,
                        self.ime_preedit_active
                    );
                }
                // If popup menu is active, handle keyboard navigation
                if self.popup_menu.is_some() && state == ElementState::Pressed {
                    match logical_key.as_ref() {
                        Key::Named(NamedKey::Escape) => {
                            self.comms
                                .send_input(InputEvent::MenuSelection { index: -1 });
                            self.popup_menu = None;
                            self.menu_bar_active = None;
                            self.frame_dirty = true;
                        }
                        Key::Named(NamedKey::ArrowDown) => {
                            if let Some(ref mut menu) = self.popup_menu {
                                if menu.move_hover(1) {
                                    self.frame_dirty = true;
                                }
                            }
                        }
                        Key::Named(NamedKey::ArrowUp) => {
                            if let Some(ref mut menu) = self.popup_menu {
                                if menu.move_hover(-1) {
                                    self.frame_dirty = true;
                                }
                            }
                        }
                        Key::Named(NamedKey::Enter) => {
                            if let Some(ref mut menu) = self.popup_menu {
                                let panel = menu.active_panel();
                                let hi = panel.hover_index;
                                if hi >= 0 && (hi as usize) < panel.item_indices.len() {
                                    let global_idx = panel.item_indices[hi as usize];
                                    if menu.all_items[global_idx].submenu {
                                        // Open submenu instead of selecting
                                        if menu.open_submenu() {
                                            self.frame_dirty = true;
                                        }
                                    } else {
                                        self.comms.send_input(InputEvent::MenuSelection {
                                            index: global_idx as i32,
                                        });
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
                        Key::Named(NamedKey::ArrowRight) => {
                            if let Some(ref mut menu) = self.popup_menu {
                                if menu.open_submenu() {
                                    self.frame_dirty = true;
                                }
                            }
                        }
                        Key::Named(NamedKey::ArrowLeft) => {
                            if let Some(ref mut menu) = self.popup_menu {
                                if menu.close_submenu() {
                                    self.frame_dirty = true;
                                }
                            }
                        }
                        Key::Named(NamedKey::Home) => {
                            if let Some(ref mut menu) = self.popup_menu {
                                menu.active_panel_mut().hover_index = -1;
                                if menu.move_hover(1) {
                                    self.frame_dirty = true;
                                }
                            }
                        }
                        Key::Named(NamedKey::End) => {
                            if let Some(ref mut menu) = self.popup_menu {
                                let len = menu.active_panel().item_indices.len() as i32;
                                menu.active_panel_mut().hover_index = len;
                                if menu.move_hover(-1) {
                                    self.frame_dirty = true;
                                }
                            }
                        }
                        _ => {} // Swallow other keys
                    }
                } else if self.ime_preedit_active {
                    // When IME preedit is active, suppress character
                    // keys to avoid double input.  The committed text
                    // will arrive via Ime::Commit instead.
                    tracing::debug!(
                        "IME preedit active, suppressing KeyboardInput: {:?}",
                        logical_key
                    );
                } else {
                    // On X11, some IME backends (e.g. fcitx5 with certain XIM
                    // styles) deliver committed text via KeyboardInput's `text`
                    // field instead of Ime::Commit.  Check `text` first for
                    // multi-char or non-ASCII content that translate_key would
                    // miss (e.g. CJK characters from shuangpin input).
                    let mut handled_via_text = false;
                    if state == ElementState::Pressed {
                        if let Some(ref txt) = text {
                            let s = txt.as_str();
                            // If text contains non-ASCII or multiple chars,
                            // it's likely IME-committed text
                            if !s.is_empty() && (s.len() > 1 || s.as_bytes()[0] > 0x7f) {
                                tracing::info!("KeyboardInput text field (IME fallback): '{}'", s);
                                for ch in s.chars() {
                                    let keysym = ch as u32;
                                    if keysym != 0 {
                                        self.comms.send_input(InputEvent::Key {
                                            keysym,
                                            modifiers: 0,
                                            pressed: true,
                                        });
                                    }
                                }
                                handled_via_text = true;
                            }
                        }
                    }
                    if !handled_via_text {
                        let mut keysym = Self::translate_key(&logical_key);
                        // When Ctrl is held, winit may transform keys (e.g. Ctrl+Space → NUL,
                        // Ctrl+letter → control char). Fall back to physical_key to recover
                        // the original unmodified key.
                        if keysym == 0 && self.modifiers != 0 {
                            use winit::keyboard::KeyCode;
                            use winit::keyboard::PhysicalKey;
                            keysym = match physical_key {
                                PhysicalKey::Code(KeyCode::Space) => 0x20,
                                _ => 0,
                            };
                        }
                        if keysym != 0 {
                            // Hide mouse cursor on keyboard input
                            if state == ElementState::Pressed && !self.mouse_hidden_for_typing {
                                if let Some(ref window) = self.window {
                                    window.set_cursor_visible(false);
                                    self.mouse_hidden_for_typing = true;
                                }
                            }
                            // Track key presses for typing speed indicator
                            if self.effects.typing_speed.enabled && state == ElementState::Pressed {
                                self.key_press_times.push(std::time::Instant::now());
                            }
                            // Track activity for idle dimming
                            if self.effects.idle_dim.enabled {
                                self.last_activity_time = std::time::Instant::now();
                            }
                            self.comms.send_input(InputEvent::Key {
                                keysym,
                                modifiers: self.modifiers,
                                pressed: state == ElementState::Pressed,
                            });
                        }
                    }
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
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
                // If popup menu is active, handle clicks for it
                if let Some(ref mut menu) = self.popup_menu {
                    if state == ElementState::Pressed && button == MouseButton::Left {
                        // Check if clicking on menu bar while popup is open (hover-to-switch)
                        if self.menu_bar_height > 0.0 && self.mouse_pos.1 < self.menu_bar_height {
                            if let Some(idx) =
                                self.menu_bar_hit_test(self.mouse_pos.0, self.mouse_pos.1)
                            {
                                // Close current popup and open new menu
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
                                // Regular item selected
                                self.comms
                                    .send_input(InputEvent::MenuSelection { index: idx });
                                self.popup_menu = None;
                                self.menu_bar_active = None;
                                self.frame_dirty = true;
                            } else {
                                // Check if click is on a submenu item (which hit_test returns -1 for)
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
                                        // Clicked a submenu item — keep menu open, submenu auto-opened on hover
                                        self.frame_dirty = true;
                                    } else {
                                        // Clicked outside or on a disabled item — cancel
                                        self.comms
                                            .send_input(InputEvent::MenuSelection { index: -1 });
                                        self.popup_menu = None;
                                        self.menu_bar_active = None;
                                        self.frame_dirty = true;
                                    }
                                } else {
                                    // Clicked outside all panels — cancel
                                    self.comms
                                        .send_input(InputEvent::MenuSelection { index: -1 });
                                    self.popup_menu = None;
                                    self.menu_bar_active = None;
                                    self.frame_dirty = true;
                                }
                            }
                        }
                    } else if state == ElementState::Pressed {
                        // Any other button cancels the menu
                        self.comms
                            .send_input(InputEvent::MenuSelection { index: -1 });
                        self.popup_menu = None;
                        self.menu_bar_active = None;
                        self.frame_dirty = true;
                    }
                } else if state == ElementState::Pressed
                    && button == MouseButton::Left
                    && self.chrome.resize_edge.is_some()
                {
                    // Borderless: initiate window resize drag
                    if let (Some(dir), Some(ref window)) =
                        (self.chrome.resize_edge, self.window.as_ref())
                    {
                        let _ = window.drag_resize_window(dir);
                    }
                } else if state == ElementState::Pressed
                    && button == MouseButton::Left
                    && self.titlebar_hit_test(self.mouse_pos.0, self.mouse_pos.1) > 0
                {
                    // Custom title bar click
                    match self.titlebar_hit_test(self.mouse_pos.0, self.mouse_pos.1) {
                        1 => {
                            // Drag area: double-click toggles maximize
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
                            // Close button (titlebar is only on primary window)
                            self.comms
                                .send_input(InputEvent::WindowClose { emacs_frame_id: 0 });
                        }
                        3 => {
                            // Maximize/restore toggle
                            if let Some(ref window) = self.window {
                                if window.is_maximized() {
                                    window.set_maximized(false);
                                } else {
                                    window.set_maximized(true);
                                }
                            }
                        }
                        4 => {
                            // Minimize
                            if let Some(ref window) = self.window {
                                window.set_minimized(true);
                            }
                        }
                        _ => {}
                    }
                } else if state == ElementState::Pressed
                    && button == MouseButton::Left
                    && !self.chrome.decorations_enabled
                    && (self.modifiers & NEOMACS_SUPER_MASK) != 0
                {
                    // Borderless: Super+click to drag-move window
                    if let Some(ref window) = self.window {
                        let _ = window.drag_window();
                    }
                } else if state == ElementState::Pressed
                    && button == MouseButton::Left
                    && self.menu_bar_height > 0.0
                    && self.mouse_pos.1 < self.menu_bar_height
                {
                    // Menu bar click — hit test menu bar items
                    tracing::debug!(
                        "Menu bar click at ({:.1}, {:.1}), menu_bar_height={}",
                        self.mouse_pos.0,
                        self.mouse_pos.1,
                        self.menu_bar_height
                    );
                    if let Some(idx) = self.menu_bar_hit_test(self.mouse_pos.0, self.mouse_pos.1) {
                        if self.menu_bar_active == Some(idx) {
                            // Clicking same item again: close
                            self.menu_bar_active = None;
                        } else {
                            self.menu_bar_active = Some(idx);
                            self.comms
                                .send_input(InputEvent::MenuBarClick { index: idx as i32 });
                        }
                        self.frame_dirty = true;
                    }
                } else if state == ElementState::Pressed
                    && button == MouseButton::Left
                    && self.toolbar_height > 0.0
                    && self.mouse_pos.1 < self.menu_bar_height + self.toolbar_height
                    && self.mouse_pos.1 >= self.menu_bar_height
                {
                    // Toolbar click — hit test toolbar items
                    if let Some(idx) = self.toolbar_hit_test(self.mouse_pos.0, self.mouse_pos.1) {
                        self.toolbar_pressed = Some(idx);
                        self.comms
                            .send_input(InputEvent::ToolBarClick { index: idx as i32 });
                        self.frame_dirty = true;
                    }
                } else if state == ElementState::Released
                    && button == MouseButton::Left
                    && self.toolbar_pressed.is_some()
                {
                    self.toolbar_pressed = None;
                    self.frame_dirty = true;
                } else {
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
                    // Hit test child frames
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
                    // WebKit glyph hit-test (search correct glyph buffer)
                    let (mut wk_id, mut wk_rx, mut wk_ry) = (0u32, 0i32, 0i32);
                    if state == ElementState::Pressed {
                        let glyphs: Option<&[FrameGlyph]> = if target_fid != 0 {
                            self.child_frames
                                .frames
                                .get(&target_fid)
                                .map(|e| e.frame.glyphs.as_slice())
                        } else {
                            self.current_frame.as_ref().map(|f| f.glyphs.as_slice())
                        };
                        if let Some(glyphs) = glyphs {
                            if let Some((id, rx, ry)) = webkit_glyph_hit_test(glyphs, ev_x, ev_y) {
                                wk_id = id;
                                wk_rx = rx;
                                wk_ry = ry;
                                tracing::trace!("WebKit hit: id={} rel=({},{})", id, rx, ry);
                            }
                        }
                        // Also check floating webkits (parent-frame absolute coords)
                        #[cfg(feature = "wpe-webkit")]
                        if wk_id == 0 && target_fid == 0 {
                            let mx = self.mouse_pos.0;
                            let my = self.mouse_pos.1;
                            for wk in self.floating_webkits.iter().rev() {
                                if mx >= wk.x
                                    && mx < wk.x + wk.width
                                    && my >= wk.y
                                    && my < wk.y + wk.height
                                {
                                    wk_id = wk.webkit_id;
                                    wk_rx = (mx - wk.x) as i32;
                                    wk_ry = (my - wk.y) as i32;
                                    break;
                                }
                            }
                        }
                    }
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
                    // Click halo effect on press
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
            }

            WindowEvent::CursorMoved { position, .. } => {
                // Convert to logical pixels for Emacs
                let lx = (position.x / self.scale_factor) as f32;
                let ly = (position.y / self.scale_factor) as f32;
                self.mouse_pos = (lx, ly);
                // Track activity for idle dimming
                if self.effects.idle_dim.enabled {
                    self.last_activity_time = std::time::Instant::now();
                }

                // Restore mouse cursor visibility when mouse moves
                if self.mouse_hidden_for_typing {
                    if let Some(ref window) = self.window {
                        window.set_cursor_visible(true);
                    }
                    self.mouse_hidden_for_typing = false;
                }

                // Borderless resize edge detection
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

                // Update title bar hover state and cursor
                if !self.chrome.decorations_enabled {
                    let new_hover = self.titlebar_hit_test(lx, ly);
                    if new_hover != self.chrome.titlebar_hover {
                        self.chrome.titlebar_hover = new_hover;
                        self.frame_dirty = true;
                        // Set cursor icon based on title bar region
                        if self.chrome.resize_edge.is_none() {
                            if let Some(ref window) = self.window {
                                use winit::window::CursorIcon;
                                let icon = match new_hover {
                                    2 | 3 | 4 => CursorIcon::Pointer, // buttons
                                    _ => CursorIcon::Default,
                                };
                                window.set_cursor(icon);
                            }
                        }
                    }
                }

                // Update menu bar hover state
                if self.menu_bar_height > 0.0 {
                    let old_hover = self.menu_bar_hovered;
                    if ly < self.menu_bar_height {
                        let new_hover = self.menu_bar_hit_test(lx, ly);
                        self.menu_bar_hovered = new_hover;
                        // Hover-to-switch: if a menu is active and hover moves to a different label
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

                // Update toolbar hover state
                if self.toolbar_height > 0.0 {
                    let old_hover = self.toolbar_hovered;
                    if ly < self.menu_bar_height + self.toolbar_height && ly >= self.menu_bar_height
                    {
                        self.toolbar_hovered = self.toolbar_hit_test(lx, ly);
                    } else {
                        self.toolbar_hovered = None;
                    }
                    if self.toolbar_hovered != old_hover {
                        self.frame_dirty = true;
                    }
                }

                // Update popup menu hover state (multi-panel)
                if let Some(ref mut menu) = self.popup_menu {
                    let (hit_depth, hit_local) = menu.hit_test_all(lx, ly);
                    if hit_depth >= 0 {
                        // Close deeper submenus if hovering on a shallower panel
                        let target_depth = hit_depth as usize;
                        while menu.submenu_panels.len() > target_depth {
                            menu.submenu_panels.pop();
                            self.frame_dirty = true;
                        }
                        // Update hover in the target panel
                        let panel = if target_depth == 0 {
                            &mut menu.root_panel
                        } else {
                            &mut menu.submenu_panels[target_depth - 1]
                        };
                        if hit_local != panel.hover_index {
                            panel.hover_index = hit_local;
                            self.frame_dirty = true;
                            // Auto-open submenu on hover
                            if hit_local >= 0 && (hit_local as usize) < panel.item_indices.len() {
                                let global_idx = panel.item_indices[hit_local as usize];
                                if menu.all_items[global_idx].submenu {
                                    menu.open_submenu();
                                }
                            }
                        }
                    }
                } else {
                    // Hit test child frames for mouse move
                    let (ev_x, ev_y, target_fid) =
                        if let Some((fid, local_x, local_y)) = self.child_frames.hit_test(lx, ly) {
                            (local_x, local_y, fid)
                        } else {
                            (lx, ly, 0)
                        };
                    self.comms.send_input(InputEvent::MouseMove {
                        x: ev_x,
                        y: ev_y,
                        modifiers: self.modifiers,
                        target_frame_id: target_fid,
                    });
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy, pixel_precise) = match delta {
                    winit::event::MouseScrollDelta::LineDelta(x, y) => (x, y, false),
                    winit::event::MouseScrollDelta::PixelDelta(pos) => {
                        // Pass raw logical pixel deltas for touchpad
                        (
                            (pos.x / self.scale_factor) as f32,
                            (pos.y / self.scale_factor) as f32,
                            true,
                        )
                    }
                };
                // Hit test child frames for scroll
                let (ev_x, ev_y, target_fid) = if let Some((fid, local_x, local_y)) = self
                    .child_frames
                    .hit_test(self.mouse_pos.0, self.mouse_pos.1)
                {
                    (local_x, local_y, fid)
                } else {
                    (self.mouse_pos.0, self.mouse_pos.1, 0)
                };
                // WebKit glyph hit-test for scroll
                let (mut wk_id, mut wk_rx, mut wk_ry) = (0u32, 0i32, 0i32);
                {
                    let glyphs: Option<&[FrameGlyph]> = if target_fid != 0 {
                        self.child_frames
                            .frames
                            .get(&target_fid)
                            .map(|e| e.frame.glyphs.as_slice())
                    } else {
                        self.current_frame.as_ref().map(|f| f.glyphs.as_slice())
                    };
                    if let Some(glyphs) = glyphs {
                        if let Some((id, rx, ry)) = webkit_glyph_hit_test(glyphs, ev_x, ev_y) {
                            wk_id = id;
                            wk_rx = rx;
                            wk_ry = ry;
                        }
                    }
                    // Also check floating webkits (parent-frame absolute coords)
                    #[cfg(feature = "wpe-webkit")]
                    if wk_id == 0 && target_fid == 0 {
                        let mx = self.mouse_pos.0;
                        let my = self.mouse_pos.1;
                        for wk in self.floating_webkits.iter().rev() {
                            if mx >= wk.x
                                && mx < wk.x + wk.width
                                && my >= wk.y
                                && my < wk.y + wk.height
                            {
                                wk_id = wk.webkit_id;
                                wk_rx = (mx - wk.x) as i32;
                                wk_ry = (my - wk.y) as i32;
                                break;
                            }
                        }
                    }
                }
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

            WindowEvent::RedrawRequested => {
                self.render();
                self.frame_dirty = false;
            }

            WindowEvent::ModifiersChanged(mods) => {
                let state = mods.state();
                self.modifiers = 0;
                if state.shift_key() {
                    self.modifiers |= NEOMACS_SHIFT_MASK;
                }
                if state.control_key() {
                    self.modifiers |= NEOMACS_CTRL_MASK;
                }
                if state.alt_key() {
                    self.modifiers |= NEOMACS_META_MASK;
                }
                if state.super_key() {
                    self.modifiers |= NEOMACS_SUPER_MASK;
                }
            }

            WindowEvent::Ime(ime_event) => {
                match ime_event {
                    winit::event::Ime::Enabled => {
                        self.ime_enabled = true;
                        self.last_ime_cursor_area = None;
                        if let Some(target) = self.cursor.target.clone() {
                            self.update_ime_cursor_area_if_needed(&target);
                        }
                        tracing::info!("IME enabled");
                    }
                    winit::event::Ime::Disabled => {
                        self.ime_enabled = false;
                        self.ime_preedit_active = false;
                        self.ime_preedit_text.clear();
                        self.last_ime_cursor_area = None;
                        tracing::info!("IME disabled");
                    }
                    winit::event::Ime::Commit(text) => {
                        tracing::debug!("IME Commit: '{}'", text);
                        self.ime_preedit_active = false;
                        self.ime_preedit_text.clear();
                        self.frame_dirty = true;
                        // Send each committed character as an individual
                        // key event to Emacs (no modifiers — IME already
                        // composed the final characters)
                        for ch in text.chars() {
                            let keysym = ch as u32;
                            if keysym != 0 {
                                self.comms.send_input(InputEvent::Key {
                                    keysym,
                                    modifiers: 0,
                                    pressed: true,
                                });
                            }
                        }
                    }
                    winit::event::Ime::Preedit(text, cursor_range) => {
                        tracing::debug!("IME Preedit: '{}' cursor: {:?}", text, cursor_range);
                        // Track whether preedit is active to suppress
                        // raw KeyboardInput during IME composition
                        self.ime_preedit_active = !text.is_empty();
                        self.ime_preedit_text = text.clone();

                        // Keep candidate window near the text cursor.
                        if let Some(target) = self.cursor.target.clone() {
                            self.update_ime_cursor_area_if_needed(&target);
                        }
                        self.frame_dirty = true;
                    }
                }
            }

            WindowEvent::DroppedFile(path) => {
                if let Some(path_str) = path.to_str() {
                    tracing::info!("File dropped: {}", path_str);
                    self.comms.send_input(InputEvent::FileDrop {
                        paths: vec![path_str.to_string()],
                        x: self.mouse_pos.0,
                        y: self.mouse_pos.1,
                    });
                }
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                tracing::info!(
                    "Scale factor changed: {} -> {}",
                    self.scale_factor,
                    scale_factor
                );
                self.scale_factor = scale_factor;
                // Update renderer's scale factor
                if let Some(ref mut renderer) = self.renderer {
                    renderer.set_scale_factor(scale_factor as f32);
                }
                // Clear glyph atlas so text re-rasterizes at new DPI
                if let Some(ref mut atlas) = self.glyph_atlas {
                    atlas.set_scale_factor(scale_factor as f32);
                }
                self.frame_dirty = true;
                // The Resized event will follow, which handles surface reconfiguration
            }

            _ => {}
        }
    }
}
