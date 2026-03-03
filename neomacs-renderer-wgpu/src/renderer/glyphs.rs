//! Glyphs methods for WgpuRenderer.

use super::super::glyph_atlas::{CachedGlyph, ComposedGlyphKey, GlyphKey, WgpuGlyphAtlas};
use super::super::vertex::{GlyphVertex, RectVertex, RoundedRectVertex, Uniforms};
use super::WgpuRenderer;
use super::{
    ClickHaloEntry, CursorGhostEntry, CursorParticle, EdgeGlowEntry, EdgeSnapEntry, HeatMapEntry,
    LineAnimEntry, MatrixColumn, ModeLineFadeEntry, RainDrop, RippleWaveEntry, ScrollMomentumEntry,
    ScrollSpacingEntry, ScrollVelocityFadeEntry, SonarPingEntry, SparkleBurstEntry, TextFadeEntry,
    TitleFadeEntry, WindowFadeEntry,
};
use cosmic_text::SubpixelBin;
use neomacs_display_protocol::face::{BoxType, Face, FaceAttributes};
use neomacs_display_protocol::frame_glyphs::{
    CursorStyle, FrameGlyph, FrameGlyphBuffer, StipplePattern,
};
use neomacs_display_protocol::types::{AnimatedCursor, Color, Rect};
use std::collections::HashMap;
use wgpu::util::DeviceExt;

/// Draw effect vertices produced by a pure effect function.
macro_rules! draw_effect {
    ($self:ident, $rp:ident, $label:expr, $verts:expr) => {{
        let verts = $verts;
        if !verts.is_empty() {
            let buf = $self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some($label),
                    contents: bytemuck::cast_slice(&verts),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            $rp.set_pipeline(&$self.rect_pipeline);
            $rp.set_bind_group(0, &$self.uniform_bind_group, &[]);
            $rp.set_vertex_buffer(0, buf.slice(..));
            $rp.draw(0..verts.len() as u32, 0..1);
        }
    }};
    // Animated/time-based effects: sets needs_continuous_redraw only when effect is active
    ($self:ident, $rp:ident, $label:expr, $verts:expr, continuous) => {{
        let verts = $verts;
        if !verts.is_empty() {
            $self.needs_continuous_redraw = true;
            let buf = $self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some($label),
                    contents: bytemuck::cast_slice(&verts),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            $rp.set_pipeline(&$self.rect_pipeline);
            $rp.set_bind_group(0, &$self.uniform_bind_group, &[]);
            $rp.set_vertex_buffer(0, buf.slice(..));
            $rp.draw(0..verts.len() as u32, 0..1);
        }
    }};
}

/// Draw effect vertices from a stateful effect function that returns (Vec<RectVertex>, bool).
macro_rules! draw_stateful {
    ($self:ident, $rp:ident, $label:expr, $result:expr) => {{
        let (verts, needs_redraw) = $result;
        if needs_redraw {
            $self.needs_continuous_redraw = true;
        }
        if !verts.is_empty() {
            let buf = $self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some($label),
                    contents: bytemuck::cast_slice(&verts),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            $rp.set_pipeline(&$self.rect_pipeline);
            $rp.set_bind_group(0, &$self.uniform_bind_group, &[]);
            $rp.set_vertex_buffer(0, buf.slice(..));
            $rp.draw(0..verts.len() as u32, 0..1);
        }
    }};
}

struct BoxSpan {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    face_id: u32,
    is_overlay: bool,
    bg: Option<Color>,
}

impl WgpuRenderer {
    /// Render frame glyphs to a texture view
    ///
    /// `surface_width` and `surface_height` should be the actual surface dimensions
    /// for correct coordinate transformation.
    pub fn render_frame_glyphs(
        &mut self,
        view: &wgpu::TextureView,
        frame_glyphs: &FrameGlyphBuffer,
        glyph_atlas: &mut WgpuGlyphAtlas,
        faces: &HashMap<u32, Face>,
        surface_width: u32,
        surface_height: u32,
        cursor_visible: bool,
        animated_cursor: Option<AnimatedCursor>,
        mouse_pos: (f32, f32),
        background_gradient: Option<((f32, f32, f32), (f32, f32, f32))>,
    ) {
        tracing::debug!(
            "render_frame_glyphs: frame={}x{} surface={}x{}, {} glyphs, {} faces",
            frame_glyphs.width,
            frame_glyphs.height,
            surface_width,
            surface_height,
            frame_glyphs.glyphs.len(),
            faces.len(),
        );

        // Reset continuous redraw flag (will be set by dim fade or other animations)
        self.needs_continuous_redraw = false;
        // Reset animated borders flag (set during box rendering if any fancy style is used)
        self.has_animated_borders = false;

        // Clean up expired line animations
        self.active_line_anims
            .retain(|a| a.started.elapsed() < a.duration);
        if !self.active_line_anims.is_empty() {
            self.needs_continuous_redraw = true;
        }

        // Clean up expired mode-line transition fades
        self.active_mode_line_fades
            .retain(|e| e.started.elapsed() < e.duration);
        if !self.active_mode_line_fades.is_empty() {
            self.needs_continuous_redraw = true;
        }

        // Detect mode-line content changes and trigger transitions
        if self.effects.mode_line_transition.enabled {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let now_ml = std::time::Instant::now();
            for info in &frame_glyphs.window_infos {
                if info.mode_line_height < 1.0 || info.is_minibuffer {
                    continue;
                }
                let ml_y = info.bounds.y + info.bounds.height - info.mode_line_height;
                // Hash overlay chars within mode-line area
                let mut hasher = DefaultHasher::new();
                for g in &frame_glyphs.glyphs {
                    if let FrameGlyph::Char {
                        x,
                        y,
                        char: ch,
                        is_overlay: true,
                        ..
                    } = g
                    {
                        if *x >= info.bounds.x
                            && *x < info.bounds.x + info.bounds.width
                            && *y >= ml_y
                            && *y < ml_y + info.mode_line_height
                        {
                            ch.hash(&mut hasher);
                        }
                    }
                }
                let hash = hasher.finish();
                let prev = self.prev_mode_line_hashes.insert(info.window_id, hash);
                if let Some(prev_hash) = prev {
                    if prev_hash != hash {
                        self.active_mode_line_fades
                            .retain(|e| e.window_id != info.window_id);
                        self.active_mode_line_fades.push(ModeLineFadeEntry {
                            window_id: info.window_id,
                            mode_line_y: ml_y,
                            mode_line_h: info.mode_line_height,
                            bounds_x: info.bounds.x,
                            bounds_w: info.bounds.width,
                            started: now_ml,
                            duration: std::time::Duration::from_millis(
                                self.effects.mode_line_transition.duration_ms as u64,
                            ),
                        });
                        self.needs_continuous_redraw = true;
                    }
                }
            }
        }

        // Clean up expired text fade-in animations
        self.active_text_fades
            .retain(|e| e.started.elapsed() < e.duration);
        if !self.active_text_fades.is_empty() {
            self.needs_continuous_redraw = true;
        }

        // Clean up expired scroll line spacing animations
        let now_spacing = std::time::Instant::now();
        self.active_scroll_spacings
            .retain(|e| now_spacing.duration_since(e.started) < e.duration);
        if !self.active_scroll_spacings.is_empty() {
            self.needs_continuous_redraw = true;
        }

        // Clear expired cursor wake animation
        if let Some(started) = self.cursor_wake_started {
            let dur = std::time::Duration::from_millis(self.effects.cursor_wake.duration_ms as u64);
            if started.elapsed() >= dur {
                self.cursor_wake_started = None;
            } else {
                self.needs_continuous_redraw = true;
            }
        }

        // Clear expired cursor error pulse
        if let Some(started) = self.cursor_error_pulse_started {
            let dur = std::time::Duration::from_millis(
                self.effects.cursor_error_pulse.duration_ms as u64,
            );
            if started.elapsed() >= dur {
                self.cursor_error_pulse_started = None;
            } else {
                self.needs_continuous_redraw = true;
            }
        }

        // Clean up expired scroll momentum entries
        self.active_scroll_momentums
            .retain(|e| e.started.elapsed() < e.duration);
        if !self.active_scroll_momentums.is_empty() {
            self.needs_continuous_redraw = true;
        }

        // Advance glyph atlas generation for LRU tracking
        glyph_atlas.advance_generation();

        // Use the frame's own logical dimensions for coordinate transformation.
        // Emacs may round up the frame size to char grid boundaries, so the frame
        // can be slightly larger than the window surface. Using the frame dimensions
        // ensures glyph positions (which are relative to the frame) map correctly.
        let logical_w = if frame_glyphs.width > 0.0 {
            frame_glyphs.width
        } else {
            surface_width as f32 / self.scale_factor
        };
        let logical_h = if frame_glyphs.height > 0.0 {
            frame_glyphs.height
        } else {
            surface_height as f32 / self.scale_factor
        };
        let elapsed = self.render_start_time.elapsed().as_secs_f32();
        let uniforms = Uniforms {
            screen_size: [logical_w, logical_h],
            time: elapsed,
            _padding: 0.0,
        };
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::cast_slice(&[uniforms]));

        // Fast path: when visual effects are fully disabled and no transient
        // effect state is active, use the shared content renderer (same path
        // used by child frames) to avoid maintaining duplicate text/media
        // drawing logic in two places.
        let use_shared_content_path = background_gradient.is_none()
            && self.effects == neomacs_display_protocol::EffectsConfig::default()
            && self.active_line_anims.is_empty()
            && self.active_mode_line_fades.is_empty()
            && self.active_text_fades.is_empty()
            && self.active_scroll_spacings.is_empty()
            && self.cursor_wake_started.is_none()
            && self.cursor_error_pulse_started.is_none()
            && self.active_scroll_momentums.is_empty()
            && self.active_window_fades.is_empty();

