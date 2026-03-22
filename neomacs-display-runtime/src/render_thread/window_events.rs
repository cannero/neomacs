use super::RenderApp;
use super::state::{effective_window_scale_factor, emacs_pixels_from_window_size};
use crate::backend::wgpu::{
    NEOMACS_CTRL_MASK, NEOMACS_META_MASK, NEOMACS_SHIFT_MASK, NEOMACS_SUPER_MASK,
};
use crate::thread_comm::InputEvent;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::WindowId;

impl RenderApp {
    pub(super) fn handle_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                tracing::info!("Window close requested");
                let emacs_fid = self
                    .multi_windows
                    .emacs_frame_for_winit(window_id)
                    .unwrap_or(0);
                self.comms.send_input(InputEvent::WindowClose {
                    emacs_frame_id: emacs_fid,
                });
                if emacs_fid == 0 {
                    event_loop.exit();
                } else {
                    self.multi_windows.request_destroy(emacs_fid);
                }
            }

            WindowEvent::Resized(size) => {
                tracing::info!("WindowEvent::Resized: {}x{}", size.width, size.height);

                let emacs_fid = self
                    .multi_windows
                    .emacs_frame_for_winit(window_id)
                    .unwrap_or(0);
                if emacs_fid == 0 {
                    self.handle_resize(size.width, size.height);
                    let (emacs_w, emacs_h) =
                        emacs_pixels_from_window_size(size.width, size.height, self.scale_factor);
                    tracing::info!(
                        "Sending WindowResize event to Emacs: {}x{}",
                        emacs_w,
                        emacs_h
                    );
                    self.comms.send_input(InputEvent::WindowResize {
                        width: emacs_w,
                        height: emacs_h,
                        emacs_frame_id: 0,
                    });
                } else if let Some(device) = self.device.clone() {
                    if let Some(ws) = self.multi_windows.get_mut(emacs_fid) {
                        ws.handle_resize(&device, size.width, size.height);
                        let (emacs_w, emacs_h) =
                            emacs_pixels_from_window_size(size.width, size.height, ws.scale_factor);
                        self.comms.send_input(InputEvent::WindowResize {
                            width: emacs_w,
                            height: emacs_h,
                            emacs_frame_id: emacs_fid,
                        });
                    }
                }
            }

            WindowEvent::Focused(focused) => {
                let emacs_fid = self
                    .multi_windows
                    .emacs_frame_for_winit(window_id)
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
                        _ => {}
                    }
                } else if self.ime_preedit_active {
                    tracing::debug!(
                        "IME preedit active, suppressing KeyboardInput: {:?}",
                        logical_key
                    );
                } else {
                    let mut handled_via_text = false;
                    if state == ElementState::Pressed {
                        if let Some(ref txt) = text {
                            let s = txt.as_str();
                            if let Some(control_keysym) = Self::translate_control_text(s) {
                                tracing::debug!(
                                    "KeyboardInput control text path: text={:?} keysym=0x{:04x} mods=0x{:x}",
                                    s,
                                    control_keysym,
                                    self.modifiers
                                );
                                self.comms.send_input(InputEvent::Key {
                                    keysym: control_keysym,
                                    modifiers: self.modifiers,
                                    pressed: true,
                                });
                                handled_via_text = true;
                            } else if let Some(keysyms) =
                                Self::translate_committed_text(s, self.modifiers)
                            {
                                tracing::debug!(
                                    "KeyboardInput committed text path: text={:?} keysyms={:?} mods=0x{:x}",
                                    s,
                                    keysyms,
                                    self.modifiers
                                );
                                for keysym in keysyms {
                                    tracing::debug!(
                                        "Queueing text key event: keysym=0x{:04x} mods=0x{:x}",
                                        keysym,
                                        self.modifiers
                                    );
                                    self.comms.send_input(InputEvent::Key {
                                        keysym,
                                        modifiers: self.modifiers,
                                        pressed: true,
                                    });
                                }
                                handled_via_text = true;
                            }
                        }
                    }
                    if !handled_via_text {
                        let mut keysym = Self::translate_key(&logical_key);
                        if keysym == 0 && self.modifiers != 0 {
                            use winit::keyboard::KeyCode;
                            use winit::keyboard::PhysicalKey;
                            keysym = match physical_key {
                                PhysicalKey::Code(KeyCode::Space) => 0x20,
                                _ => 0,
                            };
                        }
                        if keysym != 0 {
                            tracing::debug!(
                                "KeyboardInput translated path: logical_key={:?} physical_key={:?} keysym=0x{:04x} mods=0x{:x} pressed={}",
                                logical_key,
                                physical_key,
                                keysym,
                                self.modifiers,
                                state == ElementState::Pressed
                            );
                            if state == ElementState::Pressed && !self.mouse_hidden_for_typing {
                                if let Some(ref window) = self.window {
                                    window.set_cursor_visible(false);
                                    self.mouse_hidden_for_typing = true;
                                }
                            }
                            if self.effects.typing_speed.enabled && state == ElementState::Pressed {
                                self.key_press_times.push(std::time::Instant::now());
                            }
                            if self.effects.idle_dim.enabled {
                                self.last_activity_time = std::time::Instant::now();
                            }
                            self.comms.send_input(InputEvent::Key {
                                keysym,
                                modifiers: self.modifiers,
                                pressed: state == ElementState::Pressed,
                            });
                        } else if state == ElementState::Pressed {
                            tracing::debug!(
                                "KeyboardInput dropped after translation: logical_key={:?} physical_key={:?} text={:?} mods=0x{:x}",
                                logical_key,
                                physical_key,
                                text,
                                self.modifiers
                            );
                        }
                    }
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
                self.handle_mouse_input(state, button);
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position);
            }

            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(delta);
            }

            WindowEvent::RedrawRequested => {
                self.render();
                self.frame_dirty = false;
            }

            WindowEvent::ModifiersChanged(mods) => {
                let old_modifiers = self.modifiers;
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
                tracing::debug!(
                    "ModifiersChanged: old=0x{:x} new=0x{:x} shift={} ctrl={} alt={} super={}",
                    old_modifiers,
                    self.modifiers,
                    state.shift_key(),
                    state.control_key(),
                    state.alt_key(),
                    state.super_key()
                );
            }

            WindowEvent::Ime(ime_event) => match ime_event {
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
                    self.ime_preedit_active = !text.is_empty();
                    self.ime_preedit_text = text.clone();

                    if let Some(target) = self.cursor.target.clone() {
                        self.update_ime_cursor_area_if_needed(&target);
                    }
                    self.frame_dirty = true;
                }
            },

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
                let effective_scale = effective_window_scale_factor(scale_factor);
                tracing::info!(
                    "Scale factor changed: previous_effective={} raw={} effective={}",
                    self.scale_factor,
                    scale_factor,
                    effective_scale
                );
                self.scale_factor = effective_scale;
                if let Some(ref mut renderer) = self.renderer {
                    renderer.set_scale_factor(effective_scale as f32);
                }
                if let Some(ref mut atlas) = self.glyph_atlas {
                    atlas.set_scale_factor(effective_scale as f32);
                }
                self.frame_dirty = true;
            }

            _ => {}
        }
    }
}
