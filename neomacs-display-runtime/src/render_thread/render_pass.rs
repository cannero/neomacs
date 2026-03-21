use super::{RenderApp, surface_readback};
use crate::core::types::{AnimatedCursor, CursorAnimStyle};

impl RenderApp {
    pub(super) fn render(&mut self) {
        // Early return checks
        if self.current_frame.is_none()
            || self.surface.is_none()
            || self.renderer.is_none()
            || self.glyph_atlas.is_none()
        {
            return;
        }

        self.prepare_frame_state_for_render();

        // Get surface texture
        let Some(surface) = self.surface.as_ref() else {
            return;
        };
        let output = match surface.get_current_texture() {
            Ok(output) => output,
            Err(wgpu::SurfaceError::Lost) => {
                // Reconfigure surface
                let (w, h) = (self.width, self.height);
                self.handle_resize(w, h);
                return;
            }
            Err(wgpu::SurfaceError::OutOfMemory) => {
                tracing::error!("Out of GPU memory");
                return;
            }
            Err(e) => {
                tracing::warn!("Surface error: {:?}", e);
                return;
            }
        };

        let surface_view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // Build animated cursor override if applicable
        let animated_cursor =
            if let (true, Some(target)) = (self.cursor.anim_enabled, self.cursor.target.as_ref()) {
                let corners = if self.cursor.anim_style == CursorAnimStyle::CriticallyDampedSpring
                    && self.cursor.animating
                {
                    Some([
                        (
                            self.cursor.corner_springs[0].x,
                            self.cursor.corner_springs[0].y,
                        ),
                        (
                            self.cursor.corner_springs[1].x,
                            self.cursor.corner_springs[1].y,
                        ),
                        (
                            self.cursor.corner_springs[2].x,
                            self.cursor.corner_springs[2].y,
                        ),
                        (
                            self.cursor.corner_springs[3].x,
                            self.cursor.corner_springs[3].y,
                        ),
                    ])
                } else {
                    None
                };
                Some(AnimatedCursor {
                    window_id: target.window_id,
                    x: self.cursor.current_x,
                    y: self.cursor.current_y,
                    width: self.cursor.current_w,
                    height: self.cursor.current_h,
                    corners,
                    frame_id: target.frame_id,
                })
            } else {
                None
            };

        // Build background gradient option
        let bg_gradient = if self.effects.bg_gradient.enabled {
            Some((
                self.effects.bg_gradient.top,
                self.effects.bg_gradient.bottom,
            ))
        } else {
            None
        };

        // Check if we need offscreen rendering (for transitions)
        let need_offscreen = self.transitions.policy.needs_offscreen();

        if need_offscreen {
            // Swap: previous ← current
            self.transitions.current_is_a = !self.transitions.current_is_a;

            // Ensure offscreen textures exist
            self.ensure_offscreen_textures();

            // Render frame to current offscreen texture
            if let Some((current_view, _)) = self
                .current_offscreen_view_and_bg()
                .map(|(v, bg)| (v as *const wgpu::TextureView, bg))
            {
                let frame = self.current_frame.as_ref().expect("checked in render");
                let renderer = self.renderer.as_mut().expect("checked in render");
                let glyph_atlas = self.glyph_atlas.as_mut().expect("checked in render");
                renderer.set_idle_dim_alpha(self.idle_dim_current_alpha);

                // SAFETY: current_view is valid for the duration of this block
                renderer.render_frame_glyphs(
                    unsafe { &*current_view },
                    frame,
                    glyph_atlas,
                    &self.faces,
                    self.width,
                    self.height,
                    self.cursor.blink_on,
                    animated_cursor,
                    self.mouse_pos,
                    bg_gradient,
                );
            }

            // Detect transitions (compare window_infos)
            self.detect_transitions();

            // Blit current offscreen to surface
            if let Some((_, current_bg)) = self
                .current_offscreen_view_and_bg()
                .map(|(v, bg)| (v, bg as *const wgpu::BindGroup))
            {
                let renderer = self.renderer.as_ref().expect("checked in render");
                renderer.blit_texture_to_view(
                    unsafe { &*current_bg },
                    &surface_view,
                    self.width,
                    self.height,
                );
            }

            // Composite active transitions on top
            self.render_transitions(&surface_view);
        } else {
            // Simple path: render directly to surface
            let frame = self.current_frame.as_ref().expect("checked in render");
            let renderer = self.renderer.as_mut().expect("checked in render");
            let glyph_atlas = self.glyph_atlas.as_mut().expect("checked in render");
            renderer.set_idle_dim_alpha(self.idle_dim_current_alpha);

            renderer.render_frame_glyphs(
                &surface_view,
                frame,
                glyph_atlas,
                &self.faces,
                self.width,
                self.height,
                self.cursor.blink_on,
                animated_cursor,
                self.mouse_pos,
                bg_gradient,
            );
        }

        // Render child frames as floating overlays on top of the parent frame
        if !self.child_frames.is_empty() {
            for &child_id in self.child_frames.sorted_for_rendering() {
                if let Some(child_entry) = self.child_frames.frames.get(&child_id) {
                    if let (Some(renderer), Some(glyph_atlas)) =
                        (&self.renderer, &mut self.glyph_atlas)
                    {
                        // Pass animated cursor only if it belongs to this child frame
                        let child_anim = animated_cursor.filter(|ac| ac.frame_id == child_id);
                        renderer.render_child_frame(
                            &surface_view,
                            &child_entry.frame,
                            child_entry.abs_x,
                            child_entry.abs_y,
                            glyph_atlas,
                            &self.faces,
                            self.width,
                            self.height,
                            self.cursor.blink_on,
                            child_anim,
                            self.child_frame_corner_radius,
                            self.child_frame_shadow_enabled,
                            self.child_frame_shadow_layers,
                            self.child_frame_shadow_offset,
                            self.child_frame_shadow_opacity,
                        );
                    }
                }
            }
        }

        // Render breadcrumb/path bar overlay
        if self.effects.breadcrumb.enabled {
            if let (Some(renderer), Some(glyph_atlas), Some(frame)) = (
                &mut self.renderer,
                &mut self.glyph_atlas,
                &self.current_frame,
            ) {
                renderer.render_breadcrumbs(&surface_view, frame, glyph_atlas);
            }
        }

        // Render scroll position indicators and focus ring
        if self.scroll_indicators_enabled {
            if let (Some(renderer), Some(frame)) = (&self.renderer, &self.current_frame) {
                renderer.render_scroll_indicators(
                    &surface_view,
                    &frame.window_infos,
                    self.width,
                    self.height,
                );
            }
        }

        // Render window watermarks for empty/small buffers
        if self.effects.window_watermark.enabled {
            if let (Some(renderer), Some(glyph_atlas), Some(frame)) =
                (&self.renderer, &mut self.glyph_atlas, &self.current_frame)
            {
                renderer.render_window_watermarks(&surface_view, frame, glyph_atlas);
            }
        }

        // Render custom title bar when decorations are disabled (not in fullscreen)
        tracing::trace!(
            "CSD state: decorations_enabled={} is_fullscreen={} titlebar_height={}",
            self.chrome.decorations_enabled,
            self.chrome.is_fullscreen,
            self.chrome.titlebar_height
        );
        if !self.chrome.decorations_enabled
            && !self.chrome.is_fullscreen
            && self.chrome.titlebar_height > 0.0
        {
            if let (Some(renderer), Some(glyph_atlas)) = (&self.renderer, &mut self.glyph_atlas) {
                let frame_bg = self
                    .current_frame
                    .as_ref()
                    .map(|f| (f.background.r, f.background.g, f.background.b));
                renderer.render_custom_titlebar(
                    &surface_view,
                    &self.chrome.title,
                    self.chrome.titlebar_height,
                    self.chrome.titlebar_hover,
                    frame_bg,
                    glyph_atlas,
                    self.width,
                    self.height,
                );
            }
        }

        // Render floating WebKit overlays on top of everything
        #[cfg(feature = "wpe-webkit")]
        if !self.floating_webkits.is_empty() {
            if let Some(ref renderer) = self.renderer {
                renderer.render_floating_webkits(&surface_view, &self.floating_webkits);
            }
        }

        // Render menu bar overlay
        if self.menu_bar_height > 0.0 && !self.menu_bar_items.is_empty() {
            if let (Some(renderer), Some(glyph_atlas)) = (&self.renderer, &mut self.glyph_atlas) {
                renderer.render_menu_bar(
                    &surface_view,
                    &self.menu_bar_items,
                    self.menu_bar_height,
                    self.menu_bar_fg,
                    self.menu_bar_bg,
                    self.menu_bar_hovered,
                    self.menu_bar_active,
                    glyph_atlas,
                    self.width,
                    self.height,
                );
            }
        }

        // Tab bar is now rendered via the layout engine's status-line pipeline
        // (GlyphRowRole::TabBar) — no separate overlay needed.

        // Render toolbar overlay
        if self.toolbar_height > 0.0 && !self.toolbar_items.is_empty() {
            if let Some(ref renderer) = self.renderer {
                renderer.render_toolbar(
                    &surface_view,
                    &self.toolbar_items,
                    self.toolbar_height,
                    self.toolbar_fg,
                    self.toolbar_bg,
                    &self.toolbar_icon_textures,
                    self.toolbar_hovered,
                    self.toolbar_pressed,
                    self.toolbar_icon_size,
                    self.toolbar_padding,
                    self.width,
                    self.height,
                );
            }
        }

        // Render popup menu overlay (topmost layer)
        if let Some(ref menu) = self.popup_menu {
            if let (Some(renderer), Some(glyph_atlas)) = (&self.renderer, &mut self.glyph_atlas) {
                renderer.render_popup_menu(
                    &surface_view,
                    menu,
                    glyph_atlas,
                    self.width,
                    self.height,
                );
            }
        }

        // Render tooltip overlay (above everything including popup menu)
        if let Some(ref tip) = self.tooltip {
            if let (Some(renderer), Some(glyph_atlas)) = (&self.renderer, &mut self.glyph_atlas) {
                renderer.render_tooltip(&surface_view, tip, glyph_atlas, self.width, self.height);
            }
        }

        // Render IME preedit text overlay at cursor position
        if self.ime_preedit_active && !self.ime_preedit_text.is_empty() {
            if let (Some(renderer), Some(glyph_atlas), Some(target)) =
                (&self.renderer, &mut self.glyph_atlas, &self.cursor.target)
            {
                renderer.render_ime_preedit(
                    &surface_view,
                    &self.ime_preedit_text,
                    target.x,
                    target.y,
                    target.height,
                    glyph_atlas,
                    self.width,
                    self.height,
                );
            }
        }

        // Render visual bell flash overlay (above everything)
        if let Some(start) = self.visual_bell_start {
            let elapsed = start.elapsed().as_secs_f32();
            let duration = 0.15; // 150ms flash
            if elapsed < duration {
                let alpha = (1.0 - elapsed / duration) * 0.3; // max 30% opacity, fading out
                if let Some(ref renderer) = self.renderer {
                    renderer.render_visual_bell(&surface_view, self.width, self.height, alpha);
                }
                self.frame_dirty = true; // Keep redrawing during animation
            } else {
                self.visual_bell_start = None;
            }
        }

        // Render FPS counter overlay (topmost) with profiling stats
        if self.fps.enabled {
            // Measure frame time
            let frame_time = self.fps.render_start.elapsed().as_secs_f32() * 1000.0;
            // Exponential moving average (smooth over ~10 frames)
            self.fps.frame_time_ms = self.fps.frame_time_ms * 0.9 + frame_time * 0.1;

            // Gather stats
            let glyph_count = self
                .current_frame
                .as_ref()
                .map(|f| f.glyphs.len())
                .unwrap_or(0);
            let window_count = self
                .current_frame
                .as_ref()
                .map(|f| f.window_infos.len())
                .unwrap_or(0);
            let transition_count =
                self.transitions.crossfades.len() + self.transitions.scroll_slides.len();

            // Build multi-line stats text
            let stats_lines = vec![
                format!(
                    "{:.0} FPS | {:.1}ms",
                    self.fps.display_value, self.fps.frame_time_ms
                ),
                format!(
                    "{}g {}w {}t  {}x{}",
                    glyph_count, window_count, transition_count, self.width, self.height
                ),
            ];

            if let (Some(renderer), Some(glyph_atlas)) = (&self.renderer, &mut self.glyph_atlas) {
                renderer.render_fps_overlay(
                    &surface_view,
                    &stats_lines,
                    glyph_atlas,
                    self.width,
                    self.height,
                );
            }
        }

        // Render typing speed indicator
        if self.effects.typing_speed.enabled {
            let now = std::time::Instant::now();
            let window_secs = 5.0_f64;
            // Remove key presses older than the window
            self.key_press_times
                .retain(|t| now.duration_since(*t).as_secs_f64() < window_secs);
            // Calculate chars/second, then WPM (5 chars per word, * 60 for minutes)
            let count = self.key_press_times.len() as f64;
            let target_wpm = if count > 1.0 {
                let span = now.duration_since(self.key_press_times[0]).as_secs_f64();
                if span > 0.1 {
                    (count / span) * 60.0 / 5.0
                } else {
                    0.0
                }
            } else {
                0.0
            };
            // Exponential smoothing
            let alpha = 0.15_f32;
            self.displayed_wpm += (target_wpm as f32 - self.displayed_wpm) * alpha;
            if self.displayed_wpm < 0.5 {
                self.displayed_wpm = 0.0;
            }

            if let (Some(renderer), Some(glyph_atlas), Some(frame)) =
                (&self.renderer, &mut self.glyph_atlas, &self.current_frame)
            {
                renderer.render_typing_speed(&surface_view, frame, glyph_atlas, self.displayed_wpm);
            }
            // Keep redrawing while WPM is decaying
            if self.displayed_wpm > 0.5 || !self.key_press_times.is_empty() {
                self.frame_dirty = true;
            }
        }

        // Render corner mask for rounded window corners (borderless only, not fullscreen)
        if !self.chrome.decorations_enabled
            && !self.chrome.is_fullscreen
            && self.chrome.corner_radius > 0.0
        {
            if let Some(ref renderer) = self.renderer {
                renderer.render_corner_mask(
                    &surface_view,
                    self.chrome.corner_radius,
                    self.width,
                    self.height,
                );
            }
        }

        if let (Some(renderer), Some(frame)) = (&self.renderer, &self.current_frame) {
            surface_readback::maybe_log_first_frame_surface_readback(
                &mut self.debug_first_frame_readback_pending,
                &output.texture,
                renderer,
                frame,
                self.width,
                self.height,
            );
            surface_readback::maybe_log_debug_surface_readback(
                &mut self.debug_surface_readback_frames_remaining,
                &output.texture,
                renderer,
                frame,
                self.width,
                self.height,
            );
        }

        // Present the frame
        output.present();
    }
}