        if use_shared_content_path {
            tracing::debug!("render_frame_glyphs: using shared content path (effects disabled)");
            let bg = &frame_glyphs.background;
            let mut clear_encoder =
                self.device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("Frame Glyphs Clear Encoder"),
                    });
            {
                let _clear_pass = clear_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Frame Glyphs Clear Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                // Pre-multiply RGB by alpha for correct compositing.
                                r: (bg.r * bg.a) as f64,
                                g: (bg.g * bg.a) as f64,
                                b: (bg.b * bg.a) as f64,
                                a: bg.a as f64,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            }
            self.queue.submit(std::iter::once(clear_encoder.finish()));

            self.render_frame_content(
                view,
                frame_glyphs,
                glyph_atlas,
                faces,
                surface_width,
                surface_height,
                0.0,
                0.0,
                cursor_visible,
                animated_cursor,
            );
            return;
        }

        // Rendering order for correct z-layering (inverse video cursor):
        //   1. Non-overlay backgrounds (window bg, stretches, char bg)
        //   2. Cursor bg rect (inverse video background for filled box cursor)
        //   3. Animated cursor trail (behind text, for filled box cursor motion)
        //   4. Non-overlay text (with cursor_fg swap for char at cursor position)
        //   5. Overlay backgrounds (mode-line/echo bg)
        //   6. Overlay text (mode-line/echo text)
        //   7. Inline media (images, videos, webkits)
        //   8. Front cursors (bar, hbar, hollow) and borders
        //
        // Filled box cursor (style 0) is split across steps 2-4 for inverse video.
        // Bar/hbar/hollow cursors are drawn on top of text in step 8.

        // Build per-window text-area clip bottoms from window_infos.
        // Each window's text area ends at: bounds.y + bounds.height - mode_line_height.
        // Glyphs belonging to a window (by Y position) are clipped to that boundary.
        let clip_regions: Vec<(f32, f32, f32)> = frame_glyphs
            .window_infos
            .iter()
            .filter(|info| !info.is_minibuffer)
            .map(|info| {
                let y_start = info.bounds.y;
                let y_end = info.bounds.y + info.bounds.height;
                let text_bottom = y_end - info.mode_line_height;
                (y_start, y_end, text_bottom)
            })
            .collect();

        // Find the text-area clip bottom for a glyph at position y.
        // Returns the text_bottom of the window whose bounds contain y,
        // or f32::MAX if no window matches (no clipping).
        let clip_bottom_for = |y: f32| -> f32 {
            for &(y_start, y_end, text_bottom) in &clip_regions {
                if y >= y_start && y < y_end {
                    return text_bottom;
                }
            }
            f32::MAX
        };

        // Debug: scan for any FrameGlyph entries near y≈27 (the gray line area)
        {
            let mut logged_count = 0;
            for (i, glyph) in frame_glyphs.glyphs.iter().enumerate() {
                if logged_count > 20 {
                    break;
                }
                match glyph {
                    FrameGlyph::Char {
                        x,
                        y,
                        width,
                        height,
                        ascent,
                        fg,
                        face_id,
                        font_size,
                        bg,
                        char: ch,
                        is_overlay,
                        ..
                    } => {
                        // Log first row chars AND any char touching y=24-32
                        if *y < 1.0 || (*y < 32.0 && *y + *height > 24.0) {
                            let bg_str = bg
                                .as_ref()
                                .map(|c| format!("({:.3},{:.3},{:.3})", c.r, c.g, c.b))
                                .unwrap_or("None".to_string());
                            tracing::debug!(
                                "frame_glyph[{}]: Char '{}' face={} pos=({:.1},{:.1}) size=({:.1},{:.1}) ascent={:.1} fg=({:.3},{:.3},{:.3}) bg={} font_sz={:.1} overlay={}",
                                i,
                                *ch as u8 as char,
                                face_id,
                                x,
                                y,
                                width,
                                height,
                                ascent,
                                fg.r,
                                fg.g,
                                fg.b,
                                bg_str,
                                font_size,
                                is_overlay
                            );
                            logged_count += 1;
                        }
                    }
                    FrameGlyph::Stretch {
                        x,
                        y,
                        width,
                        height,
                        bg,
                        is_overlay,
                        ..
                    } => {
                        if *y < 32.0 && *y + *height > 24.0 {
                            tracing::debug!(
                                "frame_glyph[{}]: Stretch pos=({:.1},{:.1}) size=({:.1},{:.1}) bg=({:.3},{:.3},{:.3}) overlay={}",
                                i,
                                x,
                                y,
                                width,
                                height,
                                bg.r,
                                bg.g,
                                bg.b,
                                is_overlay
                            );
                            logged_count += 1;
                        }
                    }
                    FrameGlyph::Background { bounds, color } => {
                        if bounds.y < 32.0 && bounds.y + bounds.height > 24.0 {
                            tracing::debug!(
                                "frame_glyph[{}]: Background pos=({:.1},{:.1}) size=({:.1},{:.1}) color=({:.3},{:.3},{:.3})",
                                i,
                                bounds.x,
                                bounds.y,
                                bounds.width,
                                bounds.height,
                                color.r,
                                color.g,
                                color.b
                            );
                            logged_count += 1;
                        }
                    }
                    FrameGlyph::Border {
                        x,
                        y,
                        width,
                        height,
                        color,
                        ..
                    } => {
                        if *y < 32.0 && *y + *height > 24.0 {
                            tracing::debug!(
                                "frame_glyph[{}]: Border pos=({:.1},{:.1}) size=({:.1},{:.1}) color=({:.3},{:.3},{:.3})",
                                i,
                                x,
                                y,
                                width,
                                height,
                                color.r,
                                color.g,
                                color.b
                            );
                            logged_count += 1;
                        }
                    }
                    _ => {}
                }
            }
        }

        // --- Merge adjacent boxed glyphs into spans ---
        // All box faces get span-merged for proper border rendering.
        // Only faces with corner_radius > 0 get the SDF rounded rect treatment
        // (background suppression + SDF fill + SDF border).
        // Standard boxes (corner_radius=0) get merged rect borders drawn after text.
        let mut box_spans: Vec<BoxSpan> = Vec::new();

        for glyph in &frame_glyphs.glyphs {
            // Extract position info from both Char and Stretch glyphs with box faces
            let (gx, gy, gw, gh, gface_id, g_overlay, g_bg) = match glyph {
                FrameGlyph::Char {
                    x,
                    y,
                    width,
                    height,
                    face_id,
                    is_overlay,
                    bg,
                    ..
                } => (*x, *y, *width, *height, *face_id, *is_overlay, *bg),
                FrameGlyph::Stretch {
                    x,
                    y,
                    width,
                    height,
                    face_id,
                    is_overlay,
                    bg,
                    ..
                } => (*x, *y, *width, *height, *face_id, *is_overlay, Some(*bg)),
                _ => continue,
            };

            // Only include glyphs whose face has BOX attribute
            match faces.get(&gface_id) {
                Some(f) if f.attributes.contains(FaceAttributes::BOX) && f.box_line_width > 0 => {}
                _ => continue,
            };

            // Check if this glyph's face has rounded corners
            let is_rounded = faces
                .get(&gface_id)
                .map(|f| f.box_corner_radius > 0)
                .unwrap_or(false);

            let merged = if let Some(last) = box_spans.last_mut() {
                let same_row = (last.y - gy).abs() < 0.5 && (last.height - gh).abs() < 0.5;
                let same_overlay = last.is_overlay == g_overlay;
                let adjacent = (gx - (last.x + last.width)).abs() < 1.0;
                let same_face = last.face_id == gface_id;

                // Merge rules:
                // - Rounded boxes: only merge same face_id (keep separate boxes)
                // - Sharp overlay boxes (mode-line): merge across face_ids (continuity)
                // - Sharp non-overlay boxes (content): only merge same face_id
                let last_is_rounded = faces
                    .get(&last.face_id)
                    .map(|f| f.box_corner_radius > 0)
                    .unwrap_or(false);
                let face_ok = if is_rounded || last_is_rounded {
                    same_face // rounded: strict same-face merge
                } else if g_overlay {
                    true // sharp overlay: merge across faces (mode-line)
                } else {
                    same_face // sharp non-overlay: strict same-face merge
                };

                if same_row && same_overlay && adjacent && face_ok {
                    last.width = gx + gw - last.x;
                    true
                } else {
                    false
                }
            } else {
                false
            };

            if !merged {
                box_spans.push(BoxSpan {
                    x: gx,
                    y: gy,
                    width: gw,
                    height: gh,
                    face_id: gface_id,
                    is_overlay: g_overlay,
                    bg: g_bg,
                });
            }
        }

        // Helper: test whether a glyph position overlaps any ROUNDED box span.
        // Only suppresses backgrounds for rounded boxes (corner_radius > 0).
        // Standard boxes (corner_radius=0) keep normal rect backgrounds.
        let box_margin: f32 = box_spans
            .iter()
            .filter_map(|s| {
                faces
                    .get(&s.face_id)
                    .filter(|f| f.box_corner_radius > 0)
                    .map(|f| f.box_line_width as f32)
            })
            .fold(0.0_f32, f32::max);
        let overlaps_rounded_box_span =
            |gx: f32, gy: f32, g_overlay: bool, spans: &[BoxSpan]| -> bool {
                if box_margin <= 0.0 {
                    return false;
                }
                spans.iter().any(|s| {
                    // Only check rounded box spans with the same overlay status
                    if s.is_overlay != g_overlay {
                        return false;
                    }
                    let is_rounded = faces
                        .get(&s.face_id)
                        .map(|f| f.box_corner_radius > 0)
                        .unwrap_or(false);
                    if !is_rounded {
                        return false;
                    }
                    gx >= s.x - box_margin - 0.5
                        && gx < s.x + s.width + box_margin + 0.5
                        && gy >= s.y - box_margin - 0.5
                        && gy < s.y + s.height + box_margin + 0.5
                })
            };

        // --- Collect non-overlay backgrounds ---
        let mut non_overlay_rect_vertices: Vec<RectVertex> = Vec::new();

        // Background gradient (rendered behind everything)
        if let Some((top, bottom)) = background_gradient {
            let top_color = Color::new(top.0, top.1, top.2, 1.0).srgb_to_linear();
            let bot_color = Color::new(bottom.0, bottom.1, bottom.2, 1.0).srgb_to_linear();
            let tc = [top_color.r, top_color.g, top_color.b, top_color.a];
            let bc = [bot_color.r, bot_color.g, bot_color.b, bot_color.a];
            // Two triangles forming a fullscreen quad with gradient
            // Top-left, top-right, bottom-left (triangle 1)
            non_overlay_rect_vertices.push(RectVertex {
                position: [0.0, 0.0],
                color: tc,
            });
            non_overlay_rect_vertices.push(RectVertex {
                position: [logical_w, 0.0],
                color: tc,
            });
            non_overlay_rect_vertices.push(RectVertex {
                position: [0.0, logical_h],
                color: bc,
            });
            // Top-right, bottom-right, bottom-left (triangle 2)
            non_overlay_rect_vertices.push(RectVertex {
                position: [logical_w, 0.0],
                color: tc,
            });
            non_overlay_rect_vertices.push(RectVertex {
                position: [logical_w, logical_h],
                color: bc,
            });
            non_overlay_rect_vertices.push(RectVertex {
                position: [0.0, logical_h],
                color: bc,
            });
        }

        // Window backgrounds
        for glyph in &frame_glyphs.glyphs {
            if let FrameGlyph::Background { bounds, color } = glyph {
                self.add_rect(
                    &mut non_overlay_rect_vertices,
                    bounds.x,
                    bounds.y,
                    bounds.width,
                    bounds.height,
                    color,
                );
            }
        }
        // Non-overlay stretches (skip those inside a box span)
        let has_line_anims =
            !self.active_line_anims.is_empty() || !self.active_scroll_spacings.is_empty();
        for glyph in &frame_glyphs.glyphs {
            if let FrameGlyph::Stretch {
                x,
                y,
                width,
                height,
                bg,
                is_overlay,
                stipple_id,
                stipple_fg,
                ..
            } = glyph
            {
                if !*is_overlay && !overlaps_rounded_box_span(*x, *y, false, &box_spans) {
                    let ya = if has_line_anims {
                        *y + self.line_y_offset(*x, *y)
                    } else {
                        *y
                    };
                    // Draw background color first
                    self.add_rect(&mut non_overlay_rect_vertices, *x, ya, *width, *height, bg);
                    // Overlay stipple pattern if present
                    if *stipple_id > 0 {
                        if let (Some(fg), Some(pat)) =
                            (stipple_fg, frame_glyphs.stipple_patterns.get(stipple_id))
                        {
                            self.render_stipple_pattern(
                                &mut non_overlay_rect_vertices,
                                *x,
                                ya,
                                *width,
                                *height,
                                fg,
                                pat,
                            );
                        }
                    }
                }
            }
        }
        // Non-overlay char backgrounds (skip boxed chars — they get rounded bg instead)
        for glyph in &frame_glyphs.glyphs {
            if let FrameGlyph::Char {
                x,
                y,
                width,
                height,
                bg,
                is_overlay,
                ..
            } = glyph
            {
                if !*is_overlay {
                    if let Some(bg_color) = bg {
                        if !overlaps_rounded_box_span(*x, *y, false, &box_spans) {
                            let ya = if has_line_anims {
                                *y + self.line_y_offset(*x, *y)
                            } else {
                                *y
                            };
                            self.add_rect(
                                &mut non_overlay_rect_vertices,
                                *x,
                                ya,
                                *width,
                                *height,
                                bg_color,
                            );
                        }
                    }
                }
            }
        }

        // --- Current line highlight ---
        if self.effects.line_highlight.enabled {
            let (lr, lg, lb, la) = self.effects.line_highlight.color;
            let hl_color = Color::new(lr, lg, lb, la);

            // Find the active cursor (non-hollow, i.e. active window)
            for glyph in &frame_glyphs.glyphs {
                if let FrameGlyph::Cursor {
                    y, height, style, ..
                } = glyph
                {
                    if !style.is_hollow() {
                        // Find the window this cursor belongs to
                        for info in &frame_glyphs.window_infos {
                            if info.selected {
                                // Draw highlight across the window width (excluding mode-line)
                                let hl_y = *y;
                                let hl_h = *height;
                                self.add_rect(
                                    &mut non_overlay_rect_vertices,
                                    info.bounds.x,
                                    hl_y,
                                    info.bounds.width,
                                    hl_h,
                                    &hl_color,
                                );
                                break;
                            }
                        }
                        break;
                    }
                }
            }
        }

        // --- Indent guides ---
        if self.effects.indent_guides.enabled {
            let (ig_r, ig_g, ig_b, ig_a) = self.effects.indent_guides.color;
            let guide_color = Color::new(ig_r, ig_g, ig_b, ig_a);
            let guide_width = 1.0_f32;

            // Detect char_width from frame
            let char_w = frame_glyphs.char_width.max(1.0);
            let tab_w = 4; // default tab width; we infer from the glyph spacing

            // Collect row info: group chars by Y coordinate to find rows,
            // then detect indent (leading space/tab) per row.
            struct RowInfo {
                y: f32,
                height: f32,
                first_non_space_x: f32,
                text_start_x: f32, // leftmost char X in the row
            }
            let mut rows: Vec<RowInfo> = Vec::new();
            let mut current_row_y: f32 = -1.0;
            let mut current_row_h: f32 = 0.0;
            let mut first_non_space_x: f32 = f32::MAX;
            let mut text_start_x: f32 = f32::MAX;
            let mut has_chars = false;

            for glyph in &frame_glyphs.glyphs {
                if let FrameGlyph::Char {
                    x,
                    y,
                    width,
                    height,
                    char: ch,
                    is_overlay,
                    ..
                } = glyph
                {
                    if *is_overlay {
                        continue;
                    }
                    let gy = *y;
                    if (gy - current_row_y).abs() > 0.5 {
                        // New row — save previous
                        if has_chars && first_non_space_x > text_start_x + char_w {
                            rows.push(RowInfo {
                                y: current_row_y,
                                height: current_row_h,
                                first_non_space_x,
                                text_start_x,
                            });
                        }
                        current_row_y = gy;
                        current_row_h = *height;
                        first_non_space_x = f32::MAX;
                        text_start_x = f32::MAX;
                        has_chars = false;
                    }
                    has_chars = true;
                    if *x < text_start_x {
                        text_start_x = *x;
                    }
                    if *ch != ' ' && *ch != '\t' && *x < first_non_space_x {
                        first_non_space_x = *x;
                    }
                }
            }
            // Save last row
            if has_chars && first_non_space_x > text_start_x + char_w {
                rows.push(RowInfo {
                    y: current_row_y,
                    height: current_row_h,
                    first_non_space_x,
                    text_start_x,
                });
            }

            // Draw guides at each tab-stop column within the indent region
            let tab_px = char_w * tab_w as f32;
            let use_rainbow = self.effects.indent_guides.rainbow_enabled
                && !self.effects.indent_guides.rainbow_colors.is_empty();
            for row in &rows {
                let mut col_x = row.text_start_x + tab_px;
                let mut depth: usize = 0;
                while col_x < row.first_non_space_x - 1.0 {
                    let color = if use_rainbow {
                        let (r, g, b, a) = self.effects.indent_guides.rainbow_colors
                            [depth % self.effects.indent_guides.rainbow_colors.len()];
                        Color::new(r, g, b, a)
                    } else {
                        guide_color
                    };
                    self.add_rect(
                        &mut non_overlay_rect_vertices,
                        col_x,
                        row.y,
                        guide_width,
                        row.height,
                        &color,
                    );
                    col_x += tab_px;
                    depth += 1;
                }
            }
        }

        // --- Visible whitespace dots ---
        if self.effects.show_whitespace.enabled {
            let (wr, wg, wb, wa) = self.effects.show_whitespace.color;
            let ws_color = Color::new(wr, wg, wb, wa);
            let dot_size = 1.5_f32;

            for glyph in &frame_glyphs.glyphs {
                if let FrameGlyph::Char {
                    char: ch,
                    x,
                    y,
                    width,
                    height,
                    ascent,
                    is_overlay,
                    ..
                } = glyph
                {
                    if *is_overlay {
                        continue;
                    }
                    if *ch == ' ' {
                        // Centered dot for space
                        let dot_x = *x + (*width - dot_size) / 2.0;
                        let dot_y = *y + (*ascent - dot_size / 2.0);
                        self.add_rect(
                            &mut non_overlay_rect_vertices,
                            dot_x,
                            dot_y,
                            dot_size,
                            dot_size,
                            &ws_color,
                        );
                    } else if *ch == '\t' {
                        // Small horizontal arrow for tab
                        let arrow_h = 1.5_f32;
                        let arrow_y = *y + (*ascent - arrow_h / 2.0);
                        let arrow_w = (*width - 4.0).max(4.0);
                        let arrow_x = *x + 2.0;
                        // Shaft
                        self.add_rect(
                            &mut non_overlay_rect_vertices,
                            arrow_x,
                            arrow_y,
                            arrow_w,
                            arrow_h,
                            &ws_color,
                        );
                        // Arrowhead (small triangle approximated as 2 rects)
                        let tip_x = arrow_x + arrow_w;
                        self.add_rect(
                            &mut non_overlay_rect_vertices,
                            tip_x - 3.0,
                            arrow_y - 1.5,
                            3.0,
                            arrow_h + 3.0,
                            &ws_color,
                        );
                    }
                }
            }
        }

        // --- Collect overlay backgrounds ---
        let mut overlay_rect_vertices: Vec<RectVertex> = Vec::new();

        // Overlay stretches (skip those inside a box span)
        for glyph in &frame_glyphs.glyphs {
            if let FrameGlyph::Stretch {
                x,
                y,
                width,
                height,
                bg,
                is_overlay,
                stipple_id,
                stipple_fg,
                ..
            } = glyph
            {
                if *is_overlay && !overlaps_rounded_box_span(*x, *y, true, &box_spans) {
                    self.add_rect(&mut overlay_rect_vertices, *x, *y, *width, *height, bg);
                    if *stipple_id > 0 {
                        if let (Some(fg), Some(pat)) =
                            (stipple_fg, frame_glyphs.stipple_patterns.get(stipple_id))
                        {
                            self.render_stipple_pattern(
                                &mut overlay_rect_vertices,
                                *x,
                                *y,
                                *width,
                                *height,
                                fg,
                                pat,
                            );
                        }
                    }
                }
            }
        }
        // Overlay char backgrounds (skip those inside a box span)
        for glyph in &frame_glyphs.glyphs {
            if let FrameGlyph::Char {
                x,
                y,
                width,
                height,
                bg,
                is_overlay,
                ..
            } = glyph
            {
                if *is_overlay {
                    if let Some(bg_color) = bg {
                        if !overlaps_rounded_box_span(*x, *y, true, &box_spans) {
                            self.add_rect(
                                &mut overlay_rect_vertices,
                                *x,
                                *y,
                                *width,
                                *height,
                                bg_color,
                            );
                        }
                    }
                }
            }
        }

        // === Collect cursor bg rect for inverse video (drawn before text) ===
        // For filled box cursor (style 0), we draw the cursor background BEFORE text
        // so the character under the cursor can be re-drawn with inverse colors on top.
        let mut cursor_bg_vertices: Vec<RectVertex> = Vec::new();

        // === Collect behind-text cursor shapes (animated trail for filled box) ===
        let mut behind_text_cursor_vertices: Vec<RectVertex> = Vec::new();

        // === Collect front cursors and borders (drawn after text) ===
        // Bar (1), hbar (2), hollow (3), borders — all drawn on top of text.
        // Filled box (0) is EXCLUDED here — handled by bg rect + trail + fg swap.
        let mut cursor_vertices: Vec<RectVertex> = Vec::new();

        // === Collect scroll bar thumbs (drawn as rounded rects) ===
        let mut scroll_bar_thumb_vertices: Vec<(f32, f32, f32, f32, f32, Color)> = Vec::new();

        for glyph in &frame_glyphs.glyphs {
            match glyph {
                FrameGlyph::Border {
                    x,
                    y,
                    width,
                    height,
                    color,
                } => {
                    self.add_rect(&mut cursor_vertices, *x, *y, *width, *height, color);
                }
                FrameGlyph::ScrollBar {
                    horizontal,
                    x,
                    y,
                    width,
                    height,
                    thumb_start,
                    thumb_size,
                    track_color,
                    thumb_color,
                } => {
                    // Draw scroll bar track (subtle, configurable opacity)
                    let subtle_track = Color::new(
                        track_color.r,
                        track_color.g,
                        track_color.b,
                        track_color.a * self.effects.scroll_bar.track_opacity,
                    );
                    self.add_rect(&mut cursor_vertices, *x, *y, *width, *height, &subtle_track);

                    // Compute thumb bounds
                    let (tx, ty, tw, th) = if *horizontal {
                        (*x + *thumb_start, *y, *thumb_size, *height)
                    } else {
                        (*x, *y + *thumb_start, *width, *thumb_size)
                    };

                    // Check hover: brighten thumb if mouse is over the scroll bar area
                    let (mx, my) = mouse_pos;
                    let hovered = mx >= *x && mx <= *x + *width && my >= *y && my <= *y + *height;
                    let bright = self.effects.scroll_bar.hover_brightness;
                    let effective_thumb = if hovered {
                        Color::new(
                            (thumb_color.r * bright).min(1.0),
                            (thumb_color.g * bright).min(1.0),
                            (thumb_color.b * bright).min(1.0),
                            thumb_color.a.min(1.0),
                        )
                    } else {
                        *thumb_color
                    };

                    // Rounded thumb with configurable pill radius
                    let radius = tw.min(th) * self.effects.scroll_bar.thumb_radius;
                    scroll_bar_thumb_vertices.push((tx, ty, tw, th, radius, effective_thumb));
                }
                FrameGlyph::Cursor {
                    window_id,
                    x,
                    y,
                    width,
                    height,
                    style,
                    color,
                } => {
                    // Compute effective cursor color (possibly overridden by color cycling)
                    let cycle_color;
                    let effective_color =
                        if self.effects.cursor_color_cycle.enabled && !style.is_hollow() {
                            let elapsed = self.cursor_color_cycle_start.elapsed().as_secs_f32();
                            let hue = (elapsed * self.effects.cursor_color_cycle.speed) % 1.0;
                            cycle_color = Self::hsl_to_color(
                                hue,
                                self.effects.cursor_color_cycle.saturation,
                                self.effects.cursor_color_cycle.lightness,
                            );
                            self.needs_continuous_redraw = true;
                            &cycle_color
                        } else {
                            color
                        };
                    // Cursor error pulse: override color on bell
                    let error_pulse_color;
                    let effective_color = if let Some(pulse) = self.cursor_error_pulse_override() {
                        if !style.is_hollow() {
                            error_pulse_color = pulse;
                            self.needs_continuous_redraw = true;
                            &error_pulse_color
                        } else {
                            effective_color
                        }
                    } else {
                        effective_color
                    };
                    // Cursor wake animation: scale factor for pop effect
                    let wake = self.cursor_wake_factor();
                    let wake_active = wake != 1.0 && !style.is_hollow();
                    if wake_active {
                        self.needs_continuous_redraw = true;
                    }
                    if matches!(style, CursorStyle::FilledBox) {
                        // Filled box cursor: split into bg rect + behind-text trail.
                        // The static cursor bg rect uses cursor_inverse info if available,
                        // otherwise falls back to the cursor color at the static position.
                        if cursor_visible {
                            if let Some(ref inv) = frame_glyphs.cursor_inverse {
                                // Draw cursor bg rect at static position (inverse video background)
                                let inv_color = if self.effects.cursor_color_cycle.enabled {
                                    effective_color
                                } else {
                                    &inv.cursor_bg
                                };
                                if wake_active {
                                    let (sx, sy, sw, sh) =
                                        Self::scale_rect(inv.x, inv.y, inv.width, inv.height, wake);
                                    self.add_rect(
                                        &mut cursor_bg_vertices,
                                        sx,
                                        sy,
                                        sw,
                                        sh,
                                        inv_color,
                                    );
                                } else {
                                    self.add_rect(
                                        &mut cursor_bg_vertices,
                                        inv.x,
                                        inv.y,
                                        inv.width,
                                        inv.height,
                                        inv_color,
                                    );
                                }
                            } else {
                                // No inverse info — draw opaque cursor at static position
                                if wake_active {
                                    let (sx, sy, sw, sh) =
                                        Self::scale_rect(*x, *y, *width, *height, wake);
                                    self.add_rect(
                                        &mut cursor_bg_vertices,
                                        sx,
                                        sy,
                                        sw,
                                        sh,
                                        effective_color,
                                    );
                                } else {
                                    self.add_rect(
                                        &mut cursor_bg_vertices,
                                        *x,
                                        *y,
                                        *width,
                                        *height,
                                        effective_color,
                                    );
                                }
                            }

                            // Draw animated trail/rect behind text
                            let use_corners = if let Some(ref anim) = animated_cursor {
                                *window_id == anim.window_id && anim.corners.is_some()
                            } else {
                                false
                            };

                            if use_corners {
                                if let Some(ref anim) = animated_cursor {
                                    if let Some(ref corners) = anim.corners {
                                        self.add_quad(
                                            &mut behind_text_cursor_vertices,
                                            corners,
                                            effective_color,
                                        );
                                    }
                                }
                            } else if let Some(ref anim) = animated_cursor {
                                if *window_id == anim.window_id {
                                    self.add_rect(
                                        &mut behind_text_cursor_vertices,
                                        anim.x,
                                        anim.y,
                                        anim.width,
                                        anim.height,
                                        effective_color,
                                    );
                                }
                            }
                        }
                    } else {
                        // Non-filled-box cursors: bar, hbar, hollow — drawn ON TOP of text
                        let use_corners = if let Some(ref anim) = animated_cursor {
                            *window_id == anim.window_id
                                && !style.is_hollow()
                                && anim.corners.is_some()
                        } else {
                            false
                        };

                        if use_corners {
                            if let Some(ref anim) = animated_cursor {
                                if let Some(ref corners) = anim.corners {
                                    if cursor_visible {
                                        self.add_quad(
                                            &mut cursor_vertices,
                                            corners,
                                            effective_color,
                                        );
                                    }
                                }
                            }
                        } else {
                            let (cx, cy, cw, ch) = if let Some(ref anim) = animated_cursor {
                                if *window_id == anim.window_id && !style.is_hollow() {
                                    (anim.x, anim.y, anim.width, anim.height)
                                } else {
                                    (*x, *y, *width, *height)
                                }
                            } else {
                                (*x, *y, *width, *height)
                            };

                            let should_draw = style.is_hollow() || cursor_visible;
                            if should_draw {
                                match style {
                                    CursorStyle::Bar(bar_w) => {
                                        // Bar (thin vertical line)
                                        if wake_active {
                                            let (sx, sy, sw, sh) =
                                                Self::scale_rect(cx, cy, *bar_w, ch, wake);
                                            self.add_rect(
                                                &mut cursor_vertices,
                                                sx,
                                                sy,
                                                sw,
                                                sh,
                                                effective_color,
                                            );
                                        } else {
                                            self.add_rect(
                                                &mut cursor_vertices,
                                                cx,
                                                cy,
                                                *bar_w,
                                                ch,
                                                effective_color,
                                            );
                                        }
                                    }
                                    CursorStyle::Hbar(hbar_h) => {
                                        // Underline (hbar at bottom)
                                        if wake_active {
                                            let (sx, sy, sw, sh) = Self::scale_rect(
                                                cx,
                                                cy + ch - *hbar_h,
                                                cw,
                                                *hbar_h,
                                                wake,
                                            );
                                            self.add_rect(
                                                &mut cursor_vertices,
                                                sx,
                                                sy,
                                                sw,
                                                sh,
                                                effective_color,
                                            );
                                        } else {
                                            self.add_rect(
                                                &mut cursor_vertices,
                                                cx,
                                                cy + ch - *hbar_h,
                                                cw,
                                                *hbar_h,
                                                effective_color,
                                            );
                                        }
                                    }
                                    CursorStyle::Hollow => {
                                        // Hollow box (4 border edges)
                                        self.add_rect(
                                            &mut cursor_vertices,
                                            cx,
                                            cy,
                                            cw,
                                            1.0,
                                            effective_color,
                                        );
                                        self.add_rect(
                                            &mut cursor_vertices,
                                            cx,
                                            cy + ch - 1.0,
                                            cw,
                                            1.0,
                                            effective_color,
                                        );
                                        self.add_rect(
                                            &mut cursor_vertices,
                                            cx,
                                            cy,
                                            1.0,
                                            ch,
                                            effective_color,
                                        );
                                        self.add_rect(
                                            &mut cursor_vertices,
                                            cx + cw - 1.0,
                                            cy,
                                            1.0,
                                            ch,
                                            effective_color,
                                        );
                                    }
                                    _ => {
                                        self.add_rect(
                                            &mut cursor_vertices,
                                            cx,
                                            cy,
                                            cw,
                                            ch,
                                            effective_color,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Create command encoder
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Frame Glyphs Encoder"),
            });

        // Render pass - Clear with frame background color since we rebuild
        // the entire frame from current_matrix each time (no incremental updates).
        let bg = &frame_glyphs.background;
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Frame Glyphs Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            // Pre-multiply RGB by alpha for correct compositing
                            r: (bg.r * bg.a) as f64,
                            g: (bg.g * bg.a) as f64,
                            b: (bg.b * bg.a) as f64,
                            a: bg.a as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            self.draw_non_overlay_backgrounds(&mut render_pass, &non_overlay_rect_vertices);

            // Build shared effect context for all effect functions.
            // Clone effect config into a local so we can mutably borrow `self`
            // while effect functions still read configuration.
            let effects_for_ctx = self.effects.clone();
            let ctx = super::effect_common::EffectCtx {
                effects: &effects_for_ctx,
                frame_glyphs,
                animated_cursor: &animated_cursor,
                cursor_visible,
                mouse_pos,
                surface_width,
                surface_height,
                aurora_start: self.aurora_start,
                scale_factor: self.scale_factor,
                logical_w: frame_glyphs.width,
                logical_h: frame_glyphs.height,
                renderer_width: self.width as f32,
                renderer_height: self.height as f32,
            };

            self.draw_pre_content_background_effects(&mut render_pass, &ctx, faces, &box_spans);

            self.draw_pre_content_effects(&mut render_pass, &ctx);
            self.draw_pre_text_cursor_layers(
                &mut render_pass,
                &cursor_bg_vertices,
                &behind_text_cursor_vertices,
            );

            // === Steps 4-6: Draw text and overlay in correct z-order ===
            // For each overlay pass:
            //   Pass 0 (non-overlay): draw buffer text (with cursor fg swap for inverse video)
            //   Pass 1 (overlay): draw overlay backgrounds first, then overlay text
            //
            // This ensures: non-overlay bg → cursor bg → trail → text → overlay bg → overlay text

            for overlay_pass in 0..2 {
                let want_overlay = overlay_pass == 1;

                // === Step 3: Draw overlay backgrounds before overlay text ===
                if want_overlay && !overlay_rect_vertices.is_empty() {
                    let rect_buffer =
                        self.device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("Overlay Rect Buffer"),
                                contents: bytemuck::cast_slice(&overlay_rect_vertices),
                                usage: wgpu::BufferUsages::VERTEX,
                            });

                    render_pass.set_pipeline(&self.rect_pipeline);
                    render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                    render_pass.set_vertex_buffer(0, rect_buffer.slice(..));
                    render_pass.draw(0..overlay_rect_vertices.len() as u32, 0..1);
                }

                // Draw filled rounded rect backgrounds for overlay ROUNDED boxed spans.
                if want_overlay {
                    let mut overlay_box_fill: Vec<RoundedRectVertex> = Vec::new();
                    for span in &box_spans {
                        if !span.is_overlay {
                            continue;
                        }
                        if let Some(ref bg_color) = span.bg {
                            if let Some(face) = faces.get(&span.face_id) {
                                if face.box_corner_radius <= 0 {
                                    continue;
                                }
                                let radius = (face.box_corner_radius as f32)
                                    .min(span.height * 0.45)
                                    .min(span.width * 0.45);
                                let fill_bw = span.height.max(span.width);
                                self.add_rounded_rect(
                                    &mut overlay_box_fill,
                                    span.x,
                                    span.y,
                                    span.width,
                                    span.height,
                                    fill_bw,
                                    radius,
                                    bg_color,
                                );
                            }
                        }
                    }
                    if !overlay_box_fill.is_empty() {
                        let fill_buffer =
                            self.device
                                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                    label: Some("Overlay Box Fill Buffer"),
                                    contents: bytemuck::cast_slice(&overlay_box_fill),
                                    usage: wgpu::BufferUsages::VERTEX,
                                });
                        render_pass.set_pipeline(&self.rounded_rect_pipeline);
                        render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                        render_pass.set_vertex_buffer(0, fill_buffer.slice(..));
                        render_pass.draw(0..overlay_box_fill.len() as u32, 0..1);
                    }
                }

                let mut mask_data: Vec<(GlyphKey, [GlyphVertex; 6])> = Vec::new();
                let mut color_data: Vec<(GlyphKey, [GlyphVertex; 6])> = Vec::new();
                // Composed glyphs rendered individually (each is unique, no batching)
                let mut composed_mask_data: Vec<(ComposedGlyphKey, [GlyphVertex; 6])> = Vec::new();
                let mut composed_color_data: Vec<(ComposedGlyphKey, [GlyphVertex; 6])> = Vec::new();

                for glyph in &frame_glyphs.glyphs {
                    if let FrameGlyph::Char {
                        char,
                        composed,
                        x,
                        y,
                        baseline,
                        width,
                        ascent,
                        fg,
                        face_id,
                        font_size,
                        is_overlay,
                        overstrike,
                        ..
                    } = glyph
                    {
                        if *is_overlay != want_overlay {
                            continue;
                        }

                        let face = faces.get(face_id);

                        // Decompose physical-pixel positions into integer + subpixel bin.
                        // The bin is baked into the rasterized bitmap by swash for subpixel
                        // accuracy; vertex positions stay on integer pixels (no Linear blur).
                        let sf = self.scale_factor;
                        let y_offset = if has_line_anims {
                            self.line_y_offset(*x, *y)
                        } else {
                            0.0
                        };
                        let phys_x = (*x) * sf;
                        let baseline_y = *baseline + y_offset;
                        let phys_y = baseline_y * sf;
                        let (x_int, x_bin) = SubpixelBin::new(phys_x);
                        let (y_int, y_bin) = SubpixelBin::new(phys_y);

                        // Look up or create the glyph texture
                        let cached_opt = if let Some(text) = composed {
                            // Composed grapheme cluster (emoji ZWJ, combining marks, etc.)
                            glyph_atlas.get_or_create_composed(
                                &self.device,
                                &self.queue,
                                text,
                                *face_id,
                                font_size.to_bits(),
                                face,
                                x_bin,
                                y_bin,
                            )
                        } else {
                            // Single character
                            let key = GlyphKey {
                                charcode: *char as u32,
                                face_id: *face_id,
                                font_size_bits: font_size.to_bits(),
                                x_bin,
                                y_bin,
                            };
                            glyph_atlas.get_or_create(&self.device, &self.queue, &key, face)
                        };

                        if let Some(cached) = cached_opt {
                            // Vertex positions from integer physical pixels + bearing,
                            // converted back to logical pixels.
                            let glyph_x = (x_int as f32 + cached.bearing_x) / sf;
                            let glyph_y = (y_int as f32 - cached.bearing_y) / sf;
                            let glyph_w = cached.width as f32 / sf;
                            let glyph_h = cached.height as f32 / sf;

                            // Clip non-overlay glyphs to their window's text area bottom.
                            // Overlay glyphs (mode-line, header-line, echo area) are not clipped.
                            let (glyph_h, tex_v_max) = if !want_overlay {
                                let clip_y = clip_bottom_for(*y);
                                if glyph_y + glyph_h > clip_y {
                                    let clipped_h = (clip_y - glyph_y).max(0.0);
                                    if clipped_h <= 0.0 {
                                        continue;
                                    }
                                    let v = clipped_h / glyph_h;
                                    (clipped_h, v)
                                } else {
                                    (glyph_h, 1.0)
                                }
                            } else {
                                (glyph_h, 1.0)
                            };

                            // Determine effective foreground color.
                            // For the character under a filled box cursor, swap to
                            // cursor_fg (inverse video) when cursor is visible.
                            let effective_fg = if cursor_visible {
                                if let Some(ref inv) = frame_glyphs.cursor_inverse {
                                    // Match if char cell overlaps cursor inverse position
                                    let x_match = (*x - inv.x).abs() < 1.0;
                                    let y_match = (*y - inv.y).abs() < 1.0;
                                    if x_match && y_match {
                                        &inv.cursor_fg
                                    } else {
                                        fg
                                    }
                                } else {
                                    fg
                                }
                            } else {
                                fg
                            };

                            // Color glyphs use white vertex color (no tinting),
                            // mask glyphs use foreground color for tinting
                            let fade_alpha =
                                self.text_fade_alpha(*x, *y) * self.mode_line_fade_alpha(*x, *y);
                            let color = if cached.is_color {
                                [1.0, 1.0, 1.0, fade_alpha]
                            } else {
                                [
                                    effective_fg.r,
                                    effective_fg.g,
                                    effective_fg.b,
                                    effective_fg.a * fade_alpha,
                                ]
                            };

                            // Debug: log glyphs near y≈27 (where gray line appears in screenshot)
                            // and first few header glyphs (y < 5) to see row start
                            if !want_overlay && (glyph_y + glyph_h > 24.0 && glyph_y < 32.0) {
                                tracing::debug!(
                                    "glyph_near_y27: char='{}' face={} pos=({:.1},{:.1}) size=({:.1},{:.1}) ascent={:.1} bottom={:.1} fg=({:.3},{:.3},{:.3},{:.3}) is_color={} cell=({:.1},{:.1},{:.1})",
                                    if let Some(text) = composed {
                                        text.to_string()
                                    } else {
                                        format!("{}", *char as u8 as char)
                                    },
                                    face_id,
                                    glyph_x,
                                    glyph_y,
                                    glyph_w,
                                    glyph_h,
                                    *ascent,
                                    glyph_y + glyph_h,
                                    color[0],
                                    color[1],
                                    color[2],
                                    color[3],
                                    cached.is_color,
                                    *x,
                                    *y,
                                    *width,
                                );
                            }
                            if !want_overlay && *y < 1.0 {
                                tracing::debug!(
                                    "first_row_glyph: char='{}' face={} cell=({:.1},{:.1},{:.1}) glyph_pos=({:.1},{:.1}) glyph_size=({:.1},{:.1}) ascent={:.1} fg=({:.3},{:.3},{:.3})",
                                    if let Some(text) = composed {
                                        text.to_string()
                                    } else {
                                        format!("{}", *char as u8 as char)
                                    },
                                    face_id,
                                    *x,
                                    *y,
                                    *width,
                                    glyph_x,
                                    glyph_y,
                                    glyph_w,
                                    glyph_h,
                                    *ascent,
                                    color[0],
                                    color[1],
                                    color[2],
                                );
                            }

                            let vertices = [
                                GlyphVertex {
                                    position: [glyph_x, glyph_y],
                                    tex_coords: [0.0, 0.0],
                                    color,
                                },
                                GlyphVertex {
                                    position: [glyph_x + glyph_w, glyph_y],
                                    tex_coords: [1.0, 0.0],
                                    color,
                                },
                                GlyphVertex {
                                    position: [glyph_x + glyph_w, glyph_y + glyph_h],
                                    tex_coords: [1.0, tex_v_max],
                                    color,
                                },
                                GlyphVertex {
                                    position: [glyph_x, glyph_y],
                                    tex_coords: [0.0, 0.0],
                                    color,
                                },
                                GlyphVertex {
                                    position: [glyph_x + glyph_w, glyph_y + glyph_h],
                                    tex_coords: [1.0, tex_v_max],
                                    color,
                                },
                                GlyphVertex {
                                    position: [glyph_x, glyph_y + glyph_h],
                                    tex_coords: [0.0, tex_v_max],
                                    color,
                                },
                            ];

                            // Overstrike: simulate bold by drawing the
                            // glyph a second time shifted 1px right.
                            // This matches official Emacs behavior when
                            // a bold font variant is unavailable.
                            let overstrike_vertices = if *overstrike {
                                let ox = 1.0 / self.scale_factor;
                                Some([
                                    GlyphVertex {
                                        position: [glyph_x + ox, glyph_y],
                                        tex_coords: [0.0, 0.0],
                                        color,
                                    },
                                    GlyphVertex {
                                        position: [glyph_x + ox + glyph_w, glyph_y],
                                        tex_coords: [1.0, 0.0],
                                        color,
                                    },
                                    GlyphVertex {
                                        position: [glyph_x + ox + glyph_w, glyph_y + glyph_h],
                                        tex_coords: [1.0, tex_v_max],
                                        color,
                                    },
                                    GlyphVertex {
                                        position: [glyph_x + ox, glyph_y],
                                        tex_coords: [0.0, 0.0],
                                        color,
                                    },
                                    GlyphVertex {
                                        position: [glyph_x + ox + glyph_w, glyph_y + glyph_h],
                                        tex_coords: [1.0, tex_v_max],
                                        color,
                                    },
                                    GlyphVertex {
                                        position: [glyph_x + ox, glyph_y + glyph_h],
                                        tex_coords: [0.0, tex_v_max],
                                        color,
                                    },
                                ])
                            } else {
                                None
                            };

                            if let Some(text) = composed {
                                let ckey = ComposedGlyphKey {
                                    text: text.clone(),
                                    face_id: *face_id,
                                    font_size_bits: font_size.to_bits(),
                                    x_bin,
                                    y_bin,
                                };
                                if cached.is_color {
                                    composed_color_data.push((ckey.clone(), vertices));
                                    if let Some(ov) = overstrike_vertices {
                                        composed_color_data.push((ckey, ov));
                                    }
                                } else {
                                    composed_mask_data.push((ckey.clone(), vertices));
                                    if let Some(ov) = overstrike_vertices {
                                        composed_mask_data.push((ckey, ov));
                                    }
                                }
                            } else {
                                let key = GlyphKey {
                                    charcode: *char as u32,
                                    face_id: *face_id,
                                    font_size_bits: font_size.to_bits(),
                                    x_bin,
                                    y_bin,
                                };
                                if cached.is_color {
                                    color_data.push((key.clone(), vertices));
                                    if let Some(ov) = overstrike_vertices {
                                        color_data.push((key, ov));
                                    }
                                } else {
                                    mask_data.push((key.clone(), vertices));
                                    if let Some(ov) = overstrike_vertices {
                                        mask_data.push((key, ov));
                                    }
                                }
                            }
                        }
                    }
                }

                tracing::trace!(
                    "render_frame_glyphs: overlay={} {} mask glyphs, {} color glyphs",
                    want_overlay,
                    mask_data.len(),
                    color_data.len()
                );
                // Debug: dump first few glyph positions
                if !mask_data.is_empty() && !want_overlay {
                    for (i, (key, verts)) in mask_data.iter().take(3).enumerate() {
                        let p0 = verts[0].position;
                        let c0 = verts[0].color;
                        tracing::debug!(
                            "  glyph[{}]: charcode={} pos=({:.1},{:.1}) color=({:.3},{:.3},{:.3},{:.3}) logical_w={:.1}",
                            i,
                            key.charcode,
                            p0[0],
                            p0[1],
                            c0[0],
                            c0[1],
                            c0[2],
                            c0[3],
                            logical_w
                        );
                    }
                }

                // Draw mask glyphs with glyph pipeline (alpha tinted with foreground)
                // Sort by GlyphKey so identical characters batch into single draw calls,
                // significantly reducing GPU state changes (set_bind_group calls).
                if !mask_data.is_empty() {
                    mask_data.sort_by(|(a, _), (b, _)| {
                        a.face_id
                            .cmp(&b.face_id)
                            .then(a.font_size_bits.cmp(&b.font_size_bits))
                            .then(a.charcode.cmp(&b.charcode))
                    });

                    render_pass.set_pipeline(&self.glyph_pipeline);
                    render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);

                    let all_vertices: Vec<GlyphVertex> = mask_data
                        .iter()
                        .flat_map(|(_, verts)| verts.iter().copied())
                        .collect();

                    let glyph_buffer =
                        self.device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("Glyph Vertex Buffer"),
                                contents: bytemuck::cast_slice(&all_vertices),
                                usage: wgpu::BufferUsages::VERTEX,
                            });

                    render_pass.set_vertex_buffer(0, glyph_buffer.slice(..));

                    // Batch consecutive glyphs sharing the same texture
                    let mut i = 0;
                    while i < mask_data.len() {
                        let (ref key, _) = mask_data[i];
                        if let Some(cached) = glyph_atlas.get(key) {
                            let batch_start = i;
                            i += 1;
                            while i < mask_data.len() && mask_data[i].0 == *key {
                                i += 1;
                            }
                            let vert_start = (batch_start * 6) as u32;
                            let vert_end = (i * 6) as u32;
                            render_pass.set_bind_group(1, &cached.bind_group, &[]);
                            render_pass.draw(vert_start..vert_end, 0..1);
                        } else {
                            i += 1;
                        }
                    }
                }

                // Draw color glyphs with image pipeline (direct RGBA, e.g. color emoji)
                if !color_data.is_empty() {
                    color_data.sort_by(|(a, _), (b, _)| {
                        a.face_id
                            .cmp(&b.face_id)
                            .then(a.font_size_bits.cmp(&b.font_size_bits))
                            .then(a.charcode.cmp(&b.charcode))
                    });

                    render_pass.set_pipeline(&self.image_pipeline);
                    render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);

                    let all_vertices: Vec<GlyphVertex> = color_data
                        .iter()
                        .flat_map(|(_, verts)| verts.iter().copied())
                        .collect();

                    let color_buffer =
                        self.device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("Color Glyph Vertex Buffer"),
                                contents: bytemuck::cast_slice(&all_vertices),
                                usage: wgpu::BufferUsages::VERTEX,
                            });

                    render_pass.set_vertex_buffer(0, color_buffer.slice(..));

                    // Batch consecutive color glyphs sharing the same texture
                    let mut i = 0;
                    while i < color_data.len() {
                        let (ref key, _) = color_data[i];
                        if let Some(cached) = glyph_atlas.get(key) {
                            let batch_start = i;
                            i += 1;
                            while i < color_data.len() && color_data[i].0 == *key {
                                i += 1;
                            }
                            let vert_start = (batch_start * 6) as u32;
                            let vert_end = (i * 6) as u32;
                            render_pass.set_bind_group(1, &cached.bind_group, &[]);
                            render_pass.draw(vert_start..vert_end, 0..1);
                        } else {
                            i += 1;
                        }
                    }
                }

                // Draw composed mask glyphs (each unique, no batching)
                if !composed_mask_data.is_empty() {
                    render_pass.set_pipeline(&self.glyph_pipeline);
                    render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);

                    for (ckey, verts) in &composed_mask_data {
                        if let Some(cached) = glyph_atlas.get_composed(ckey) {
                            let vbuf =
                                self.device
                                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                        label: Some("Composed Glyph VB"),
                                        contents: bytemuck::cast_slice(verts),
                                        usage: wgpu::BufferUsages::VERTEX,
                                    });
                            render_pass.set_vertex_buffer(0, vbuf.slice(..));
                            render_pass.set_bind_group(1, &cached.bind_group, &[]);
                            render_pass.draw(0..6, 0..1);
                        }
                    }
                }

                // Draw composed color glyphs (emoji ZWJ sequences, etc.)
                if !composed_color_data.is_empty() {
                    render_pass.set_pipeline(&self.image_pipeline);
                    render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);

                    for (ckey, verts) in &composed_color_data {
                        if let Some(cached) = glyph_atlas.get_composed(ckey) {
                            let vbuf =
                                self.device
                                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                        label: Some("Composed Color Glyph VB"),
                                        contents: bytemuck::cast_slice(verts),
                                        usage: wgpu::BufferUsages::VERTEX,
                                    });
                            render_pass.set_vertex_buffer(0, vbuf.slice(..));
                            render_pass.set_bind_group(1, &cached.bind_group, &[]);
                            render_pass.draw(0..6, 0..1);
                        }
                    }
                }

                // === Draw text decorations (underline, overline, strike-through) ===
                // Rendered after text so decorations appear on top of glyphs.
                // Box borders are handled separately via merged box_spans below.
                {
                    let mut decoration_vertices: Vec<RectVertex> = Vec::new();

                    for glyph in &frame_glyphs.glyphs {
                        if let FrameGlyph::Char {
                            x,
                            y,
                            baseline,
                            width,
                            height,
                            ascent,
                            fg,
                            face_id,
                            underline,
                            underline_color,
                            strike_through,
                            strike_through_color,
                            overline,
                            overline_color,
                            is_overlay,
                            ..
                        } = glyph
                        {
                            if *is_overlay != want_overlay {
                                continue;
                            }

                            let y_offset = if has_line_anims {
                                self.line_y_offset(*x, *y)
                            } else {
                                0.0
                            };
                            let ya = *y + y_offset;
                            let baseline_y = *baseline + y_offset;

                            // Get per-face font metrics for proper decoration positioning
                            let (ul_pos, ul_thick) = frame_glyphs
                                .faces
                                .get(face_id)
                                .map(|f| {
                                    (f.underline_position as f32, f.underline_thickness as f32)
                                })
                                .unwrap_or((1.0, 1.0));

                            // --- Underline ---
                            if *underline > 0 {
                                let ul_color = underline_color.as_ref().unwrap_or(fg);
                                let ul_y = baseline_y + ul_pos;
                                let line_thickness = ul_thick.max(1.0);

                                match underline {
                                    1 => {
                                        // Single solid line
                                        self.add_rect(
                                            &mut decoration_vertices,
                                            *x,
                                            ul_y,
                                            *width,
                                            line_thickness,
                                            ul_color,
                                        );
                                    }
                                    2 => {
                                        // Wave: smooth sine wave underline
                                        let amplitude: f32 = 2.0;
                                        let wavelength: f32 = 8.0;
                                        let seg_w: f32 = 1.0;
                                        let mut cx = *x;
                                        while cx < *x + *width {
                                            let sw = seg_w.min(*x + *width - cx);
                                            let phase =
                                                (cx - *x) * std::f32::consts::TAU / wavelength;
                                            let offset = phase.sin() * amplitude;
                                            self.add_rect(
                                                &mut decoration_vertices,
                                                cx,
                                                ul_y + offset,
                                                sw,
                                                line_thickness,
                                                ul_color,
                                            );
                                            cx += seg_w;
                                        }
                                    }
                                    3 => {
                                        // Double line
                                        self.add_rect(
                                            &mut decoration_vertices,
                                            *x,
                                            ul_y,
                                            *width,
                                            line_thickness,
                                            ul_color,
                                        );
                                        self.add_rect(
                                            &mut decoration_vertices,
                                            *x,
                                            ul_y + line_thickness + 1.0,
                                            *width,
                                            line_thickness,
                                            ul_color,
                                        );
                                    }
                                    4 => {
                                        // Dots (dot size = thickness, gap = 2px)
                                        let mut cx = *x;
                                        while cx < *x + *width {
                                            let dw = line_thickness.min(*x + *width - cx);
                                            self.add_rect(
                                                &mut decoration_vertices,
                                                cx,
                                                ul_y,
                                                dw,
                                                line_thickness,
                                                ul_color,
                                            );
                                            cx += line_thickness + 2.0;
                                        }
                                    }
                                    5 => {
                                        // Dashes (4px with 3px gap)
                                        let mut cx = *x;
                                        while cx < *x + *width {
                                            let dw = 4.0_f32.min(*x + *width - cx);
                                            self.add_rect(
                                                &mut decoration_vertices,
                                                cx,
                                                ul_y,
                                                dw,
                                                line_thickness,
                                                ul_color,
                                            );
                                            cx += 7.0;
                                        }
                                    }
                                    _ => {
                                        // Fallback: single line
                                        self.add_rect(
                                            &mut decoration_vertices,
                                            *x,
                                            ul_y,
                                            *width,
                                            line_thickness,
                                            ul_color,
                                        );
                                    }
                                }
                            }

                            // --- Overline ---
                            if *overline > 0 {
                                let ol_color = overline_color.as_ref().unwrap_or(fg);
                                self.add_rect(
                                    &mut decoration_vertices,
                                    *x,
                                    ya,
                                    *width,
                                    ul_thick.max(1.0),
                                    ol_color,
                                );
                            }

                            // --- Strike-through ---
                            if *strike_through > 0 {
                                let st_color = strike_through_color.as_ref().unwrap_or(fg);
                                // Position at ~1/3 of ascent above baseline (standard typographic position)
                                let st_y = baseline_y - *ascent / 3.0;
                                self.add_rect(
                                    &mut decoration_vertices,
                                    *x,
                                    st_y,
                                    *width,
                                    ul_thick.max(1.0),
                                    st_color,
                                );
                            }
                        }
                    }

                    if !decoration_vertices.is_empty() {
                        let decoration_buffer =
                            self.device
                                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                    label: Some("Decoration Rect Buffer"),
                                    contents: bytemuck::cast_slice(&decoration_vertices),
                                    usage: wgpu::BufferUsages::VERTEX,
                                });

                        render_pass.set_pipeline(&self.rect_pipeline);
                        render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                        render_pass.set_vertex_buffer(0, decoration_buffer.slice(..));
                        render_pass.draw(0..decoration_vertices.len() as u32, 0..1);
                    }
                }

                // === Draw box borders (merged spans) ===
                // Standard boxes (corner_radius=0): merged rect borders (top/bottom/left/right).
                // Rounded boxes (corner_radius>0): SDF border ring.
                {
                    // Sharp box borders as merged rect spans
                    let mut sharp_border_vertices: Vec<RectVertex> = Vec::new();
                    // Rounded box borders via SDF
                    let mut rounded_border_vertices: Vec<RoundedRectVertex> = Vec::new();

                    // Filter spans for this overlay pass
                    let pass_spans: Vec<usize> = box_spans
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| s.is_overlay == want_overlay)
                        .map(|(i, _)| i)
                        .collect();

                    for (idx_in_pass, &span_idx) in pass_spans.iter().enumerate() {
                        let span = &box_spans[span_idx];
                        if let Some(face) = faces.get(&span.face_id) {
                            let bx_color = face.box_color.as_ref().unwrap_or(&face.foreground);
                            let bw = face.box_line_width as f32;

                            if face.box_corner_radius > 0 {
                                // Rounded border via SDF (with optional fancy style)
                                let radius = (face.box_corner_radius as f32)
                                    .min(span.height * 0.45)
                                    .min(span.width * 0.45);
                                let color2 = face.box_color2.as_ref().unwrap_or(bx_color);
                                self.add_rounded_rect_styled(
                                    &mut rounded_border_vertices,
                                    span.x,
                                    span.y,
                                    span.width,
                                    span.height,
                                    bw,
                                    radius,
                                    bx_color,
                                    face.box_border_style,
                                    face.box_border_speed,
                                    color2,
                                );
                                if face.box_border_style > 0 {
                                    self.has_animated_borders = true;
                                }
                            } else {
                                // Sharp border — for overlay spans (mode-line), suppress
                                // internal left/right borders between adjacent spans for
                                // continuity. For non-overlay spans, always draw all 4 borders.
                                let suppress_internal = span.is_overlay;
                                let has_left_neighbor = suppress_internal && idx_in_pass > 0 && {
                                    let prev = &box_spans[pass_spans[idx_in_pass - 1]];
                                    (prev.y - span.y).abs() < 0.5
                                        && ((prev.x + prev.width) - span.x).abs() < 1.5
                                };
                                let has_right_neighbor =
                                    suppress_internal && idx_in_pass + 1 < pass_spans.len() && {
                                        let next = &box_spans[pass_spans[idx_in_pass + 1]];
                                        (next.y - span.y).abs() < 0.5
                                            && (next.x - (span.x + span.width)).abs() < 1.5
                                    };

                                // Compute edge colors for 3D box types
                                let (top_left_color, bottom_right_color) = match face.box_type {
                                    BoxType::Raised3D => {
                                        let light = Color {
                                            r: (bx_color.r * 1.4).min(1.0),
                                            g: (bx_color.g * 1.4).min(1.0),
                                            b: (bx_color.b * 1.4).min(1.0),
                                            a: bx_color.a,
                                        };
                                        let dark = Color {
                                            r: bx_color.r * 0.6,
                                            g: bx_color.g * 0.6,
                                            b: bx_color.b * 0.6,
                                            a: bx_color.a,
                                        };
                                        (light, dark)
                                    }
                                    BoxType::Sunken3D => {
                                        let light = Color {
                                            r: (bx_color.r * 1.4).min(1.0),
                                            g: (bx_color.g * 1.4).min(1.0),
                                            b: (bx_color.b * 1.4).min(1.0),
                                            a: bx_color.a,
                                        };
                                        let dark = Color {
                                            r: bx_color.r * 0.6,
                                            g: bx_color.g * 0.6,
                                            b: bx_color.b * 0.6,
                                            a: bx_color.a,
                                        };
                                        (dark, light)
                                    }
                                    _ => (bx_color.clone(), bx_color.clone()),
                                };

                                // Top
                                self.add_rect(
                                    &mut sharp_border_vertices,
                                    span.x,
                                    span.y,
                                    span.width,
                                    bw,
                                    &top_left_color,
                                );
                                // Bottom
                                self.add_rect(
                                    &mut sharp_border_vertices,
                                    span.x,
                                    span.y + span.height - bw,
                                    span.width,
                                    bw,
                                    &bottom_right_color,
                                );
                                // Left (only if no adjacent span to the left on same row)
                                if !has_left_neighbor {
                                    self.add_rect(
                                        &mut sharp_border_vertices,
                                        span.x,
                                        span.y,
                                        bw,
                                        span.height,
                                        &top_left_color,
                                    );
                                }
                                // Right (only if no adjacent span to the right on same row)
                                if !has_right_neighbor {
                                    self.add_rect(
                                        &mut sharp_border_vertices,
                                        span.x + span.width - bw,
                                        span.y,
                                        bw,
                                        span.height,
                                        &bottom_right_color,
                                    );
                                }
                            }
                        }
                    }

                    // Draw sharp box borders
                    if !sharp_border_vertices.is_empty() {
                        let sharp_buffer =
                            self.device
                                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                    label: Some("Sharp Box Border Buffer"),
                                    contents: bytemuck::cast_slice(&sharp_border_vertices),
                                    usage: wgpu::BufferUsages::VERTEX,
                                });
                        render_pass.set_pipeline(&self.rect_pipeline);
                        render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                        render_pass.set_vertex_buffer(0, sharp_buffer.slice(..));
                        render_pass.draw(0..sharp_border_vertices.len() as u32, 0..1);
                    }

                    // Draw rounded box borders
                    if !rounded_border_vertices.is_empty() {
                        let rounded_buffer =
                            self.device
                                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                    label: Some("Rounded Box Border Buffer"),
                                    contents: bytemuck::cast_slice(&rounded_border_vertices),
                                    usage: wgpu::BufferUsages::VERTEX,
                                });
                        render_pass.set_pipeline(&self.rounded_rect_pipeline);
                        render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                        render_pass.set_vertex_buffer(0, rounded_buffer.slice(..));
                        render_pass.draw(0..rounded_border_vertices.len() as u32, 0..1);
                    }
                }
            }

            // Draw inline images
            render_pass.set_pipeline(&self.image_pipeline);
            render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);

            for glyph in &frame_glyphs.glyphs {
                if let FrameGlyph::Image {
                    image_id,
                    x,
                    y,
                    width,
                    height,
                } = glyph
                {
                    // Clip to window text-area boundary
                    let clip_y = clip_bottom_for(*y);
                    let (clipped_height, tex_v_max) = if *y + *height > clip_y {
                        let clipped = (clip_y - *y).max(0.0);
                        let v_max = if *height > 0.0 {
                            clipped / *height
                        } else {
                            1.0
                        };
                        (clipped, v_max)
                    } else {
                        (*height, 1.0)
                    };

                    // Skip if fully clipped
                    if clipped_height <= 0.0 {
                        continue;
                    }

                    tracing::debug!(
                        "Rendering image {} at ({}, {}) size {}x{} (clipped to {})",
                        image_id,
                        x,
                        y,
                        width,
                        height,
                        clipped_height
                    );
                    // Check if image texture is ready
                    if let Some(cached) = self.image_cache.get(*image_id) {
                        // Create vertices for image quad (white color = no tinting)
                        let vertices = [
                            GlyphVertex {
                                position: [*x, *y],
                                tex_coords: [0.0, 0.0],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [*x + *width, *y],
                                tex_coords: [1.0, 0.0],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [*x + *width, *y + clipped_height],
                                tex_coords: [1.0, tex_v_max],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [*x, *y],
                                tex_coords: [0.0, 0.0],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [*x + *width, *y + clipped_height],
                                tex_coords: [1.0, tex_v_max],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [*x, *y + clipped_height],
                                tex_coords: [0.0, tex_v_max],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                        ];

                        let image_buffer =
                            self.device
                                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                    label: Some("Image Vertex Buffer"),
                                    contents: bytemuck::cast_slice(&vertices),
                                    usage: wgpu::BufferUsages::VERTEX,
                                });

                        render_pass.set_bind_group(1, &cached.bind_group, &[]);
                        render_pass.set_vertex_buffer(0, image_buffer.slice(..));
                        render_pass.draw(0..6, 0..1);
                    }
                }
            }

            // Apply video loop_count and autoplay before rendering
            #[cfg(feature = "video")]
            for glyph in &frame_glyphs.glyphs {
                if let FrameGlyph::Video {
                    video_id,
                    loop_count,
                    autoplay,
                    ..
                } = glyph
                {
                    if *loop_count != 0 {
                        self.video_cache.set_loop(*video_id, *loop_count);
                    }
                    if *autoplay {
                        let state = self.video_cache.get_state(*video_id);
                        if matches!(
                            state,
                            Some(super::super::VideoState::Stopped)
                                | Some(super::super::VideoState::Loading)
                        ) {
                            self.video_cache.play(*video_id);
                        }
                    }
                }
            }

            // Draw inline videos
            #[cfg(feature = "video")]
            for glyph in &frame_glyphs.glyphs {
                if let FrameGlyph::Video {
                    video_id,
                    x,
                    y,
                    width,
                    height,
                    ..
                } = glyph
                {
                    // Clip to window text-area boundary
                    let clip_y = clip_bottom_for(*y);
                    let (clipped_height, tex_v_max) = if *y + *height > clip_y {
                        let clipped = (clip_y - *y).max(0.0);
                        let v_max = if *height > 0.0 {
                            clipped / *height
                        } else {
                            1.0
                        };
                        (clipped, v_max)
                    } else {
                        (*height, 1.0)
                    };

                    // Skip if fully clipped
                    if clipped_height <= 0.0 {
                        continue;
                    }

                    // Check if video texture is ready
                    if let Some(cached) = self.video_cache.get(*video_id) {
                        tracing::trace!(
                            "Rendering video {} at ({}, {}) size {}x{} (clipped to {}), frame_count={}",
                            video_id,
                            x,
                            y,
                            width,
                            height,
                            clipped_height,
                            cached.frame_count
                        );
                        if let Some(ref bind_group) = cached.bind_group {
                            // Create vertices for video quad (white color = no tinting)
                            let vertices = [
                                GlyphVertex {
                                    position: [*x, *y],
                                    tex_coords: [0.0, 0.0],
                                    color: [1.0, 1.0, 1.0, 1.0],
                                },
                                GlyphVertex {
                                    position: [*x + *width, *y],
                                    tex_coords: [1.0, 0.0],
                                    color: [1.0, 1.0, 1.0, 1.0],
                                },
                                GlyphVertex {
                                    position: [*x + *width, *y + clipped_height],
                                    tex_coords: [1.0, tex_v_max],
                                    color: [1.0, 1.0, 1.0, 1.0],
                                },
                                GlyphVertex {
                                    position: [*x, *y],
                                    tex_coords: [0.0, 0.0],
                                    color: [1.0, 1.0, 1.0, 1.0],
                                },
                                GlyphVertex {
                                    position: [*x + *width, *y + clipped_height],
                                    tex_coords: [1.0, tex_v_max],
                                    color: [1.0, 1.0, 1.0, 1.0],
                                },
                                GlyphVertex {
                                    position: [*x, *y + clipped_height],
                                    tex_coords: [0.0, tex_v_max],
                                    color: [1.0, 1.0, 1.0, 1.0],
                                },
                            ];

                            let video_buffer =
                                self.device
                                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                        label: Some("Video Vertex Buffer"),
                                        contents: bytemuck::cast_slice(&vertices),
                                        usage: wgpu::BufferUsages::VERTEX,
                                    });

                            render_pass.set_bind_group(1, bind_group, &[]);
                            render_pass.set_vertex_buffer(0, video_buffer.slice(..));
                            render_pass.draw(0..6, 0..1);
                        } else {
                            tracing::warn!("Video {} has no bind_group!", video_id);
                        }
                    } else {
                        tracing::warn!("Video {} not found in cache!", video_id);
                    }
                }
            }

            // Draw inline webkit views (use opaque pipeline — DMA-BUF XRGB has alpha=0)
            #[cfg(feature = "wpe-webkit")]
            {
                render_pass.set_pipeline(&self.opaque_image_pipeline);
                render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);

                for glyph in &frame_glyphs.glyphs {
                    if let FrameGlyph::WebKit {
                        webkit_id,
                        x,
                        y,
                        width,
                        height,
                    } = glyph
                    {
                        // Clip to window text-area boundary
                        let clip_y = clip_bottom_for(*y);
                        let (clipped_height, tex_v_max) = if *y + *height > clip_y {
                            let clipped = (clip_y - *y).max(0.0);
                            let v_max = if *height > 0.0 {
                                clipped / *height
                            } else {
                                1.0
                            };
                            (clipped, v_max)
                        } else {
                            (*height, 1.0)
                        };

                        // Skip if fully clipped
                        if clipped_height <= 0.0 {
                            continue;
                        }

                        // Check if webkit texture is ready
                        if let Some(cached) = self.webkit_cache.get(*webkit_id) {
                            tracing::debug!(
                                "Rendering webkit {} at ({}, {}) size {}x{} (clipped to {})",
                                webkit_id,
                                x,
                                y,
                                width,
                                height,
                                clipped_height
                            );
                            // Create vertices for webkit quad (white color = no tinting)
                            let vertices = [
                                GlyphVertex {
                                    position: [*x, *y],
                                    tex_coords: [0.0, 0.0],
                                    color: [1.0, 1.0, 1.0, 1.0],
                                },
                                GlyphVertex {
                                    position: [*x + *width, *y],
                                    tex_coords: [1.0, 0.0],
                                    color: [1.0, 1.0, 1.0, 1.0],
                                },
                                GlyphVertex {
                                    position: [*x + *width, *y + clipped_height],
                                    tex_coords: [1.0, tex_v_max],
                                    color: [1.0, 1.0, 1.0, 1.0],
                                },
                                GlyphVertex {
                                    position: [*x, *y],
                                    tex_coords: [0.0, 0.0],
                                    color: [1.0, 1.0, 1.0, 1.0],
                                },
                                GlyphVertex {
                                    position: [*x + *width, *y + clipped_height],
                                    tex_coords: [1.0, tex_v_max],
                                    color: [1.0, 1.0, 1.0, 1.0],
                                },
                                GlyphVertex {
                                    position: [*x, *y + clipped_height],
                                    tex_coords: [0.0, tex_v_max],
                                    color: [1.0, 1.0, 1.0, 1.0],
                                },
                            ];

                            let webkit_buffer =
                                self.device
                                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                        label: Some("WebKit Vertex Buffer"),
                                        contents: bytemuck::cast_slice(&vertices),
                                        usage: wgpu::BufferUsages::VERTEX,
                                    });

                            render_pass.set_bind_group(1, &cached.bind_group, &[]);
                            render_pass.set_vertex_buffer(0, webkit_buffer.slice(..));
                            render_pass.draw(0..6, 0..1);
                        } else {
                            tracing::debug!("WebKit {} not found in cache", webkit_id);
                        }
                    }
                }
            }

            // Draw cursors and borders (after text)
            if !cursor_vertices.is_empty() {
                let cursor_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("Cursor Vertex Buffer"),
                            contents: bytemuck::cast_slice(&cursor_vertices),
                            usage: wgpu::BufferUsages::VERTEX,
                        });

                render_pass.set_pipeline(&self.rect_pipeline);
                render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                render_pass.set_vertex_buffer(0, cursor_buffer.slice(..));
                render_pass.draw(0..cursor_vertices.len() as u32, 0..1);
            }

            // === Draw scroll bar thumbs as filled rounded rects ===
            if !scroll_bar_thumb_vertices.is_empty() {
                let mut rounded_verts: Vec<RoundedRectVertex> = Vec::new();
                for (tx, ty, tw, th, radius, color) in &scroll_bar_thumb_vertices {
                    // border_width = 0 triggers filled mode in the shader
                    self.add_rounded_rect(
                        &mut rounded_verts,
                        *tx,
                        *ty,
                        *tw,
                        *th,
                        0.0,
                        *radius,
                        color,
                    );
                }
                let thumb_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("Scroll Bar Thumb Buffer"),
                            contents: bytemuck::cast_slice(&rounded_verts),
                            usage: wgpu::BufferUsages::VERTEX,
                        });
                render_pass.set_pipeline(&self.rounded_rect_pipeline);
                render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                render_pass.set_vertex_buffer(0, thumb_buffer.slice(..));
                render_pass.draw(0..rounded_verts.len() as u32, 0..1);
            }

            self.draw_post_content_effects(&mut render_pass, &ctx, faces);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
    }

    fn draw_pre_content_background_effects(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        ctx: &super::effect_common::EffectCtx<'_>,
        faces: &HashMap<u32, Face>,
        box_spans: &[BoxSpan],
    ) {
        // === Step 1a: Background pattern (dots/grid/crosshatch) ===
        draw_effect!(
            self,
            render_pass,
            "Background Pattern",
            super::pattern_effects::emit_background_pattern(&ctx)
        );

        // === Step 1b: Draw filled rounded rect backgrounds for ROUNDED boxed spans ===
        // Only for corner_radius > 0. Standard boxes use normal rect backgrounds.
        let mut box_fill_vertices: Vec<RoundedRectVertex> = Vec::new();
        for span in box_spans {
            if span.is_overlay {
                continue;
            }
            if let Some(ref bg_color) = span.bg {
                if let Some(face) = faces.get(&span.face_id) {
                    if face.box_corner_radius <= 0 {
                        continue;
                    }
                    let radius = (face.box_corner_radius as f32)
                        .min(span.height * 0.45)
                        .min(span.width * 0.45);
                    // Use a border_width larger than half the rect to fill solid
                    let fill_bw = span.height.max(span.width);
                    self.add_rounded_rect(
                        &mut box_fill_vertices,
                        span.x,
                        span.y,
                        span.width,
                        span.height,
                        fill_bw,
                        radius,
                        bg_color,
                    );
                }
            }
        }
        if !box_fill_vertices.is_empty() {
            let fill_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Box Fill Buffer"),
                    contents: bytemuck::cast_slice(&box_fill_vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            render_pass.set_pipeline(&self.rounded_rect_pipeline);
            render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            render_pass.set_vertex_buffer(0, fill_buffer.slice(..));
            render_pass.draw(0..box_fill_vertices.len() as u32, 0..1);
        }
    }

    fn draw_non_overlay_backgrounds(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        non_overlay_rect_vertices: &[RectVertex],
    ) {
        // === Step 1: Draw non-overlay backgrounds ===
        if !non_overlay_rect_vertices.is_empty() {
            let rect_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Non-overlay Rect Buffer"),
                    contents: bytemuck::cast_slice(non_overlay_rect_vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });

            render_pass.set_pipeline(&self.rect_pipeline);
            render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            render_pass.set_vertex_buffer(0, rect_buffer.slice(..));
            render_pass.draw(0..non_overlay_rect_vertices.len() as u32, 0..1);
        }
    }

    fn draw_pre_content_effects(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        ctx: &super::effect_common::EffectCtx<'_>,
    ) {
        self.draw_pre_content_effects_core(render_pass, ctx);
        self.draw_pre_content_effects_extended(render_pass, ctx);
    }

    fn draw_pre_content_effects_core(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        ctx: &super::effect_common::EffectCtx<'_>,
    ) {
        // === Step 1c: Cursor glow ===
        draw_effect!(
            self,
            render_pass,
            "Cursor Glow",
            super::cursor_effects::emit_cursor_glow(&ctx, &self.cursor_pulse_start)
        );

        // === Step 1d: Draw cursor crosshair guide lines ===
        draw_effect!(
            self,
            render_pass,
            "Cursor Crosshair",
            super::cursor_effects::emit_cursor_crosshair(&ctx)
        );

        // === Step 1e: Draw buffer modified border indicator ===
        draw_effect!(
            self,
            render_pass,
            "Modified Indicator",
            super::window_effects::emit_modified_indicator(&ctx)
        );

        // === Step 1f: Typing heat map overlay ===
        draw_stateful!(
            self,
            render_pass,
            "Heat Map",
            super::window_effects::emit_typing_heatmap(
                &ctx,
                &mut self.typing_heatmap_entries,
                &mut self.typing_heatmap_prev_cursor
            )
        );

        // === Step 1g: Per-window rounded border ===
        if self.effects.window_border_radius.enabled {
            let (wr, wg, wb) = self.effects.window_border_radius.color;
            let walpha = self.effects.window_border_radius.opacity;
            let wc = Color::new(wr, wg, wb, walpha);
            let radius = self.effects.window_border_radius.radius;
            let bw = self.effects.window_border_radius.width;
            let mut border_verts: Vec<RoundedRectVertex> = Vec::new();
            for win_info in &ctx.frame_glyphs.window_infos {
                if !win_info.is_minibuffer {
                    let wb_bounds = &win_info.bounds;
                    let mode_h = win_info.mode_line_height;
                    let content_h = wb_bounds.height - mode_h;
                    if content_h > 0.0 {
                        self.add_rounded_rect(
                            &mut border_verts,
                            wb_bounds.x,
                            wb_bounds.y,
                            wb_bounds.width,
                            content_h,
                            bw,
                            radius,
                            &wc,
                        );
                    }
                }
            }
            if !border_verts.is_empty() {
                let border_buf =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("Window Border Radius Buffer"),
                            contents: bytemuck::cast_slice(&border_verts),
                            usage: wgpu::BufferUsages::VERTEX,
                        });
                render_pass.set_pipeline(&self.rounded_rect_pipeline);
                render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                render_pass.set_vertex_buffer(0, border_buf.slice(..));
                render_pass.draw(0..border_verts.len() as u32, 0..1);
            }
        }

        // === Step 1h: Inactive window stained glass effect ===
        draw_effect!(
            self,
            render_pass,
            "Stained Glass",
            super::window_effects::emit_stained_glass(&ctx)
        );

        // === Step 1i_focus: Focus gradient border ===
        draw_effect!(
            self,
            render_pass,
            "Focus Gradient Border",
            super::window_effects::emit_focus_gradient_border(&ctx)
        );

        // === Step 1i_depth: Window depth shadow layers ===
        draw_effect!(
            self,
            render_pass,
            "Depth Shadow",
            super::window_effects::emit_window_depth_shadow(&ctx)
        );

        // === Step 1i_modeline_grad: Mode-line gradient background ===
        draw_effect!(
            self,
            render_pass,
            "Mode-line Gradient",
            super::window_effects::emit_mode_line_gradient(&ctx)
        );

        // === Step 1i_magnetism: Cursor magnetism effect ===
        draw_stateful!(
            self,
            render_pass,
            "Cursor Magnetism",
            super::cursor_effects::emit_cursor_magnetism(&ctx, &mut self.cursor_magnetism_entries)
        );

        // === Step 1i2: Window corner fold effect ===
        draw_effect!(
            self,
            render_pass,
            "Corner Fold",
            super::window_effects::emit_window_corner_fold(&ctx)
        );

        // === Step 1i2: Frosted window border effect ===
        draw_effect!(
            self,
            render_pass,
            "Frosted Border",
            super::window_effects::emit_frosted_window_border(&ctx)
        );

        // === Step 1i3: Line number pulse on cursor line ===
        draw_stateful!(
            self,
            render_pass,
            "Line Number Pulse",
            super::cursor_effects::emit_line_number_pulse(&ctx)
        );

        // === Step 1i4: Window breathing border animation ===
        draw_stateful!(
            self,
            render_pass,
            "Breathing Border",
            super::window_effects::emit_window_breathing_border(&ctx)
        );

        // === Step 1i5: Window scanline (CRT) effect ===
        draw_effect!(
            self,
            render_pass,
            "Scanlines",
            super::window_effects::emit_window_scanline(&ctx)
        );

        // === Step 1j: Cursor spotlight/radial gradient effect ===
        draw_effect!(
            self,
            render_pass,
            "Cursor Spotlight",
            super::cursor_effects::emit_cursor_spotlight(&ctx)
        );

        // === Step 1k: Cursor comet tail effect ===
        draw_stateful!(
            self,
            render_pass,
            "Cursor Comet",
            super::cursor_effects::emit_cursor_comet(&ctx, &mut self.cursor_comet_positions)
        );

        // === Step 1l: Cursor particle trail effect ===
        draw_stateful!(
            self,
            render_pass,
            "Cursor Particles",
            super::cursor_effects::emit_cursor_particles(
                &ctx,
                &mut self.cursor_particles,
                &mut self.cursor_particles_prev_pos
            )
        );

        // Matrix/digital rain effect
        draw_stateful!(
            self,
            render_pass,
            "Matrix Rain",
            super::cursor_effects::emit_matrix_rain(&ctx, &mut self.matrix_rain_columns)
        );

        // Frost/ice border effect
        draw_effect!(
            self,
            render_pass,
            "Frost Border",
            super::cursor_effects::emit_frost_border(&ctx)
        );

        // Cursor ghost afterimage effect
        draw_stateful!(
            self,
            render_pass,
            "Cursor Ghost",
            super::window_effects::emit_cursor_ghost(&ctx, &mut self.cursor_ghost_entries)
        );

        // Edge glow on scroll boundaries
        draw_stateful!(
            self,
            render_pass,
            "Edge Glow",
            super::window_effects::emit_edge_glow(&ctx, &mut self.edge_glow_entries)
        );

        // Rain/drip ambient effect
        draw_stateful!(
            self,
            render_pass,
            "Rain",
            super::window_effects::emit_rain_effect(&ctx, &mut self.rain_drops)
        );

        // Cursor ripple wave effect
        draw_stateful!(
            self,
            render_pass,
            "Cursor Ripple",
            super::cursor_effects::emit_cursor_ripple_wave(&ctx, &mut self.cursor_ripple_waves)
        );
    }

    fn draw_pre_content_effects_extended(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        ctx: &super::effect_common::EffectCtx<'_>,
    ) {
        self.draw_pre_content_effects_extended_part1(render_pass, ctx);
        self.draw_pre_content_effects_extended_part2(render_pass, ctx);
        self.draw_pre_content_effects_extended_part3(render_pass, ctx);
    }

    fn draw_pre_content_effects_extended_part1(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        ctx: &super::effect_common::EffectCtx<'_>,
    ) {
        // Aurora/northern lights effect
        draw_stateful!(
            self,
            render_pass,
            "Aurora",
            super::window_effects::emit_aurora_overlay(&ctx)
        );

        // === Heat distortion effect ===
        draw_effect!(
            self,
            render_pass,
            "Heat Distortion Buffer",
            super::pattern_effects::emit_heat_distortion(&ctx),
            continuous
        );

        // === Cursor lighthouse beam ===
        draw_effect!(
            self,
            render_pass,
            "Lighthouse Beam Buffer",
            super::cursor_effects::emit_cursor_lighthouse_beam(&ctx),
            continuous
        );

        // === Neon border effect ===
        draw_effect!(
            self,
            render_pass,
            "Neon Border Buffer",
            super::pattern_effects::emit_neon_border(&ctx),
            continuous
        );

        // === Cursor sonar ping effect ===
        draw_stateful!(
            self,
            render_pass,
            "Sonar Ping Buffer",
            super::cursor_effects::emit_cursor_sonar_ping(
                &ctx,
                &mut self.cursor_sonar_ping_entries
            )
        );

        // === Lightning bolt effect ===
        draw_stateful!(
            self,
            render_pass,
            "Lightning Bolt Buffer",
            super::cursor_effects::emit_lightning_bolt(
                &ctx,
                &mut self.lightning_bolt_last,
                &mut self.lightning_bolt_segments,
                &mut self.lightning_bolt_age
            )
        );

        // === Cursor orbit particles effect ===
        draw_effect!(
            self,
            render_pass,
            "Orbit Particles Buffer",
            super::cursor_effects::emit_cursor_orbit_particles(&ctx),
            continuous
        );

        // === Plasma border effect ===
        draw_effect!(
            self,
            render_pass,
            "Plasma Border Buffer",
            super::pattern_effects::emit_plasma_border(&ctx),
            continuous
        );

        // === Cursor heartbeat pulse effect ===
        draw_effect!(
            self,
            render_pass,
            "Heartbeat Pulse Buffer",
            super::cursor_effects::emit_cursor_heartbeat_pulse(&ctx),
            continuous
        );

        // === Topographic contour effect ===
        draw_effect!(
            self,
            render_pass,
            "Topo Contour Buffer",
            super::pattern_effects::emit_topographic_contour(&ctx),
            continuous
        );

        // === Cursor metronome tick effect ===
        draw_stateful!(
            self,
            render_pass,
            "Metronome Tick Buffer",
            super::cursor_effects::emit_cursor_metronome_tick(
                &ctx,
                &mut self.cursor_metronome_last_x,
                &mut self.cursor_metronome_last_y,
                &mut self.cursor_metronome_tick_start
            )
        );

        // === Constellation overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "Constellation Buffer",
            super::pattern_effects::emit_constellation(&ctx),
            continuous
        );

        // === Cursor radar sweep effect ===
        draw_effect!(
            self,
            render_pass,
            "Radar Sweep Buffer",
            super::cursor_effects::emit_cursor_radar_sweep(&ctx),
            continuous
        );

        // === Kaleidoscope overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "Kaleidoscope Buffer",
            super::pattern_effects::emit_kaleidoscope(&ctx),
            continuous
        );

        // === Cursor ripple ring effect ===
        draw_stateful!(
            self,
            render_pass,
            "Ripple Ring Buffer",
            super::cursor_effects::emit_cursor_ripple_ring(
                &ctx,
                &mut self.cursor_ripple_ring_start,
                &mut self.cursor_ripple_ring_last_x,
                &mut self.cursor_ripple_ring_last_y
            )
        );

        // === Noise field overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "Noise Field Buffer",
            super::pattern_effects::emit_noise_field(&ctx),
            continuous
        );

        // === Cursor scope effect ===
        draw_effect!(
            self,
            render_pass,
            "Cursor Scope Buffer",
            super::cursor_effects::emit_cursor_scope(&ctx),
            continuous
        );

        // === Spiral vortex overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "Spiral Vortex Buffer",
            super::pattern_effects::emit_spiral_vortex(&ctx),
            continuous
        );

        // === Cursor shockwave effect ===
        draw_stateful!(
            self,
            render_pass,
            "Shockwave Buffer",
            super::cursor_effects::emit_cursor_shockwave(
                &ctx,
                &mut self.cursor_shockwave_start,
                &mut self.cursor_shockwave_last_x,
                &mut self.cursor_shockwave_last_y
            )
        );

        // === Diamond lattice overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "Diamond Lattice Buffer",
            super::pattern_effects::emit_diamond_lattice(&ctx),
            continuous
        );

        // === Cursor gravity well effect ===
        draw_effect!(
            self,
            render_pass,
            "Gravity Well Buffer",
            super::cursor_effects::emit_cursor_gravity_well(&ctx),
            continuous
        );

        // === Wave interference overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "Wave Interference Buffer",
            super::pattern_effects::emit_wave_interference(&ctx),
            continuous
        );

        // === Cursor portal effect ===
        draw_effect!(
            self,
            render_pass,
            "Cursor Portal Buffer",
            super::cursor_effects::emit_cursor_portal(&ctx),
            continuous
        );

        // === Chevron pattern overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "Chevron Pattern Buffer",
            super::pattern_effects::emit_chevron(&ctx),
            continuous
        );

        // === Cursor bubble effect ===
        draw_stateful!(
            self,
            render_pass,
            "Cursor Bubble Buffer",
            super::cursor_effects::emit_cursor_bubble(
                &ctx,
                &mut self.cursor_bubble_spawn_time,
                &mut self.cursor_bubble_last_x,
                &mut self.cursor_bubble_last_y
            )
        );
    }

    fn draw_pre_content_effects_extended_part2(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        ctx: &super::effect_common::EffectCtx<'_>,
    ) {
        // === Sunburst pattern overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "sunburst_pattern_vb",
            super::pattern_effects::emit_sunburst(&ctx),
            continuous
        );

        // === Cursor firework effect ===
        draw_stateful!(
            self,
            render_pass,
            "cursor_firework_vb",
            super::cursor_effects::emit_cursor_firework(
                &ctx,
                &mut self.cursor_firework_start,
                &mut self.cursor_firework_last_x,
                &mut self.cursor_firework_last_y
            )
        );

        // === Honeycomb dissolve overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "honeycomb_dissolve_vb",
            super::pattern_effects::emit_honeycomb_dissolve(&ctx),
            continuous
        );

        // === Cursor tornado effect ===
        draw_effect!(
            self,
            render_pass,
            "cursor_tornado_vb",
            super::cursor_effects::emit_cursor_tornado(&ctx),
            continuous
        );

        // === Moiré pattern overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "moire_pattern_vb",
            super::pattern_effects::emit_moire(&ctx),
            continuous
        );

        // === Cursor lightning effect ===
        draw_stateful!(
            self,
            render_pass,
            "cursor_lightning_vb",
            super::cursor_effects::emit_cursor_lightning(
                &ctx,
                &mut self.cursor_lightning_start,
                &mut self.cursor_lightning_last_x,
                &mut self.cursor_lightning_last_y
            )
        );

        // === Dot matrix overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "dot_matrix_vb",
            super::pattern_effects::emit_dot_matrix(&ctx),
            continuous
        );

        // === Cursor snowflake effect ===
        draw_stateful!(
            self,
            render_pass,
            "cursor_snowflake_vb",
            super::cursor_effects::emit_cursor_snowflake(
                &ctx,
                &mut self.cursor_snowflake_start,
                &mut self.cursor_snowflake_last_x,
                &mut self.cursor_snowflake_last_y
            )
        );

        // === Concentric rings overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "concentric_rings_vb",
            super::pattern_effects::emit_concentric_rings(&ctx),
            continuous
        );

        // === Cursor flame effect ===
        draw_effect!(
            self,
            render_pass,
            "cursor_flame_vb",
            super::cursor_effects::emit_cursor_flame(&ctx),
            continuous
        );

        // === Zigzag pattern overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "zigzag_pattern_vb",
            super::pattern_effects::emit_zigzag(&ctx),
            continuous
        );

        // === Cursor crystal effect ===
        draw_effect!(
            self,
            render_pass,
            "cursor_crystal_vb",
            super::cursor_effects::emit_cursor_crystal(&ctx),
            continuous
        );

        // === Tessellation overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "tessellation_verts",
            super::pattern_effects::emit_tessellation(&ctx)
        );

        // === Cursor water drop effect ===
        draw_effect!(
            self,
            render_pass,
            "cursor_water_drop_verts",
            super::cursor_effects::emit_cursor_water_drop(&ctx),
            continuous
        );

        // === Guilloche overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "guilloche_verts",
            super::pattern_effects::emit_guilloche(&ctx),
            continuous
        );

        // === Cursor pixel dust effect ===
        draw_effect!(
            self,
            render_pass,
            "cursor_pixel_dust_verts",
            super::cursor_effects::emit_cursor_pixel_dust(&ctx),
            continuous
        );

        // === Celtic knot overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "celtic_knot_verts",
            super::pattern_effects::emit_celtic_knot(&ctx),
            continuous
        );

        // === Cursor candle flame effect ===
        draw_effect!(
            self,
            render_pass,
            "cursor_candle_flame_verts",
            super::cursor_effects::emit_cursor_candle_flame(&ctx),
            continuous
        );

        // === Argyle pattern overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "argyle_pattern_verts",
            super::pattern_effects::emit_argyle(&ctx)
        );

        // === Cursor moth flame effect ===
        draw_effect!(
            self,
            render_pass,
            "cursor_moth_flame_verts",
            super::cursor_effects::emit_cursor_moth_flame(&ctx),
            continuous
        );

        // === Basket weave overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "basket_weave_verts",
            super::pattern_effects::emit_basket_weave(&ctx)
        );

        // === Cursor sparkler effect ===
        draw_effect!(
            self,
            render_pass,
            "cursor_sparkler_verts",
            super::cursor_effects::emit_cursor_sparkler(&ctx),
            continuous
        );

        // === Fish scale overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "fish_scale_verts",
            super::pattern_effects::emit_fish_scale(&ctx)
        );

        // === Cursor plasma ball effect ===
        draw_effect!(
            self,
            render_pass,
            "cursor_plasma_ball_verts",
            super::cursor_effects::emit_cursor_plasma_ball(&ctx),
            continuous
        );

        // === Trefoil knot overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "trefoil_knot_verts",
            super::pattern_effects::emit_trefoil_knot(&ctx),
            continuous
        );

        // === Cursor quill pen effect ===
        draw_effect!(
            self,
            render_pass,
            "cursor_quill_pen_verts",
            super::cursor_effects::emit_cursor_quill_pen(&ctx),
            continuous
        );

        // === Herringbone pattern overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "herringbone_pattern_verts",
            super::pattern_effects::emit_herringbone(&ctx)
        );

        // === Cursor aurora borealis effect ===
        draw_effect!(
            self,
            render_pass,
            "cursor_aurora_borealis_verts",
            super::cursor_effects::emit_cursor_aurora_borealis(&ctx),
            continuous
        );

        // === Target reticle overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "target_reticle_verts",
            super::pattern_effects::emit_target_reticle(&ctx),
            continuous
        );

        // === Cursor feather effect ===
        draw_effect!(
            self,
            render_pass,
            "cursor_feather_verts",
            super::cursor_effects::emit_cursor_feather(&ctx),
            continuous
        );
    }

    fn draw_pre_content_effects_extended_part3(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        ctx: &super::effect_common::EffectCtx<'_>,
    ) {
        // === Plaid pattern overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "plaid_pattern_verts",
            super::pattern_effects::emit_plaid(&ctx)
        );

        // === Cursor stardust effect ===
        draw_effect!(
            self,
            render_pass,
            "cursor_stardust_verts",
            super::cursor_effects::emit_cursor_stardust(&ctx),
            continuous
        );

        // === Brick wall overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "brick_wall_verts",
            super::pattern_effects::emit_brick_wall(&ctx)
        );

        // === Cursor compass needle effect ===
        draw_effect!(
            self,
            render_pass,
            "cursor_compass_needle_verts",
            super::cursor_effects::emit_cursor_compass_needle(&ctx),
            continuous
        );

        // === Sine wave overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "sine_wave_verts",
            super::pattern_effects::emit_sine_wave(&ctx),
            continuous
        );

        // === Cursor galaxy effect ===
        draw_effect!(
            self,
            render_pass,
            "cursor_galaxy_verts",
            super::cursor_effects::emit_cursor_galaxy(&ctx),
            continuous
        );

        // === Rotating gear overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "rotating_gear_verts",
            super::pattern_effects::emit_rotating_gear(&ctx),
            continuous
        );

        // === Cursor prism effect ===
        draw_effect!(
            self,
            render_pass,
            "cursor_prism_verts",
            super::cursor_effects::emit_cursor_prism(&ctx),
            continuous
        );

        // === Crosshatch pattern overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "crosshatch_pattern_verts",
            super::pattern_effects::emit_crosshatch(&ctx),
            continuous
        );

        // === Cursor moth effect ===
        draw_effect!(
            self,
            render_pass,
            "cursor_moth_verts",
            super::cursor_effects::emit_cursor_moth(&ctx),
            continuous
        );

        // === Hex grid overlay effect ===
        draw_effect!(
            self,
            render_pass,
            "Hex Grid Buffer",
            super::pattern_effects::emit_hex_grid(&ctx),
            continuous
        );

        // === Cursor sparkle burst effect ===
        draw_stateful!(
            self,
            render_pass,
            "Sparkle Burst Buffer",
            super::cursor_effects::emit_cursor_sparkle_burst(
                &ctx,
                &mut self.cursor_sparkle_burst_entries
            )
        );

        // === Circuit board trace effect ===
        draw_effect!(
            self,
            render_pass,
            "Circuit Trace Buffer",
            super::pattern_effects::emit_circuit_board(&ctx),
            continuous
        );

        // === Cursor compass rose effect ===
        draw_effect!(
            self,
            render_pass,
            "Compass Rose Buffer",
            super::cursor_effects::emit_cursor_compass_rose(&ctx),
            continuous
        );

        // === Warp/distortion grid effect ===
        draw_effect!(
            self,
            render_pass,
            "Warp Grid Buffer",
            super::pattern_effects::emit_warp_grid(&ctx),
            continuous
        );

        // === Cursor DNA helix trail effect ===
        draw_effect!(
            self,
            render_pass,
            "DNA Helix Buffer",
            super::cursor_effects::emit_cursor_dna_helix(&ctx),
            continuous
        );

        // === Prism/rainbow edge effect ===
        draw_effect!(
            self,
            render_pass,
            "Prism Edge Buffer",
            super::pattern_effects::emit_prism_rainbow_edge(&ctx),
            continuous
        );

        // === Cursor pendulum swing effect ===
        draw_stateful!(
            self,
            render_pass,
            "Pendulum Buffer",
            super::cursor_effects::emit_cursor_pendulum(
                &ctx,
                &mut self.cursor_pendulum_last_x,
                &mut self.cursor_pendulum_last_y,
                &mut self.cursor_pendulum_swing_start
            )
        );

        // === Cursor drop shadow (drawn before cursor bg) ===
        draw_effect!(
            self,
            render_pass,
            "Cursor Shadow Buffer",
            super::cursor_effects::emit_cursor_drop_shadow(&ctx)
        );
    }

    fn draw_pre_text_cursor_layers(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        cursor_bg_vertices: &[RectVertex],
        behind_text_cursor_vertices: &[RectVertex],
    ) {
        // === Step 2: Draw cursor bg rect (inverse video background) ===
        // Drawn after window/char backgrounds but before text, so the cursor
        // background color is visible behind the inverse-video character.
        if !cursor_bg_vertices.is_empty() {
            let cursor_bg_buffer =
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Cursor BG Rect Buffer"),
                        contents: bytemuck::cast_slice(cursor_bg_vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    });

            render_pass.set_pipeline(&self.rect_pipeline);
            render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            render_pass.set_vertex_buffer(0, cursor_bg_buffer.slice(..));
            render_pass.draw(0..cursor_bg_vertices.len() as u32, 0..1);
        }

        // === Step 3: Draw animated cursor trail behind text ===
        // The spring trail or animated rect for filled box cursor appears
        // behind text so characters remain readable during cursor motion.
        if !behind_text_cursor_vertices.is_empty() {
            let trail_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Behind-Text Cursor Buffer"),
                    contents: bytemuck::cast_slice(behind_text_cursor_vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });

            render_pass.set_pipeline(&self.rect_pipeline);
            render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            render_pass.set_vertex_buffer(0, trail_buffer.slice(..));
            render_pass.draw(0..behind_text_cursor_vertices.len() as u32, 0..1);
        }
    }

    fn draw_inline_media(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        frame_glyphs: &FrameGlyphBuffer,
        overlay_y: Option<f32>,
    ) {
        // Draw inline images
        render_pass.set_pipeline(&self.image_pipeline);
        render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);

        for glyph in &frame_glyphs.glyphs {
            if let FrameGlyph::Image {
                image_id,
                x,
                y,
                width,
                height,
            } = glyph
            {
                // Clip to mode-line boundary if needed
                let (clipped_height, tex_v_max) = if let Some(oy) = overlay_y {
                    if *y + *height > oy {
                        let clipped = (oy - *y).max(0.0);
                        let v_max = if *height > 0.0 {
                            clipped / *height
                        } else {
                            1.0
                        };
                        (clipped, v_max)
                    } else {
                        (*height, 1.0)
                    }
                } else {
                    (*height, 1.0)
                };

                // Skip if fully clipped
                if clipped_height <= 0.0 {
                    continue;
                }

                tracing::debug!(
                    "Rendering image {} at ({}, {}) size {}x{} (clipped to {})",
                    image_id,
                    x,
                    y,
                    width,
                    height,
                    clipped_height
                );
                // Check if image texture is ready
                if let Some(cached) = self.image_cache.get(*image_id) {
                    // Create vertices for image quad (white color = no tinting)
                    let vertices = [
                        GlyphVertex {
                            position: [*x, *y],
                            tex_coords: [0.0, 0.0],
                            color: [1.0, 1.0, 1.0, 1.0],
                        },
                        GlyphVertex {
                            position: [*x + *width, *y],
                            tex_coords: [1.0, 0.0],
                            color: [1.0, 1.0, 1.0, 1.0],
                        },
                        GlyphVertex {
                            position: [*x + *width, *y + clipped_height],
                            tex_coords: [1.0, tex_v_max],
                            color: [1.0, 1.0, 1.0, 1.0],
                        },
                        GlyphVertex {
                            position: [*x, *y],
                            tex_coords: [0.0, 0.0],
                            color: [1.0, 1.0, 1.0, 1.0],
                        },
                        GlyphVertex {
                            position: [*x + *width, *y + clipped_height],
                            tex_coords: [1.0, tex_v_max],
                            color: [1.0, 1.0, 1.0, 1.0],
                        },
                        GlyphVertex {
                            position: [*x, *y + clipped_height],
                            tex_coords: [0.0, tex_v_max],
                            color: [1.0, 1.0, 1.0, 1.0],
                        },
                    ];

                    let image_buffer =
                        self.device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("Image Vertex Buffer"),
                                contents: bytemuck::cast_slice(&vertices),
                                usage: wgpu::BufferUsages::VERTEX,
                            });

                    render_pass.set_bind_group(1, &cached.bind_group, &[]);
                    render_pass.set_vertex_buffer(0, image_buffer.slice(..));
                    render_pass.draw(0..6, 0..1);
                }
            }
        }

        // Apply video loop_count and autoplay before rendering
        #[cfg(feature = "video")]
        for glyph in &frame_glyphs.glyphs {
            if let FrameGlyph::Video {
                video_id,
                loop_count,
                autoplay,
                ..
            } = glyph
            {
                if *loop_count != 0 {
                    self.video_cache.set_loop(*video_id, *loop_count);
                }
                if *autoplay {
                    let state = self.video_cache.get_state(*video_id);
                    if matches!(
                        state,
                        Some(super::super::VideoState::Stopped)
                            | Some(super::super::VideoState::Loading)
                    ) {
                        self.video_cache.play(*video_id);
                    }
                }
            }
        }

        // Draw inline videos
        #[cfg(feature = "video")]
        for glyph in &frame_glyphs.glyphs {
            if let FrameGlyph::Video {
                video_id,
                x,
                y,
                width,
                height,
                ..
            } = glyph
            {
                // Clip to mode-line boundary if needed
                let (clipped_height, tex_v_max) = if let Some(oy) = overlay_y {
                    if *y + *height > oy {
                        let clipped = (oy - *y).max(0.0);
                        let v_max = if *height > 0.0 {
                            clipped / *height
                        } else {
                            1.0
                        };
                        (clipped, v_max)
                    } else {
                        (*height, 1.0)
                    }
                } else {
                    (*height, 1.0)
                };

                // Skip if fully clipped
                if clipped_height <= 0.0 {
                    continue;
                }

                // Check if video texture is ready
                if let Some(cached) = self.video_cache.get(*video_id) {
                    tracing::trace!(
                        "Rendering video {} at ({}, {}) size {}x{} (clipped to {}), frame_count={}",
                        video_id,
                        x,
                        y,
                        width,
                        height,
                        clipped_height,
                        cached.frame_count
                    );
                    if let Some(ref bind_group) = cached.bind_group {
                        // Create vertices for video quad (white color = no tinting)
                        let vertices = [
                            GlyphVertex {
                                position: [*x, *y],
                                tex_coords: [0.0, 0.0],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [*x + *width, *y],
                                tex_coords: [1.0, 0.0],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [*x + *width, *y + clipped_height],
                                tex_coords: [1.0, tex_v_max],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [*x, *y],
                                tex_coords: [0.0, 0.0],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [*x + *width, *y + clipped_height],
                                tex_coords: [1.0, tex_v_max],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [*x, *y + clipped_height],
                                tex_coords: [0.0, tex_v_max],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                        ];

                        let video_buffer =
                            self.device
                                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                    label: Some("Video Vertex Buffer"),
                                    contents: bytemuck::cast_slice(&vertices),
                                    usage: wgpu::BufferUsages::VERTEX,
                                });

                        render_pass.set_bind_group(1, bind_group, &[]);
                        render_pass.set_vertex_buffer(0, video_buffer.slice(..));
                        render_pass.draw(0..6, 0..1);
                    } else {
                        tracing::warn!("Video {} has no bind_group!", video_id);
                    }
                } else {
                    tracing::warn!("Video {} not found in cache!", video_id);
                }
            }
        }

        // Draw inline webkit views (use opaque pipeline — DMA-BUF XRGB has alpha=0)
        #[cfg(feature = "wpe-webkit")]
        {
            render_pass.set_pipeline(&self.opaque_image_pipeline);
            render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);

            for glyph in &frame_glyphs.glyphs {
                if let FrameGlyph::WebKit {
                    webkit_id,
                    x,
                    y,
                    width,
                    height,
                } = glyph
                {
                    // Clip to mode-line boundary if needed
                    tracing::trace!(
                        "WebKit clip check: webkit {} at y={}, height={}, y+h={}, overlay_y={:?}",
                        webkit_id,
                        y,
                        height,
                        y + height,
                        overlay_y
                    );
                    let (clipped_height, tex_v_max) = if let Some(oy) = overlay_y {
                        if *y + *height > oy {
                            let clipped = (oy - *y).max(0.0);
                            let v_max = if *height > 0.0 {
                                clipped / *height
                            } else {
                                1.0
                            };
                            tracing::trace!(
                                "WebKit {} clipped: y={} + h={} > overlay_y={}, clipped_height={}",
                                webkit_id,
                                y,
                                height,
                                oy,
                                clipped
                            );
                            (clipped, v_max)
                        } else {
                            (*height, 1.0)
                        }
                    } else {
                        (*height, 1.0)
                    };

                    // Skip if fully clipped
                    if clipped_height <= 0.0 {
                        continue;
                    }

                    // Check if webkit texture is ready
                    if let Some(cached) = self.webkit_cache.get(*webkit_id) {
                        tracing::debug!(
                            "Rendering webkit {} at ({}, {}) size {}x{} (clipped to {})",
                            webkit_id,
                            x,
                            y,
                            width,
                            height,
                            clipped_height
                        );
                        // Create vertices for webkit quad (white color = no tinting)
                        let vertices = [
                            GlyphVertex {
                                position: [*x, *y],
                                tex_coords: [0.0, 0.0],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [*x + *width, *y],
                                tex_coords: [1.0, 0.0],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [*x + *width, *y + clipped_height],
                                tex_coords: [1.0, tex_v_max],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [*x, *y],
                                tex_coords: [0.0, 0.0],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [*x + *width, *y + clipped_height],
                                tex_coords: [1.0, tex_v_max],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [*x, *y + clipped_height],
                                tex_coords: [0.0, tex_v_max],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                        ];

                        let webkit_buffer =
                            self.device
                                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                    label: Some("WebKit Vertex Buffer"),
                                    contents: bytemuck::cast_slice(&vertices),
                                    usage: wgpu::BufferUsages::VERTEX,
                                });

                        render_pass.set_bind_group(1, &cached.bind_group, &[]);
                        render_pass.set_vertex_buffer(0, webkit_buffer.slice(..));
                        render_pass.draw(0..6, 0..1);
                    } else {
                        tracing::debug!("WebKit {} not found in cache", webkit_id);
                    }
                }
            }
        }
    }

    fn draw_text_and_overlay_layers(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        frame_glyphs: &FrameGlyphBuffer,
        glyph_atlas: &mut WgpuGlyphAtlas,
        faces: &HashMap<u32, Face>,
        box_spans: &[BoxSpan],
        overlay_rect_vertices: &[RectVertex],
        has_line_anims: bool,
        cursor_visible: bool,
        logical_w: f32,
    ) {
        // === Steps 4-6: Draw text and overlay in correct z-order ===
        // For each overlay pass:
        //   Pass 0 (non-overlay): draw buffer text (with cursor fg swap for inverse video)
        //   Pass 1 (overlay): draw overlay backgrounds first, then overlay text
        //
        // This ensures: non-overlay bg → cursor bg → trail → text → overlay bg → overlay text

        for overlay_pass in 0..2 {
            let want_overlay = overlay_pass == 1;
            self.draw_text_overlay_pass(
                render_pass,
                frame_glyphs,
                glyph_atlas,
                faces,
                box_spans,
                overlay_rect_vertices,
                has_line_anims,
                cursor_visible,
                logical_w,
                want_overlay,
            );
        }
    }

    fn draw_text_overlay_pass(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        frame_glyphs: &FrameGlyphBuffer,
        glyph_atlas: &mut WgpuGlyphAtlas,
        faces: &HashMap<u32, Face>,
        box_spans: &[BoxSpan],
        overlay_rect_vertices: &[RectVertex],
        has_line_anims: bool,
        cursor_visible: bool,
        logical_w: f32,
        want_overlay: bool,
    ) {
        self.draw_overlay_backgrounds(
            render_pass,
            faces,
            box_spans,
            overlay_rect_vertices,
            want_overlay,
        );

        let mut mask_data: Vec<(GlyphKey, [GlyphVertex; 6])> = Vec::new();
        let mut color_data: Vec<(GlyphKey, [GlyphVertex; 6])> = Vec::new();
        // Composed glyphs rendered individually (each is unique, no batching)
        let mut composed_mask_data: Vec<(ComposedGlyphKey, [GlyphVertex; 6])> = Vec::new();
        let mut composed_color_data: Vec<(ComposedGlyphKey, [GlyphVertex; 6])> = Vec::new();

        self.collect_overlay_pass_glyph_data(
            frame_glyphs,
            glyph_atlas,
            faces,
            has_line_anims,
            cursor_visible,
            want_overlay,
            &mut mask_data,
            &mut color_data,
            &mut composed_mask_data,
            &mut composed_color_data,
        );

        self.draw_overlay_pass_batches_and_decorations(
            render_pass,
            glyph_atlas,
            frame_glyphs,
            faces,
            box_spans,
            has_line_anims,
            want_overlay,
            logical_w,
            &mut mask_data,
            &mut color_data,
            &composed_mask_data,
            &composed_color_data,
        );
    }

    fn draw_overlay_pass_batches_and_decorations(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        glyph_atlas: &mut WgpuGlyphAtlas,
        frame_glyphs: &FrameGlyphBuffer,
        faces: &HashMap<u32, Face>,
        box_spans: &[BoxSpan],
        has_line_anims: bool,
        want_overlay: bool,
        logical_w: f32,
        mask_data: &mut Vec<(GlyphKey, [GlyphVertex; 6])>,
        color_data: &mut Vec<(GlyphKey, [GlyphVertex; 6])>,
        composed_mask_data: &[(ComposedGlyphKey, [GlyphVertex; 6])],
        composed_color_data: &[(ComposedGlyphKey, [GlyphVertex; 6])],
    ) {
        self.log_overlay_pass_batch_debug(mask_data, color_data, want_overlay, logical_w);

        self.draw_mask_glyph_batch(render_pass, glyph_atlas, mask_data);
        self.draw_color_glyph_batch(render_pass, glyph_atlas, color_data);
        self.draw_composed_mask_glyphs(render_pass, glyph_atlas, &composed_mask_data);
        self.draw_composed_color_glyphs(render_pass, glyph_atlas, &composed_color_data);

        self.draw_text_decorations_and_borders(
            render_pass,
            frame_glyphs,
            faces,
            box_spans,
            has_line_anims,
            want_overlay,
        );
    }

    fn collect_overlay_pass_glyph_data(
        &mut self,
        frame_glyphs: &FrameGlyphBuffer,
        glyph_atlas: &mut WgpuGlyphAtlas,
        faces: &HashMap<u32, Face>,
        has_line_anims: bool,
        cursor_visible: bool,
        want_overlay: bool,
        mask_data: &mut Vec<(GlyphKey, [GlyphVertex; 6])>,
        color_data: &mut Vec<(GlyphKey, [GlyphVertex; 6])>,
        composed_mask_data: &mut Vec<(ComposedGlyphKey, [GlyphVertex; 6])>,
        composed_color_data: &mut Vec<(ComposedGlyphKey, [GlyphVertex; 6])>,
    ) {
        for glyph in &frame_glyphs.glyphs {
            if let FrameGlyph::Char {
                char,
                composed,
                x,
                y,
                baseline,
                width,
                ascent,
                fg,
                face_id,
                font_size,
                is_overlay,
                overstrike,
                ..
            } = glyph
            {
                if *is_overlay != want_overlay {
                    continue;
                }
                self.collect_overlay_char_glyph(
                    frame_glyphs,
                    glyph_atlas,
                    faces,
                    has_line_anims,
                    cursor_visible,
                    want_overlay,
                    *char,
                    composed.as_deref(),
                    *x,
                    *y,
                    *baseline,
                    *width,
                    *ascent,
                    fg,
                    *face_id,
                    *font_size,
                    *overstrike,
                    mask_data,
                    color_data,
                    composed_mask_data,
                    composed_color_data,
                );
            }
        }
    }

    fn log_overlay_pass_batch_debug(
        &self,
        mask_data: &[(GlyphKey, [GlyphVertex; 6])],
        color_data: &[(GlyphKey, [GlyphVertex; 6])],
        want_overlay: bool,
        logical_w: f32,
    ) {
        tracing::trace!(
            "render_frame_glyphs: overlay={} {} mask glyphs, {} color glyphs",
            want_overlay,
            mask_data.len(),
            color_data.len()
        );
        // Debug: dump first few glyph positions.
        if !mask_data.is_empty() && !want_overlay {
            for (i, (key, verts)) in mask_data.iter().take(3).enumerate() {
                let p0 = verts[0].position;
                let c0 = verts[0].color;
                tracing::debug!(
                    "  glyph[{}]: charcode={} pos=({:.1},{:.1}) color=({:.3},{:.3},{:.3},{:.3}) logical_w={:.1}",
                    i,
                    key.charcode,
                    p0[0],
                    p0[1],
                    c0[0],
                    c0[1],
                    c0[2],
                    c0[3],
                    logical_w
                );
            }
        }
    }

    fn collect_overlay_char_glyph(
        &mut self,
        frame_glyphs: &FrameGlyphBuffer,
        glyph_atlas: &mut WgpuGlyphAtlas,
        faces: &HashMap<u32, Face>,
        has_line_anims: bool,
        cursor_visible: bool,
        want_overlay: bool,
        char_code: char,
        composed_text: Option<&str>,
        x: f32,
        y: f32,
        baseline: f32,
        width: f32,
        ascent: f32,
        fg: &Color,
        face_id: u32,
        font_size: f32,
        overstrike: bool,
        mask_data: &mut Vec<(GlyphKey, [GlyphVertex; 6])>,
        color_data: &mut Vec<(GlyphKey, [GlyphVertex; 6])>,
        composed_mask_data: &mut Vec<(ComposedGlyphKey, [GlyphVertex; 6])>,
        composed_color_data: &mut Vec<(ComposedGlyphKey, [GlyphVertex; 6])>,
    ) {
        let face = faces.get(&face_id);

        // Decompose physical-pixel positions into integer + subpixel bin.
        // The bin is baked into the rasterized bitmap by swash for subpixel
        // accuracy; vertex positions stay on integer pixels (no Linear blur).
        let sf = self.scale_factor;
        let y_offset = if has_line_anims {
            self.line_y_offset(x, y)
        } else {
            0.0
        };
        let phys_x = x * sf;
        let baseline_y = baseline + y_offset;
        let phys_y = baseline_y * sf;
        let (x_int, x_bin) = SubpixelBin::new(phys_x);
        let (y_int, y_bin) = SubpixelBin::new(phys_y);

        let cached = self.lookup_overlay_cached_glyph(
            glyph_atlas,
            face,
            composed_text,
            char_code,
            face_id,
            font_size.to_bits(),
            x_bin,
            y_bin,
        );

        if let Some(cached) = cached {
            // Vertex positions from integer physical pixels + bearing,
            // converted back to logical pixels.
            let glyph_x = (x_int as f32 + cached.bearing_x) / sf;
            let glyph_y = (y_int as f32 - cached.bearing_y) / sf;
            let glyph_w = cached.width as f32 / sf;
            let glyph_h = cached.height as f32 / sf;

            let effective_fg =
                self.resolve_effective_foreground(frame_glyphs, cursor_visible, x, y, fg);
            let color = self.resolve_glyph_vertex_color(cached.is_color, effective_fg, x, y);

            self.log_overlay_char_debug(
                want_overlay,
                composed_text,
                char_code,
                face_id,
                glyph_x,
                glyph_y,
                glyph_w,
                glyph_h,
                ascent,
                color,
                cached.is_color,
                x,
                y,
                width,
            );

            let vertices =
                self.build_glyph_quad_vertices(glyph_x, glyph_y, glyph_w, glyph_h, color);
            let overstrike_vertices = self.build_overstrike_glyph_vertices(
                overstrike, glyph_x, glyph_y, glyph_w, glyph_h, color,
            );

            self.append_glyph_vertices_to_batches(
                composed_text,
                char_code,
                face_id,
                font_size.to_bits(),
                x_bin,
                y_bin,
                cached.is_color,
                vertices,
                overstrike_vertices,
                mask_data,
                color_data,
                composed_mask_data,
                composed_color_data,
            );
        }
    }

    fn lookup_overlay_cached_glyph<'a>(
        &self,
        glyph_atlas: &'a mut WgpuGlyphAtlas,
        face: Option<&Face>,
        composed_text: Option<&str>,
        char_code: char,
        face_id: u32,
        font_size_bits: u32,
        x_bin: SubpixelBin,
        y_bin: SubpixelBin,
    ) -> Option<&'a CachedGlyph> {
        if let Some(text) = composed_text {
            // Composed grapheme cluster (emoji ZWJ, combining marks, etc.).
            glyph_atlas.get_or_create_composed(
                &self.device,
                &self.queue,
                text,
                face_id,
                font_size_bits,
                face,
                x_bin,
                y_bin,
            )
        } else {
            // Single character.
            let key = GlyphKey {
                charcode: char_code as u32,
                face_id,
                font_size_bits,
                x_bin,
                y_bin,
            };
            glyph_atlas.get_or_create(&self.device, &self.queue, &key, face)
        }
    }

    fn resolve_effective_foreground<'a>(
        &self,
        frame_glyphs: &'a FrameGlyphBuffer,
        cursor_visible: bool,
        x: f32,
        y: f32,
        fg: &'a Color,
    ) -> &'a Color {
        // For the character under a filled box cursor, swap to cursor_fg
        // (inverse video) when cursor is visible.
        if !cursor_visible {
            return fg;
        }
        let Some(ref inv) = frame_glyphs.cursor_inverse else {
            return fg;
        };
        if (x - inv.x).abs() < 1.0 && (y - inv.y).abs() < 1.0 {
            &inv.cursor_fg
        } else {
            fg
        }
    }

    fn resolve_glyph_vertex_color(
        &self,
        is_color_glyph: bool,
        effective_fg: &Color,
        x: f32,
        y: f32,
    ) -> [f32; 4] {
        // Color glyphs use white vertex color (no tinting),
        // mask glyphs use foreground color for tinting.
        let fade_alpha = self.text_fade_alpha(x, y) * self.mode_line_fade_alpha(x, y);
        if is_color_glyph {
            [1.0, 1.0, 1.0, fade_alpha]
        } else {
            [
                effective_fg.r,
                effective_fg.g,
                effective_fg.b,
                effective_fg.a * fade_alpha,
            ]
        }
    }

    fn build_glyph_quad_vertices(
        &self,
        glyph_x: f32,
        glyph_y: f32,
        glyph_w: f32,
        glyph_h: f32,
        color: [f32; 4],
    ) -> [GlyphVertex; 6] {
        [
            GlyphVertex {
                position: [glyph_x, glyph_y],
                tex_coords: [0.0, 0.0],
                color,
            },
            GlyphVertex {
                position: [glyph_x + glyph_w, glyph_y],
                tex_coords: [1.0, 0.0],
                color,
            },
            GlyphVertex {
                position: [glyph_x + glyph_w, glyph_y + glyph_h],
                tex_coords: [1.0, 1.0],
                color,
            },
            GlyphVertex {
                position: [glyph_x, glyph_y],
                tex_coords: [0.0, 0.0],
                color,
            },
            GlyphVertex {
                position: [glyph_x + glyph_w, glyph_y + glyph_h],
                tex_coords: [1.0, 1.0],
                color,
            },
            GlyphVertex {
                position: [glyph_x, glyph_y + glyph_h],
                tex_coords: [0.0, 1.0],
                color,
            },
        ]
    }

    fn build_overstrike_glyph_vertices(
        &self,
        overstrike: bool,
        glyph_x: f32,
        glyph_y: f32,
        glyph_w: f32,
        glyph_h: f32,
        color: [f32; 4],
    ) -> Option<[GlyphVertex; 6]> {
        // Overstrike: simulate bold by drawing the glyph a second time shifted
        // 1px right. This matches official Emacs behavior when a bold font
        // variant is unavailable.
        if !overstrike {
            return None;
        }
        let ox = 1.0 / self.scale_factor;
        Some(self.build_glyph_quad_vertices(glyph_x + ox, glyph_y, glyph_w, glyph_h, color))
    }

    fn append_glyph_vertices_to_batches(
        &mut self,
        composed_text: Option<&str>,
        char_code: char,
        face_id: u32,
        font_size_bits: u32,
        x_bin: SubpixelBin,
        y_bin: SubpixelBin,
        is_color: bool,
        vertices: [GlyphVertex; 6],
        overstrike_vertices: Option<[GlyphVertex; 6]>,
        mask_data: &mut Vec<(GlyphKey, [GlyphVertex; 6])>,
        color_data: &mut Vec<(GlyphKey, [GlyphVertex; 6])>,
        composed_mask_data: &mut Vec<(ComposedGlyphKey, [GlyphVertex; 6])>,
        composed_color_data: &mut Vec<(ComposedGlyphKey, [GlyphVertex; 6])>,
    ) {
        if let Some(text) = composed_text {
            let ckey = ComposedGlyphKey {
                text: text.into(),
                face_id,
                font_size_bits,
                x_bin,
                y_bin,
            };
            if is_color {
                composed_color_data.push((ckey.clone(), vertices));
                if let Some(ov) = overstrike_vertices {
                    composed_color_data.push((ckey, ov));
                }
            } else {
                composed_mask_data.push((ckey.clone(), vertices));
                if let Some(ov) = overstrike_vertices {
                    composed_mask_data.push((ckey, ov));
                }
            }
        } else {
            let key = GlyphKey {
                charcode: char_code as u32,
                face_id,
                font_size_bits,
                x_bin,
                y_bin,
            };
            if is_color {
                color_data.push((key.clone(), vertices));
                if let Some(ov) = overstrike_vertices {
                    color_data.push((key, ov));
                }
            } else {
                mask_data.push((key.clone(), vertices));
                if let Some(ov) = overstrike_vertices {
                    mask_data.push((key, ov));
                }
            }
        }
    }

    fn log_overlay_char_debug(
        &self,
        want_overlay: bool,
        composed_text: Option<&str>,
        char_code: char,
        face_id: u32,
        glyph_x: f32,
        glyph_y: f32,
        glyph_w: f32,
        glyph_h: f32,
        ascent: f32,
        color: [f32; 4],
        is_color: bool,
        x: f32,
        y: f32,
        width: f32,
    ) {
        // Debug: log glyphs near y≈27 (where gray line appears in screenshot)
        // and first few header glyphs (y < 5) to see row start
        if !want_overlay && (glyph_y + glyph_h > 24.0 && glyph_y < 32.0) {
            tracing::debug!(
                "glyph_near_y27: char='{}' face={} pos=({:.1},{:.1}) size=({:.1},{:.1}) ascent={:.1} bottom={:.1} fg=({:.3},{:.3},{:.3},{:.3}) is_color={} cell=({:.1},{:.1},{:.1})",
                if let Some(text) = composed_text {
                    text.to_string()
                } else {
                    format!("{}", char_code as u8 as char)
                },
                face_id,
                glyph_x,
                glyph_y,
                glyph_w,
                glyph_h,
                ascent,
                glyph_y + glyph_h,
                color[0],
                color[1],
                color[2],
                color[3],
                is_color,
                x,
                y,
                width,
            );
        }
        if !want_overlay && y < 1.0 {
            tracing::debug!(
                "first_row_glyph: char='{}' face={} cell=({:.1},{:.1},{:.1}) glyph_pos=({:.1},{:.1}) glyph_size=({:.1},{:.1}) ascent={:.1} fg=({:.3},{:.3},{:.3})",
                if let Some(text) = composed_text {
                    text.to_string()
                } else {
                    format!("{}", char_code as u8 as char)
                },
                face_id,
                x,
                y,
                width,
                glyph_x,
                glyph_y,
                glyph_w,
                glyph_h,
                ascent,
                color[0],
                color[1],
                color[2],
            );
        }
    }

    fn draw_mask_glyph_batch(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        glyph_atlas: &WgpuGlyphAtlas,
        mask_data: &mut Vec<(GlyphKey, [GlyphVertex; 6])>,
    ) {
        // Draw mask glyphs with glyph pipeline (alpha tinted with foreground)
        // Sort by GlyphKey so identical characters batch into single draw calls,
        // significantly reducing GPU state changes (set_bind_group calls).
        if mask_data.is_empty() {
            return;
        }

        mask_data.sort_by(|(a, _), (b, _)| {
            a.face_id
                .cmp(&b.face_id)
                .then(a.font_size_bits.cmp(&b.font_size_bits))
                .then(a.charcode.cmp(&b.charcode))
        });

        render_pass.set_pipeline(&self.glyph_pipeline);
        render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);

        let all_vertices: Vec<GlyphVertex> = mask_data
            .iter()
            .flat_map(|(_, verts)| verts.iter().copied())
            .collect();

        let glyph_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Glyph Vertex Buffer"),
                contents: bytemuck::cast_slice(&all_vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });

        render_pass.set_vertex_buffer(0, glyph_buffer.slice(..));

        // Batch consecutive glyphs sharing the same texture.
        let mut i = 0;
        while i < mask_data.len() {
            let (ref key, _) = mask_data[i];
            if let Some(cached) = glyph_atlas.get(key) {
                let batch_start = i;
                i += 1;
                while i < mask_data.len() && mask_data[i].0 == *key {
                    i += 1;
                }
                let vert_start = (batch_start * 6) as u32;
                let vert_end = (i * 6) as u32;
                render_pass.set_bind_group(1, &cached.bind_group, &[]);
                render_pass.draw(vert_start..vert_end, 0..1);
            } else {
                i += 1;
            }
        }
    }

    fn draw_color_glyph_batch(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        glyph_atlas: &WgpuGlyphAtlas,
        color_data: &mut Vec<(GlyphKey, [GlyphVertex; 6])>,
    ) {
        // Draw color glyphs with image pipeline (direct RGBA, e.g. color emoji).
        if color_data.is_empty() {
            return;
        }

        color_data.sort_by(|(a, _), (b, _)| {
            a.face_id
                .cmp(&b.face_id)
                .then(a.font_size_bits.cmp(&b.font_size_bits))
                .then(a.charcode.cmp(&b.charcode))
        });

        render_pass.set_pipeline(&self.image_pipeline);
        render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);

        let all_vertices: Vec<GlyphVertex> = color_data
            .iter()
            .flat_map(|(_, verts)| verts.iter().copied())
            .collect();

        let color_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Color Glyph Vertex Buffer"),
                contents: bytemuck::cast_slice(&all_vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });

        render_pass.set_vertex_buffer(0, color_buffer.slice(..));

        // Batch consecutive color glyphs sharing the same texture.
        let mut i = 0;
        while i < color_data.len() {
            let (ref key, _) = color_data[i];
            if let Some(cached) = glyph_atlas.get(key) {
                let batch_start = i;
                i += 1;
                while i < color_data.len() && color_data[i].0 == *key {
                    i += 1;
                }
                let vert_start = (batch_start * 6) as u32;
                let vert_end = (i * 6) as u32;
                render_pass.set_bind_group(1, &cached.bind_group, &[]);
                render_pass.draw(vert_start..vert_end, 0..1);
            } else {
                i += 1;
            }
        }
    }

    fn draw_composed_mask_glyphs(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        glyph_atlas: &WgpuGlyphAtlas,
        composed_mask_data: &[(ComposedGlyphKey, [GlyphVertex; 6])],
    ) {
        // Draw composed mask glyphs (each unique, no batching).
        if composed_mask_data.is_empty() {
            return;
        }

        render_pass.set_pipeline(&self.glyph_pipeline);
        render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);

        for (ckey, verts) in composed_mask_data {
            if let Some(cached) = glyph_atlas.get_composed(ckey) {
                let vbuf = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Composed Glyph VB"),
                        contents: bytemuck::cast_slice(verts),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                render_pass.set_vertex_buffer(0, vbuf.slice(..));
                render_pass.set_bind_group(1, &cached.bind_group, &[]);
                render_pass.draw(0..6, 0..1);
            }
        }
    }

    fn draw_composed_color_glyphs(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        glyph_atlas: &WgpuGlyphAtlas,
        composed_color_data: &[(ComposedGlyphKey, [GlyphVertex; 6])],
    ) {
        // Draw composed color glyphs (emoji ZWJ sequences, etc.).
        if composed_color_data.is_empty() {
            return;
        }

        render_pass.set_pipeline(&self.image_pipeline);
        render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);

        for (ckey, verts) in composed_color_data {
            if let Some(cached) = glyph_atlas.get_composed(ckey) {
                let vbuf = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Composed Color Glyph VB"),
                        contents: bytemuck::cast_slice(verts),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                render_pass.set_vertex_buffer(0, vbuf.slice(..));
                render_pass.set_bind_group(1, &cached.bind_group, &[]);
                render_pass.draw(0..6, 0..1);
            }
        }
    }

    fn draw_text_decorations_and_borders(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        frame_glyphs: &FrameGlyphBuffer,
        faces: &HashMap<u32, Face>,
        box_spans: &[BoxSpan],
        has_line_anims: bool,
        want_overlay: bool,
    ) {
        self.draw_text_decorations(render_pass, frame_glyphs, has_line_anims, want_overlay);
        self.draw_merged_box_borders(render_pass, faces, box_spans, want_overlay);
    }

    fn draw_text_decorations(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        frame_glyphs: &FrameGlyphBuffer,
        has_line_anims: bool,
        want_overlay: bool,
    ) {
        // === Draw text decorations (underline, overline, strike-through) ===
        // Rendered after text so decorations appear on top of glyphs.
        // Box borders are handled separately via merged box_spans below.
        let mut decoration_vertices: Vec<RectVertex> = Vec::new();

        for glyph in &frame_glyphs.glyphs {
            if let FrameGlyph::Char {
                x,
                y,
                baseline,
                width,
                height,
                ascent,
                fg,
                face_id,
                underline,
                underline_color,
                strike_through,
                strike_through_color,
                overline,
                overline_color,
                is_overlay,
                ..
            } = glyph
            {
                if *is_overlay != want_overlay {
                    continue;
                }

                let y_offset = if has_line_anims {
                    self.line_y_offset(*x, *y)
                } else {
                    0.0
                };
                let ya = *y + y_offset;
                let baseline_y = *baseline + y_offset;

                // Get per-face font metrics for proper decoration positioning
                let (ul_pos, ul_thick) = frame_glyphs
                    .faces
                    .get(face_id)
                    .map(|f| (f.underline_position as f32, f.underline_thickness as f32))
                    .unwrap_or((1.0, 1.0));

                // --- Underline ---
                if *underline > 0 {
                    let ul_color = underline_color.as_ref().unwrap_or(fg);
                    let ul_y = baseline_y + ul_pos;
                    let line_thickness = ul_thick.max(1.0);

                    match underline {
                        1 => {
                            // Single solid line
                            self.add_rect(
                                &mut decoration_vertices,
                                *x,
                                ul_y,
                                *width,
                                line_thickness,
                                ul_color,
                            );
                        }
                        2 => {
                            // Wave: smooth sine wave underline
                            let amplitude: f32 = 2.0;
                            let wavelength: f32 = 8.0;
                            let seg_w: f32 = 1.0;
                            let mut cx = *x;
                            while cx < *x + *width {
                                let sw = seg_w.min(*x + *width - cx);
                                let phase = (cx - *x) * std::f32::consts::TAU / wavelength;
                                let offset = phase.sin() * amplitude;
                                self.add_rect(
                                    &mut decoration_vertices,
                                    cx,
                                    ul_y + offset,
                                    sw,
                                    line_thickness,
                                    ul_color,
                                );
                                cx += seg_w;
                            }
                        }
                        3 => {
                            // Double line
                            self.add_rect(
                                &mut decoration_vertices,
                                *x,
                                ul_y,
                                *width,
                                line_thickness,
                                ul_color,
                            );
                            self.add_rect(
                                &mut decoration_vertices,
                                *x,
                                ul_y + line_thickness + 1.0,
                                *width,
                                line_thickness,
                                ul_color,
                            );
                        }
                        4 => {
                            // Dots (dot size = thickness, gap = 2px)
                            let mut cx = *x;
                            while cx < *x + *width {
                                let dw = line_thickness.min(*x + *width - cx);
                                self.add_rect(
                                    &mut decoration_vertices,
                                    cx,
                                    ul_y,
                                    dw,
                                    line_thickness,
                                    ul_color,
                                );
                                cx += line_thickness + 2.0;
                            }
                        }
                        5 => {
                            // Dashes (4px with 3px gap)
                            let mut cx = *x;
                            while cx < *x + *width {
                                let dw = 4.0_f32.min(*x + *width - cx);
                                self.add_rect(
                                    &mut decoration_vertices,
                                    cx,
                                    ul_y,
                                    dw,
                                    line_thickness,
                                    ul_color,
                                );
                                cx += 7.0;
                            }
                        }
                        _ => {
                            // Fallback: single line
                            self.add_rect(
                                &mut decoration_vertices,
                                *x,
                                ul_y,
                                *width,
                                line_thickness,
                                ul_color,
                            );
                        }
                    }
                }

                // --- Overline ---
                if *overline > 0 {
                    let ol_color = overline_color.as_ref().unwrap_or(fg);
                    self.add_rect(
                        &mut decoration_vertices,
                        *x,
                        ya,
                        *width,
                        ul_thick.max(1.0),
                        ol_color,
                    );
                }

                // --- Strike-through ---
                if *strike_through > 0 {
                    let st_color = strike_through_color.as_ref().unwrap_or(fg);
                    // Position at ~1/3 of ascent above baseline (standard typographic position)
                    let st_y = baseline_y - *ascent / 3.0;
                    self.add_rect(
                        &mut decoration_vertices,
                        *x,
                        st_y,
                        *width,
                        ul_thick.max(1.0),
                        st_color,
                    );
                }
            }
        }

        if !decoration_vertices.is_empty() {
            let decoration_buffer =
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Decoration Rect Buffer"),
                        contents: bytemuck::cast_slice(&decoration_vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    });

            render_pass.set_pipeline(&self.rect_pipeline);
            render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            render_pass.set_vertex_buffer(0, decoration_buffer.slice(..));
            render_pass.draw(0..decoration_vertices.len() as u32, 0..1);
        }
    }

    fn draw_merged_box_borders(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        faces: &HashMap<u32, Face>,
        box_spans: &[BoxSpan],
        want_overlay: bool,
    ) {
        // === Draw box borders (merged spans) ===
        // Standard boxes (corner_radius=0): merged rect borders (top/bottom/left/right).
        // Rounded boxes (corner_radius>0): SDF border ring.
        // Sharp box borders as merged rect spans
        let mut sharp_border_vertices: Vec<RectVertex> = Vec::new();
        // Rounded box borders via SDF
        let mut rounded_border_vertices: Vec<RoundedRectVertex> = Vec::new();

        // Filter spans for this overlay pass
        let pass_spans: Vec<usize> = box_spans
            .iter()
            .enumerate()
            .filter(|(_, s)| s.is_overlay == want_overlay)
            .map(|(i, _)| i)
            .collect();

        for (idx_in_pass, &span_idx) in pass_spans.iter().enumerate() {
            let span = &box_spans[span_idx];
            if let Some(face) = faces.get(&span.face_id) {
                let bx_color = face.box_color.as_ref().unwrap_or(&face.foreground);
                let bw = face.box_line_width as f32;

                if face.box_corner_radius > 0 {
                    // Rounded border via SDF (with optional fancy style)
                    let radius = (face.box_corner_radius as f32)
                        .min(span.height * 0.45)
                        .min(span.width * 0.45);
                    let color2 = face.box_color2.as_ref().unwrap_or(bx_color);
                    self.add_rounded_rect_styled(
                        &mut rounded_border_vertices,
                        span.x,
                        span.y,
                        span.width,
                        span.height,
                        bw,
                        radius,
                        bx_color,
                        face.box_border_style,
                        face.box_border_speed,
                        color2,
                    );
                    if face.box_border_style > 0 {
                        self.has_animated_borders = true;
                    }
                } else {
                    // Sharp border — for overlay spans (mode-line), suppress
                    // internal left/right borders between adjacent spans for
                    // continuity. For non-overlay spans, always draw all 4 borders.
                    let suppress_internal = span.is_overlay;
                    let has_left_neighbor = suppress_internal && idx_in_pass > 0 && {
                        let prev = &box_spans[pass_spans[idx_in_pass - 1]];
                        (prev.y - span.y).abs() < 0.5
                            && ((prev.x + prev.width) - span.x).abs() < 1.5
                    };
                    let has_right_neighbor =
                        suppress_internal && idx_in_pass + 1 < pass_spans.len() && {
                            let next = &box_spans[pass_spans[idx_in_pass + 1]];
                            (next.y - span.y).abs() < 0.5
                                && (next.x - (span.x + span.width)).abs() < 1.5
                        };

                    // Compute edge colors for 3D box types
                    let (top_left_color, bottom_right_color) = match face.box_type {
                        BoxType::Raised3D => {
                            let light = Color {
                                r: (bx_color.r * 1.4).min(1.0),
                                g: (bx_color.g * 1.4).min(1.0),
                                b: (bx_color.b * 1.4).min(1.0),
                                a: bx_color.a,
                            };
                            let dark = Color {
                                r: bx_color.r * 0.6,
                                g: bx_color.g * 0.6,
                                b: bx_color.b * 0.6,
                                a: bx_color.a,
                            };
                            (light, dark)
                        }
                        BoxType::Sunken3D => {
                            let light = Color {
                                r: (bx_color.r * 1.4).min(1.0),
                                g: (bx_color.g * 1.4).min(1.0),
                                b: (bx_color.b * 1.4).min(1.0),
                                a: bx_color.a,
                            };
                            let dark = Color {
                                r: bx_color.r * 0.6,
                                g: bx_color.g * 0.6,
                                b: bx_color.b * 0.6,
                                a: bx_color.a,
                            };
                            (dark, light)
                        }
                        _ => (bx_color.clone(), bx_color.clone()),
                    };

                    // Top
                    self.add_rect(
                        &mut sharp_border_vertices,
                        span.x,
                        span.y,
                        span.width,
                        bw,
                        &top_left_color,
                    );
                    // Bottom
                    self.add_rect(
                        &mut sharp_border_vertices,
                        span.x,
                        span.y + span.height - bw,
                        span.width,
                        bw,
                        &bottom_right_color,
                    );
                    // Left (only if no adjacent span to the left on same row)
                    if !has_left_neighbor {
                        self.add_rect(
                            &mut sharp_border_vertices,
                            span.x,
                            span.y,
                            bw,
                            span.height,
                            &top_left_color,
                        );
                    }
                    // Right (only if no adjacent span to the right on same row)
                    if !has_right_neighbor {
                        self.add_rect(
                            &mut sharp_border_vertices,
                            span.x + span.width - bw,
                            span.y,
                            bw,
                            span.height,
                            &bottom_right_color,
                        );
                    }
                }
            }
        }

        // Draw sharp box borders
        if !sharp_border_vertices.is_empty() {
            let sharp_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Sharp Box Border Buffer"),
                    contents: bytemuck::cast_slice(&sharp_border_vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            render_pass.set_pipeline(&self.rect_pipeline);
            render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            render_pass.set_vertex_buffer(0, sharp_buffer.slice(..));
            render_pass.draw(0..sharp_border_vertices.len() as u32, 0..1);
        }

        // Draw rounded box borders
        if !rounded_border_vertices.is_empty() {
            let rounded_buffer =
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Rounded Box Border Buffer"),
                        contents: bytemuck::cast_slice(&rounded_border_vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
            render_pass.set_pipeline(&self.rounded_rect_pipeline);
            render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            render_pass.set_vertex_buffer(0, rounded_buffer.slice(..));
            render_pass.draw(0..rounded_border_vertices.len() as u32, 0..1);
        }
    }

    fn draw_overlay_backgrounds(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        faces: &HashMap<u32, Face>,
        box_spans: &[BoxSpan],
        overlay_rect_vertices: &[RectVertex],
        want_overlay: bool,
    ) {
        self.draw_overlay_rect_backgrounds(render_pass, overlay_rect_vertices, want_overlay);
        self.draw_overlay_rounded_box_fills(render_pass, faces, box_spans, want_overlay);
    }

    fn draw_overlay_rect_backgrounds(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        overlay_rect_vertices: &[RectVertex],
        want_overlay: bool,
    ) {
        // === Step 3: Draw overlay backgrounds before overlay text ===
        if want_overlay && !overlay_rect_vertices.is_empty() {
            let rect_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Overlay Rect Buffer"),
                    contents: bytemuck::cast_slice(overlay_rect_vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });

            render_pass.set_pipeline(&self.rect_pipeline);
            render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            render_pass.set_vertex_buffer(0, rect_buffer.slice(..));
            render_pass.draw(0..overlay_rect_vertices.len() as u32, 0..1);
        }
    }

    fn draw_overlay_rounded_box_fills(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        faces: &HashMap<u32, Face>,
        box_spans: &[BoxSpan],
        want_overlay: bool,
    ) {
        // Draw filled rounded rect backgrounds for overlay ROUNDED boxed spans.
        if want_overlay {
            let mut overlay_box_fill: Vec<RoundedRectVertex> = Vec::new();
            for span in box_spans {
                if !span.is_overlay {
                    continue;
                }
                if let Some(ref bg_color) = span.bg {
                    if let Some(face) = faces.get(&span.face_id) {
                        if face.box_corner_radius <= 0 {
                            continue;
                        }
                        let radius = (face.box_corner_radius as f32)
                            .min(span.height * 0.45)
                            .min(span.width * 0.45);
                        let fill_bw = span.height.max(span.width);
                        self.add_rounded_rect(
                            &mut overlay_box_fill,
                            span.x,
                            span.y,
                            span.width,
                            span.height,
                            fill_bw,
                            radius,
                            bg_color,
                        );
                    }
                }
            }
            if !overlay_box_fill.is_empty() {
                let fill_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("Overlay Box Fill Buffer"),
                            contents: bytemuck::cast_slice(&overlay_box_fill),
                            usage: wgpu::BufferUsages::VERTEX,
                        });
                render_pass.set_pipeline(&self.rounded_rect_pipeline);
                render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                render_pass.set_vertex_buffer(0, fill_buffer.slice(..));
                render_pass.draw(0..overlay_box_fill.len() as u32, 0..1);
            }
        }
    }

    fn draw_post_text_front_layers(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        cursor_vertices: &[RectVertex],
        scroll_bar_thumb_vertices: &[(f32, f32, f32, f32, f32, Color)],
    ) {
        self.draw_front_cursor_layer(render_pass, cursor_vertices);
        self.draw_front_scrollbar_thumb_layer(render_pass, scroll_bar_thumb_vertices);
    }

    fn draw_front_cursor_layer(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        cursor_vertices: &[RectVertex],
    ) {
        // Draw cursors and borders (after text)
        if !cursor_vertices.is_empty() {
            let cursor_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Cursor Vertex Buffer"),
                    contents: bytemuck::cast_slice(&cursor_vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });

            render_pass.set_pipeline(&self.rect_pipeline);
            render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            render_pass.set_vertex_buffer(0, cursor_buffer.slice(..));
            render_pass.draw(0..cursor_vertices.len() as u32, 0..1);
        }
    }

    fn draw_front_scrollbar_thumb_layer(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        scroll_bar_thumb_vertices: &[(f32, f32, f32, f32, f32, Color)],
    ) {
        // === Draw scroll bar thumbs as filled rounded rects ===
        if !scroll_bar_thumb_vertices.is_empty() {
            let mut rounded_verts: Vec<RoundedRectVertex> = Vec::new();
            for (tx, ty, tw, th, radius, color) in scroll_bar_thumb_vertices {
                // border_width = 0 triggers filled mode in the shader
                self.add_rounded_rect(&mut rounded_verts, *tx, *ty, *tw, *th, 0.0, *radius, color);
            }
            let thumb_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Scroll Bar Thumb Buffer"),
                    contents: bytemuck::cast_slice(&rounded_verts),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            render_pass.set_pipeline(&self.rounded_rect_pipeline);
            render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            render_pass.set_vertex_buffer(0, thumb_buffer.slice(..));
            render_pass.draw(0..rounded_verts.len() as u32, 0..1);
        }
    }

    fn draw_post_content_effects(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        ctx: &super::effect_common::EffectCtx<'_>,
        faces: &HashMap<u32, Face>,
    ) {
        self.draw_post_content_effects_part1(render_pass, ctx);
        self.draw_post_content_effects_part2(render_pass, ctx, faces);
        self.draw_post_content_effects_part3(render_pass, ctx);
    }

    fn draw_post_content_effects_part1(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        ctx: &super::effect_common::EffectCtx<'_>,
    ) {
        // === Draw mode-line separators ===
        draw_effect!(
            self,
            render_pass,
            "Mode-line Separator Buffer",
            super::window_effects::emit_mode_line_separator(ctx)
        );

        // === Buffer-local accent color strip ===
        draw_effect!(
            self,
            render_pass,
            "Accent Strip Buffer",
            super::window_effects::emit_accent_strip(ctx)
        );

        // === Window background tint based on file type ===
        draw_effect!(
            self,
            render_pass,
            "Mode Tint Buffer",
            super::window_effects::emit_window_mode_tint(ctx)
        );

        // === Animated focus ring (marching ants) around selected window ===
        draw_stateful!(
            self,
            render_pass,
            "Focus Ring Buffer",
            super::window_effects::emit_focus_ring(ctx, self.focus_ring_start)
        );

        // === Window padding gradient (inner edge shading for depth) ===
        draw_effect!(
            self,
            render_pass,
            "Padding Gradient Buffer",
            super::window_effects::emit_window_padding_gradient(ctx)
        );

        // === Smooth border color transition on focus ===
        draw_stateful!(
            self,
            render_pass,
            "Border Transition Buffer",
            super::window_effects::emit_border_transition(
                ctx,
                &mut self.border_transitions,
                &mut self.prev_border_selected,
                self.border_transition_duration,
            )
        );

        // === Frosted glass effect on mode-lines ===
        draw_effect!(
            self,
            render_pass,
            "Frosted Glass Buffer",
            super::window_effects::emit_frosted_glass(ctx)
        );

        // === Noise/film grain texture overlay ===
        draw_stateful!(
            self,
            render_pass,
            "Noise Grain Buffer",
            super::window_effects::emit_noise_grain(ctx, &mut self.noise_grain_frame)
        );

        // === Idle screen dimming ===
        draw_effect!(
            self,
            render_pass,
            "Idle Dim Buffer",
            super::window_effects::emit_idle_dimming(ctx, self.idle_dim_alpha)
        );

        // === Focus mode: dim lines outside current paragraph ===
        draw_effect!(
            self,
            render_pass,
            "Focus Mode Buffer",
            super::window_effects::emit_focus_mode(ctx)
        );

        // === Draw inactive window dimming overlays (with smooth fade) ===
        draw_stateful!(
            self,
            render_pass,
            "Inactive Dim Buffer",
            super::window_effects::emit_inactive_window_dimming(
                ctx,
                &mut self.per_window_dim,
                &mut self.last_dim_tick,
            )
        );

        // === Inactive window color tint ===
        draw_effect!(
            self,
            render_pass,
            "Inactive Tint Buffer",
            super::window_effects::emit_inactive_window_tint(ctx)
        );
    }

    fn draw_post_content_effects_part2(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        ctx: &super::effect_common::EffectCtx<'_>,
        faces: &HashMap<u32, Face>,
    ) {
        // === Zen mode: draw margin overlays for centered content ===
        draw_effect!(
            self,
            render_pass,
            "Zen Mode Buffer",
            super::window_effects::emit_zen_mode(ctx)
        );

        // === Cursor trail fade (afterimage ghost) ===
        draw_stateful!(
            self,
            render_pass,
            "Cursor Trail Buffer",
            super::cursor_effects::emit_cursor_trail_fade(
                ctx,
                &mut self.cursor_trail_positions,
                &self.cursor_trail_fade_duration,
            )
        );

        // === Search highlight pulse (glow on isearch face glyphs) ===
        draw_stateful!(
            self,
            render_pass,
            "Search Pulse Buffer",
            super::window_effects::emit_search_highlight(ctx, self.search_pulse_start)
        );

        // === Selection region glow highlight ===
        draw_effect!(
            self,
            render_pass,
            "Region Glow Buffer",
            super::window_effects::emit_selection_glow(ctx, faces)
        );

        // === Typing ripple effect ===
        draw_stateful!(
            self,
            render_pass,
            "Ripple Buffer",
            super::window_effects::emit_typing_ripple(
                ctx,
                &mut self.active_ripples,
                self.typing_ripple_duration,
            )
        );

        // === Minimap: code overview column on right side of each window ===
        draw_effect!(
            self,
            render_pass,
            "Minimap Buffer",
            super::window_effects::emit_minimap(ctx)
        );

        // === Header/mode-line shadow depth effect ===
        draw_effect!(
            self,
            render_pass,
            "Header Shadow Buffer",
            super::window_effects::emit_header_shadow(ctx)
        );

        // === Active window border glow ===
        draw_effect!(
            self,
            render_pass,
            "Window Glow Buffer",
            super::window_effects::emit_active_window_glow(ctx)
        );

        // === Scroll progress indicator bar ===
        draw_effect!(
            self,
            render_pass,
            "Scroll Progress Buffer",
            super::window_effects::emit_scroll_progress(ctx)
        );

        // === Window content shadow/depth effect ===
        draw_effect!(
            self,
            render_pass,
            "Window Content Shadow Buffer",
            super::window_effects::emit_window_content_shadow(ctx)
        );
    }

    fn draw_post_content_effects_part3(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        ctx: &super::effect_common::EffectCtx<'_>,
    ) {
        // === Resize padding transition overlay ===
        {
            let pad = self.resize_padding_amount();
            draw_stateful!(
                self,
                render_pass,
                "Resize Padding Buffer",
                super::window_effects::emit_resize_padding(ctx, pad)
            );
            if pad <= 0.5 && self.resize_padding_started.is_some() {
                // Animation complete, clean up
                self.resize_padding_started = None;
            }
        }

        // === Mini-buffer completion highlight ===
        draw_effect!(
            self,
            render_pass,
            "Minibuffer Highlight Buffer",
            super::window_effects::emit_minibuffer_completion(ctx)
        );

        // === Scroll velocity fade overlay ===
        draw_stateful!(
            self,
            render_pass,
            "Scroll Velocity Fade Buffer",
            super::window_effects::emit_scroll_velocity_fade(ctx, &mut self.scroll_velocity_fades,)
        );

        // === Click halo effect ===
        draw_stateful!(
            self,
            render_pass,
            "Click Halo Buffer",
            super::window_effects::emit_click_halo(ctx, &mut self.click_halos,)
        );

        // === Window edge snap indicator ===
        draw_stateful!(
            self,
            render_pass,
            "Edge Snap Buffer",
            super::window_effects::emit_edge_snap(ctx, &mut self.edge_snaps,)
        );

        // === Line wrap indicator overlay ===
        draw_effect!(
            self,
            render_pass,
            "Wrap Indicator Buffer",
            super::window_effects::emit_line_wrap_indicator(ctx)
        );

        // === Scroll momentum indicator ===
        draw_stateful!(
            self,
            render_pass,
            "Scroll Momentum Buffer",
            super::window_effects::emit_scroll_momentum(ctx, &self.active_scroll_momentums,)
        );

        // === Vignette effect: darken edges of the frame ===
        draw_effect!(
            self,
            render_pass,
            "Vignette Buffer",
            super::window_effects::emit_vignette(ctx)
        );

        // === Window switch highlight fade ===
        draw_stateful!(
            self,
            render_pass,
            "Window Switch Fade Buffer",
            super::window_effects::emit_window_switch_fade(ctx, &mut self.active_window_fades,)
        );
    }
}
