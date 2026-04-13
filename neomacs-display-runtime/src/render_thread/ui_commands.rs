//! UI overlay, animation, and effect render commands.

use super::{PopupMenuState, RenderApp, TooltipState};
use crate::thread_comm::RenderCommand;

impl RenderApp {
    pub(super) fn handle_ui_command(&mut self, cmd: RenderCommand) -> Result<(), RenderCommand> {
        match cmd {
            RenderCommand::SetCursorBlink {
                enabled,
                interval_ms,
            } => {
                tracing::debug!(
                    "Cursor blink: enabled={}, interval={}ms",
                    enabled,
                    interval_ms
                );
                self.cursor.blink_enabled = enabled;
                self.cursor.blink_interval = std::time::Duration::from_millis(interval_ms as u64);
                if !enabled {
                    self.cursor.blink_on = true;
                    self.frame_dirty = true;
                }
                Ok(())
            }
            RenderCommand::SetCursorAnimation { enabled, speed } => {
                tracing::debug!("Cursor animation: enabled={}, speed={}", enabled, speed);
                self.cursor.anim_enabled = enabled;
                self.cursor.anim_speed = speed;
                if !enabled {
                    self.cursor.animating = false;
                }
                Ok(())
            }
            RenderCommand::SetAnimationConfig {
                cursor_enabled,
                cursor_speed,
                cursor_style,
                cursor_duration_ms,
                transition_policy,
                trail_size,
            } => {
                tracing::debug!(
                    "Animation config: cursor={}/{}/style={:?}/{}ms/trail={}, crossfade={}/{}ms/effect={:?}/easing={:?}, scroll={}/{}ms/effect={:?}/easing={:?}",
                    cursor_enabled,
                    cursor_speed,
                    cursor_style,
                    cursor_duration_ms,
                    trail_size,
                    transition_policy.crossfade_enabled,
                    transition_policy.crossfade_duration_ms,
                    transition_policy.crossfade_effect,
                    transition_policy.crossfade_easing,
                    transition_policy.scroll_enabled,
                    transition_policy.scroll_duration_ms,
                    transition_policy.scroll_effect,
                    transition_policy.scroll_easing
                );
                self.cursor.anim_enabled = cursor_enabled;
                self.cursor.anim_speed = cursor_speed;
                self.cursor.anim_style = cursor_style;
                self.cursor.anim_duration = cursor_duration_ms as f32 / 1000.0;
                self.cursor.trail_size = trail_size.clamp(0.0, 1.0);
                self.transitions.policy = transition_policy;
                if !cursor_enabled {
                    self.cursor.animating = false;
                }
                if !self.transitions.policy.crossfade_enabled {
                    self.transitions.crossfades.clear();
                }
                if !self.transitions.policy.scroll_enabled {
                    self.transitions.scroll_slides.clear();
                }
                Ok(())
            }
            RenderCommand::ShowPopupMenu {
                x,
                y,
                items,
                title,
                fg,
                bg,
            } => {
                tracing::info!("ShowPopupMenu at ({}, {}) with {} items", x, y, items.len());
                let (fs, lh, cw) = self
                    .glyph_atlas
                    .as_ref()
                    .map(|a| {
                        (
                            a.default_font_size(),
                            a.default_line_height(),
                            a.default_char_width(),
                        )
                    })
                    .unwrap_or((13.0, 17.0, 13.0 * 0.6));
                let mut menu = PopupMenuState::new(x, y, items, title, fs, lh, cw);
                menu.face_fg = fg;
                menu.face_bg = bg;
                self.popup_menu = Some(menu);
                self.frame_dirty = true;
                Ok(())
            }
            RenderCommand::HidePopupMenu => {
                tracing::info!("HidePopupMenu");
                self.popup_menu = None;
                self.menu_bar_active = None;
                self.frame_dirty = true;
                Ok(())
            }
            RenderCommand::ShowTooltip {
                x,
                y,
                text,
                fg_r,
                fg_g,
                fg_b,
                bg_r,
                bg_g,
                bg_b,
            } => {
                tracing::debug!("ShowTooltip at ({}, {})", x, y);
                let (fs, lh, cw) = self
                    .glyph_atlas
                    .as_ref()
                    .map(|a| {
                        (
                            a.default_font_size(),
                            a.default_line_height(),
                            a.default_char_width(),
                        )
                    })
                    .unwrap_or((13.0, 17.0, 13.0 * 0.6));
                self.tooltip = Some(TooltipState::new(
                    x,
                    y,
                    &text,
                    (fg_r, fg_g, fg_b),
                    (bg_r, bg_g, bg_b),
                    self.width as f32 / self.scale_factor as f32,
                    self.height as f32 / self.scale_factor as f32,
                    fs,
                    lh,
                    cw,
                ));
                self.frame_dirty = true;
                Ok(())
            }
            RenderCommand::HideTooltip => {
                tracing::debug!("HideTooltip");
                self.tooltip = None;
                self.frame_dirty = true;
                Ok(())
            }
            RenderCommand::VisualBell => {
                self.visual_bell_start = Some(std::time::Instant::now());
                if self.effects.cursor_error_pulse.enabled {
                    if let Some(renderer) = self.renderer.as_mut() {
                        renderer.trigger_cursor_error_pulse(std::time::Instant::now());
                    }
                }
                if self.effects.edge_snap.enabled {
                    if let Some(ref frame) = self.current_frame {
                        for info in &frame.window_infos {
                            if info.selected && !info.is_minibuffer {
                                let at_top = info.window_start <= 1;
                                let at_bottom = info.window_end >= info.buffer_size;
                                if at_top || at_bottom {
                                    if let Some(renderer) = self.renderer.as_mut() {
                                        renderer.trigger_edge_snap(
                                            info.bounds,
                                            info.mode_line_height,
                                            at_top,
                                            at_bottom,
                                            std::time::Instant::now(),
                                        );
                                    }
                                }
                                break;
                            }
                        }
                    }
                }
                self.frame_dirty = true;
                Ok(())
            }
            RenderCommand::UpdateEffect(updater) => {
                (updater.0)(&mut self.effects);
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.effects = self.effects.clone();
                }
                self.frame_dirty = true;
                Ok(())
            }
            RenderCommand::SetScrollIndicators { enabled } => {
                self.scroll_indicators_enabled = enabled;
                self.frame_dirty = true;
                Ok(())
            }
            RenderCommand::SetTitlebarHeight { height } => {
                self.chrome.titlebar_height = height;
                self.frame_dirty = true;
                Ok(())
            }
            RenderCommand::SetShowFps { enabled } => {
                self.fps.enabled = enabled;
                self.frame_dirty = true;
                Ok(())
            }
            RenderCommand::SetCornerRadius { radius } => {
                self.chrome.corner_radius = radius;
                self.frame_dirty = true;
                Ok(())
            }
            RenderCommand::SetExtraSpacing {
                line_spacing,
                letter_spacing,
            } => {
                self.extra_line_spacing = line_spacing;
                self.extra_letter_spacing = letter_spacing;
                self.frame_dirty = true;
                Ok(())
            }
            RenderCommand::SetIndentGuideRainbow { enabled, colors } => {
                let linear_colors: Vec<(f32, f32, f32, f32)> = colors
                    .iter()
                    .map(|(r, g, b, a)| {
                        let c = crate::core::types::Color::new(*r, *g, *b, *a).srgb_to_linear();
                        (c.r, c.g, c.b, c.a)
                    })
                    .collect();
                self.effects.indent_guides.rainbow_enabled = enabled;
                self.effects.indent_guides.rainbow_colors = linear_colors.clone();
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.set_indent_guide_rainbow(enabled, linear_colors);
                }
                self.frame_dirty = true;
                Ok(())
            }
            RenderCommand::SetCursorSizeTransition {
                enabled,
                duration_ms,
            } => {
                self.cursor.size_transition_enabled = enabled;
                self.cursor.size_transition_duration = duration_ms as f32 / 1000.0;
                if !enabled {
                    self.cursor.size_animating = false;
                }
                self.frame_dirty = true;
                Ok(())
            }
            RenderCommand::SetLigaturesEnabled { enabled } => {
                tracing::info!("Ligatures enabled: {}", enabled);
                Ok(())
            }
            RenderCommand::RemoveChildFrame { frame_id } => {
                tracing::info!("Removing child frame 0x{:x}", frame_id);
                self.child_frames.remove_frame(frame_id);
                self.frame_dirty = true;
                Ok(())
            }
            RenderCommand::SetChildFrameStyle {
                corner_radius,
                shadow_enabled,
                shadow_layers,
                shadow_offset,
                shadow_opacity,
            } => {
                self.child_frame_corner_radius = corner_radius;
                self.child_frame_shadow_enabled = shadow_enabled;
                self.child_frame_shadow_layers = shadow_layers;
                self.child_frame_shadow_offset = shadow_offset;
                self.child_frame_shadow_opacity = shadow_opacity;
                self.frame_dirty = true;
                Ok(())
            }
            RenderCommand::SetToolBar {
                items,
                height,
                fg_r,
                fg_g,
                fg_b,
                bg_r,
                bg_g,
                bg_b,
            } => {
                for item in &items {
                    if !item.is_separator
                        && !item.icon_name.is_empty()
                        && !self.toolbar_icon_textures.contains_key(&item.icon_name)
                    {
                        if let Some(svg_data) =
                            crate::backend::wgpu::toolbar_icons::get_icon_svg(&item.icon_name)
                        {
                            if let Some(renderer) = self.renderer.as_mut() {
                                let icon_size = self.toolbar_icon_size;
                                let id =
                                    renderer.load_image_data(svg_data, icon_size, icon_size, 0, 0);
                                self.toolbar_icon_textures
                                    .insert(item.icon_name.clone(), id);
                                tracing::debug!(
                                    "Loaded toolbar icon '{}' as image_id={}",
                                    item.icon_name,
                                    id
                                );
                            }
                        }
                    }
                }
                self.toolbar_items = items;
                self.toolbar_height = height;
                self.toolbar_fg = (fg_r, fg_g, fg_b);
                self.toolbar_bg = (bg_r, bg_g, bg_b);
                self.frame_dirty = true;
                Ok(())
            }
            RenderCommand::SetToolBarConfig { icon_size, padding } => {
                self.toolbar_icon_size = icon_size;
                self.toolbar_padding = padding;
                for (_name, id) in self.toolbar_icon_textures.drain() {
                    if let Some(renderer) = self.renderer.as_mut() {
                        renderer.free_image(id);
                    }
                }
                self.frame_dirty = true;
                Ok(())
            }
            RenderCommand::SetMenuBar {
                items,
                height,
                fg_r,
                fg_g,
                fg_b,
                bg_r,
                bg_g,
                bg_b,
            } => {
                tracing::debug!(
                    "SetMenuBar: {} items, height={}, fg=({:.3},{:.3},{:.3}), bg=({:.3},{:.3},{:.3})",
                    items.len(),
                    height,
                    fg_r,
                    fg_g,
                    fg_b,
                    bg_r,
                    bg_g,
                    bg_b
                );
                self.menu_bar_items = items;
                self.menu_bar_height = height;
                self.menu_bar_fg = (fg_r, fg_g, fg_b);
                self.menu_bar_bg = (bg_r, bg_g, bg_b);
                self.frame_dirty = true;
                Ok(())
            }
            RenderCommand::SetTabBar { items, height } => {
                tracing::debug!("SetTabBar: {} items, height={}", items.len(), height);
                self.tab_bar_items = items;
                self.tab_bar_y = self.menu_bar_height;
                self.tab_bar_height = height;
                self.frame_dirty = true;
                Ok(())
            }
            other => Err(other),
        }
    }
}
