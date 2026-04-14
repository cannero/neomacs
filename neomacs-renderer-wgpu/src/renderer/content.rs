//! Child-frame content rendering core.
//!
//! `render_frame_content()` renders ALL glyph types from a `FrameGlyphBuffer`
//! into an existing surface. Used by child frame rendering for full parity with
//! the main frame's glyph handling.
//!
//! Handles: Char (with overstrike, composed, decorations), Stretch (with stipple),
//! Background, Border, Cursor (all styles with animation), ScrollBar (with rounded
//! thumbs), Image, Video, WebKit.

use super::super::glyph_atlas::{ComposedGlyphKey, GlyphKey, WgpuGlyphAtlas};
use super::super::vertex::{GlyphVertex, RectVertex, RoundedRectVertex, SubpixelGlyphVertex};
use super::WgpuRenderer;
use cosmic_text::SubpixelBin;
use neomacs_display_protocol::face::{BoxType, Face, FaceAttributes};
use neomacs_display_protocol::frame_glyphs::{
    CursorStyle, FrameGlyph, FrameGlyphBuffer, PhysCursor, WindowCursorVisual,
};
use neomacs_display_protocol::types::{AnimatedCursor, Color};
use std::collections::HashMap;
use wgpu::util::DeviceExt;

fn window_cursor_visual_matches_phys(
    cursor: &WindowCursorVisual,
    phys_cursor: &PhysCursor,
) -> bool {
    cursor.window_id == phys_cursor.window_id && cursor.slot_id == phys_cursor.slot_id
}

fn subpixel_foreground_color(bg: Color, fg: Color, blend: f32) -> [f32; 4] {
    let t = blend.clamp(0.0, 1.0);
    [
        bg.r + (fg.r - bg.r) * t,
        bg.g + (fg.g - bg.g) * t,
        bg.b + (fg.b - bg.b) * t,
        1.0,
    ]
}

fn subpixel_background_color(bg: Color) -> [f32; 4] {
    [bg.r, bg.g, bg.b, bg.a]
}

fn build_subpixel_vertices(
    glyph_x: f32,
    glyph_y: f32,
    glyph_w: f32,
    glyph_h: f32,
    tex_v_min: f32,
    tex_v_max: f32,
    fg_color: [f32; 4],
    bg_color: [f32; 4],
) -> [SubpixelGlyphVertex; 6] {
    [
        SubpixelGlyphVertex {
            position: [glyph_x, glyph_y],
            tex_coords: [0.0, tex_v_min],
            fg_color,
            bg_color,
        },
        SubpixelGlyphVertex {
            position: [glyph_x + glyph_w, glyph_y],
            tex_coords: [1.0, tex_v_min],
            fg_color,
            bg_color,
        },
        SubpixelGlyphVertex {
            position: [glyph_x + glyph_w, glyph_y + glyph_h],
            tex_coords: [1.0, tex_v_max],
            fg_color,
            bg_color,
        },
        SubpixelGlyphVertex {
            position: [glyph_x, glyph_y],
            tex_coords: [0.0, tex_v_min],
            fg_color,
            bg_color,
        },
        SubpixelGlyphVertex {
            position: [glyph_x + glyph_w, glyph_y + glyph_h],
            tex_coords: [1.0, tex_v_max],
            fg_color,
            bg_color,
        },
        SubpixelGlyphVertex {
            position: [glyph_x, glyph_y + glyph_h],
            tex_coords: [0.0, tex_v_max],
            fg_color,
            bg_color,
        },
    ]
}

impl WgpuRenderer {
    /// Render all glyphs from a `FrameGlyphBuffer` with coordinate offset.
    ///
    /// This is the child-frame content rendering core. It handles
    /// every glyph type with the same fidelity as the main frame renderer
    /// (minus visual effects which are main-frame-only).
    ///
    /// Uses `LoadOp::Load` to composite on top of existing content.
    /// Everything is rendered in a single encoder + single `queue.submit()`.
    pub fn render_frame_content(
        &self,
        view: &wgpu::TextureView,
        frame: &FrameGlyphBuffer,
        glyph_atlas: &mut WgpuGlyphAtlas,
        faces: &HashMap<u32, Face>,
        _surface_width: u32,
        _surface_height: u32,
        offset_x: f32,
        offset_y: f32,
        cursor_visible: bool,
        animated_cursor: Option<AnimatedCursor>,
        clip_corner_radius: f32,
    ) {
        tracing::debug!(
            "render_frame_content: frame={}x{} offset=({:.1},{:.1}) {} glyphs",
            frame.width,
            frame.height,
            offset_x,
            offset_y,
            frame.glyphs.len(),
        );

        // --- Box span merging (for proper border rendering) ---
        let box_spans = self.merge_box_spans(frame, faces);

        // --- Collect vertices by category for correct z-ordering ---
        //
        // Rendering order:
        //   1. Backgrounds (window bg, stretches, char bg)
        //   2. Text (mask glyphs, color glyphs, composed)
        //   3. Decorations (underline, overline, strikethrough)
        //   4. Box borders (sharp and rounded)
        //   5. Inline media (images, videos, webkit)
        //   6. Cursors, borders, scroll bars (on top)
        let mut bg_vertices: Vec<RectVertex> = Vec::new();
        let mut cursor_bg_vertices: Vec<RectVertex> = Vec::new();
        let mut cursor_vertices: Vec<RectVertex> = Vec::new();
        let mut scroll_bar_thumbs: Vec<(f32, f32, f32, f32, f32, Color)> = Vec::new();

        // --- Step 1: Collect backgrounds ---
        for glyph in &frame.glyphs {
            match glyph {
                FrameGlyph::Background { bounds, color } => {
                    self.add_rect(
                        &mut bg_vertices,
                        bounds.x + offset_x,
                        bounds.y + offset_y,
                        bounds.width,
                        bounds.height,
                        color,
                    );
                }
                FrameGlyph::Stretch {
                    x,
                    y,
                    width,
                    height,
                    bg,
                    stipple_id,
                    stipple_fg,
                    ..
                } => {
                    self.add_rect(
                        &mut bg_vertices,
                        *x + offset_x,
                        *y + offset_y,
                        *width,
                        *height,
                        bg,
                    );
                    // Stipple pattern overlay
                    if *stipple_id > 0 {
                        if let (Some(fg), Some(pat)) =
                            (stipple_fg, frame.stipple_patterns.get(stipple_id))
                        {
                            self.render_stipple_pattern(
                                &mut bg_vertices,
                                *x + offset_x,
                                *y + offset_y,
                                *width,
                                *height,
                                fg,
                                pat,
                            );
                        }
                    }
                }
                FrameGlyph::Char {
                    x,
                    y,
                    width,
                    height,
                    bg,
                    ..
                } => {
                    if let Some(bg_color) = bg {
                        self.add_rect(
                            &mut bg_vertices,
                            *x + offset_x,
                            *y + offset_y,
                            *width,
                            *height,
                            bg_color,
                        );
                    }
                }
                _ => {}
            }
        }

        // --- Collect cursors, borders, scroll bars ---
        for glyph in &frame.glyphs {
            match glyph {
                FrameGlyph::Border {
                    x,
                    y,
                    width,
                    height,
                    color,
                    ..
                } => {
                    self.add_rect(
                        &mut cursor_vertices,
                        *x + offset_x,
                        *y + offset_y,
                        *width,
                        *height,
                        color,
                    );
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
                    // Track
                    self.add_rect(
                        &mut cursor_vertices,
                        *x + offset_x,
                        *y + offset_y,
                        *width,
                        *height,
                        track_color,
                    );
                    // Thumb (rounded)
                    let (tx, ty, tw, th) = if *horizontal {
                        (
                            *x + offset_x + *thumb_start,
                            *y + offset_y,
                            *thumb_size,
                            *height,
                        )
                    } else {
                        (
                            *x + offset_x,
                            *y + offset_y + *thumb_start,
                            *width,
                            *thumb_size,
                        )
                    };
                    let radius = tw.min(th) * self.effects.scroll_bar.thumb_radius;
                    scroll_bar_thumbs.push((tx, ty, tw, th, radius, *thumb_color));
                }
                _ => {}
            }
        }

        for cursor in &frame.window_cursors {
            if frame
                .phys_cursor
                .as_ref()
                .is_some_and(|phys| window_cursor_visual_matches_phys(cursor, phys))
            {
                continue;
            }

            if !cursor_visible && !cursor.style.is_hollow() {
                continue;
            }

            let (gx, gy, gw, gh) = if !cursor.style.is_hollow() {
                if let Some(ref ac) = animated_cursor {
                    if ac.window_id == cursor.window_id {
                        (ac.x + offset_x, ac.y + offset_y, ac.width, ac.height)
                    } else {
                        (
                            cursor.x + offset_x,
                            cursor.y + offset_y,
                            cursor.width,
                            cursor.height,
                        )
                    }
                } else {
                    (
                        cursor.x + offset_x,
                        cursor.y + offset_y,
                        cursor.width,
                        cursor.height,
                    )
                }
            } else {
                (
                    cursor.x + offset_x,
                    cursor.y + offset_y,
                    cursor.width,
                    cursor.height,
                )
            };

            match cursor.style {
                CursorStyle::FilledBox => {
                    self.add_rect(&mut cursor_bg_vertices, gx, gy, gw, gh, &cursor.color);
                }
                CursorStyle::Bar(bar_w) => {
                    self.add_rect(&mut cursor_vertices, gx, gy, bar_w, gh, &cursor.color);
                }
                CursorStyle::Hbar(hbar_h) => {
                    self.add_rect(
                        &mut cursor_vertices,
                        gx,
                        gy + gh - hbar_h,
                        gw,
                        hbar_h,
                        &cursor.color,
                    );
                }
                CursorStyle::Hollow => {
                    self.add_rect(&mut cursor_vertices, gx, gy, gw, 1.0, &cursor.color);
                    self.add_rect(
                        &mut cursor_vertices,
                        gx,
                        gy + gh - 1.0,
                        gw,
                        1.0,
                        &cursor.color,
                    );
                    self.add_rect(&mut cursor_vertices, gx, gy, 1.0, gh, &cursor.color);
                    self.add_rect(
                        &mut cursor_vertices,
                        gx + gw - 1.0,
                        gy,
                        1.0,
                        gh,
                        &cursor.color,
                    );
                }
            }
        }

        if let Some(cursor) = frame.phys_cursor.as_ref() {
            if cursor_visible || cursor.style.is_hollow() {
                let (gx, gy, gw, gh) = if !cursor.style.is_hollow() {
                    if let Some(ref ac) = animated_cursor {
                        if ac.window_id == cursor.window_id {
                            (ac.x + offset_x, ac.y + offset_y, ac.width, ac.height)
                        } else {
                            (
                                cursor.x + offset_x,
                                cursor.y + offset_y,
                                cursor.width,
                                cursor.height,
                            )
                        }
                    } else {
                        (
                            cursor.x + offset_x,
                            cursor.y + offset_y,
                            cursor.width,
                            cursor.height,
                        )
                    }
                } else {
                    (
                        cursor.x + offset_x,
                        cursor.y + offset_y,
                        cursor.width,
                        cursor.height,
                    )
                };

                match cursor.style {
                    CursorStyle::FilledBox => {
                        self.add_rect(&mut cursor_bg_vertices, gx, gy, gw, gh, &cursor.color);
                    }
                    CursorStyle::Bar(bar_w) => {
                        self.add_rect(&mut cursor_vertices, gx, gy, bar_w, gh, &cursor.color);
                    }
                    CursorStyle::Hbar(hbar_h) => {
                        self.add_rect(
                            &mut cursor_vertices,
                            gx,
                            gy + gh - hbar_h,
                            gw,
                            hbar_h,
                            &cursor.color,
                        );
                    }
                    CursorStyle::Hollow => {
                        self.add_rect(&mut cursor_vertices, gx, gy, gw, 1.0, &cursor.color);
                        self.add_rect(
                            &mut cursor_vertices,
                            gx,
                            gy + gh - 1.0,
                            gw,
                            1.0,
                            &cursor.color,
                        );
                        self.add_rect(&mut cursor_vertices, gx, gy, 1.0, gh, &cursor.color);
                        self.add_rect(
                            &mut cursor_vertices,
                            gx + gw - 1.0,
                            gy,
                            1.0,
                            gh,
                            &cursor.color,
                        );
                    }
                }
            }
        }

        // --- Step 2: Collect text glyphs (with overstrike and composed) ---
        let mut mask_data: Vec<(GlyphKey, [GlyphVertex; 6])> = Vec::new();
        let mut subpixel_data: Vec<(GlyphKey, [SubpixelGlyphVertex; 6])> = Vec::new();
        let mut color_data: Vec<(GlyphKey, [GlyphVertex; 6])> = Vec::new();
        let mut composed_mask_data: Vec<(ComposedGlyphKey, [GlyphVertex; 6])> = Vec::new();
        let mut composed_subpixel_data: Vec<(ComposedGlyphKey, [SubpixelGlyphVertex; 6])> =
            Vec::new();
        let mut composed_color_data: Vec<(ComposedGlyphKey, [GlyphVertex; 6])> = Vec::new();
        let enable_subpixel = glyph_atlas.subpixel_enabled();

        for glyph in &frame.glyphs {
            if let FrameGlyph::Char {
                char: ch,
                composed,
                x,
                y: _,
                baseline,
                width: _,
                ascent: _,
                fg,
                bg,
                face_id,
                font_size,
                overstrike,
                ..
            } = glyph
            {
                let face = faces.get(face_id);

                // Decompose physical-pixel positions into integer + subpixel bin.
                // The bin is baked into the rasterized bitmap by swash for subpixel
                // accuracy; vertex positions stay on integer pixels (no Linear blur).
                let sf = self.scale_factor;
                let phys_x = (*x + offset_x) * sf;
                let baseline_y = *baseline + offset_y;
                let phys_y = baseline_y * sf;
                let (x_int, x_bin) = SubpixelBin::new(phys_x);
                let (y_int, y_bin) = SubpixelBin::new(phys_y);

                let cached_opt = if let Some(text) = composed {
                    glyph_atlas.get_or_create_composed(
                        &self.device,
                        &self.queue,
                        text,
                        *face_id,
                        font_size.to_bits(),
                        face,
                        x_bin,
                        y_bin,
                        enable_subpixel,
                    )
                } else {
                    let key = GlyphKey {
                        charcode: *ch as u32,
                        face_id: *face_id,
                        font_size_bits: font_size.to_bits(),
                        x_bin,
                        y_bin,
                    };
                    glyph_atlas.get_or_create(
                        &self.device,
                        &self.queue,
                        &key,
                        face,
                        enable_subpixel,
                    )
                };

                if let Some(cached) = cached_opt {
                    // Vertex positions from integer physical pixels + bearing,
                    // converted back to logical pixels.
                    let glyph_x = (x_int as f32 + cached.bearing_x) / sf;
                    let glyph_y = (y_int as f32 - cached.bearing_y) / sf;
                    let glyph_w = cached.width as f32 / sf;
                    let glyph_h = cached.height as f32 / sf;

                    // Inverse-video cursor support: when the filled box cursor is
                    // visible, draw the covered character with cursor_fg.
                    let mut effective_fg = *fg;
                    let mut effective_bg = (*bg)
                        .or_else(|| face.map(|resolved| resolved.background))
                        .unwrap_or(Color::rgb(1.0, 1.0, 1.0));
                    if cursor_visible
                        && let Some(cursor) = frame.phys_cursor.as_ref()
                        && matches!(cursor.style, CursorStyle::FilledBox)
                        && glyph.slot_id().is_some_and(|slot| slot == cursor.slot_id)
                    {
                        effective_fg = cursor.cursor_fg;
                        effective_bg = cursor.color;
                    }

                    let color = if cached.is_color {
                        [1.0, 1.0, 1.0, 1.0]
                    } else {
                        [
                            effective_fg.r,
                            effective_fg.g,
                            effective_fg.b,
                            effective_fg.a,
                        ]
                    };
                    let subpixel_fg = subpixel_foreground_color(effective_bg, effective_fg, 1.0);
                    let subpixel_bg = subpixel_background_color(effective_bg);

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
                    ];

                    // Overstrike: simulate bold by drawing shifted 1px right
                    let overstrike_vertices = if *overstrike {
                        let ox = 1.0 / sf;
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
                                tex_coords: [1.0, 1.0],
                                color,
                            },
                            GlyphVertex {
                                position: [glyph_x + ox, glyph_y],
                                tex_coords: [0.0, 0.0],
                                color,
                            },
                            GlyphVertex {
                                position: [glyph_x + ox + glyph_w, glyph_y + glyph_h],
                                tex_coords: [1.0, 1.0],
                                color,
                            },
                            GlyphVertex {
                                position: [glyph_x + ox, glyph_y + glyph_h],
                                tex_coords: [0.0, 1.0],
                                color,
                            },
                        ])
                    } else {
                        None
                    };

                    let subpixel_vertices = build_subpixel_vertices(
                        glyph_x,
                        glyph_y,
                        glyph_w,
                        glyph_h,
                        0.0,
                        1.0,
                        subpixel_fg,
                        subpixel_bg,
                    );

                    let overstrike_subpixel_vertices = if *overstrike {
                        let ox = 1.0 / sf;
                        Some(build_subpixel_vertices(
                            glyph_x + ox,
                            glyph_y,
                            glyph_w,
                            glyph_h,
                            0.0,
                            1.0,
                            subpixel_fg,
                            subpixel_bg,
                        ))
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
                        } else if cached.is_subpixel {
                            composed_subpixel_data.push((ckey.clone(), subpixel_vertices));
                            if let Some(ov) = overstrike_subpixel_vertices {
                                composed_subpixel_data.push((ckey, ov));
                            }
                        } else {
                            composed_mask_data.push((ckey.clone(), vertices));
                            if let Some(ov) = overstrike_vertices {
                                composed_mask_data.push((ckey, ov));
                            }
                        }
                    } else {
                        let key = GlyphKey {
                            charcode: *ch as u32,
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
                        } else if cached.is_subpixel {
                            subpixel_data.push((key.clone(), subpixel_vertices));
                            if let Some(ov) = overstrike_subpixel_vertices {
                                subpixel_data.push((key, ov));
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

        // --- Step 3: Collect decorations (underline, overline, strikethrough) ---
        let mut decoration_vertices: Vec<RectVertex> = Vec::new();
        for glyph in &frame.glyphs {
            if let FrameGlyph::Char {
                x,
                y,
                baseline,
                width,
                ascent,
                fg,
                face_id,
                underline,
                underline_color,
                strike_through,
                strike_through_color,
                overline,
                overline_color,
                ..
            } = glyph
            {
                let gx = *x + offset_x;
                let gy = *y + offset_y;
                let baseline_y = *baseline + offset_y;

                // Per-face font metrics for underline positioning
                let (ul_pos, ul_thick) = frame
                    .faces
                    .get(face_id)
                    .map(|f| (f.underline_position as f32, f.underline_thickness as f32))
                    .unwrap_or((1.0, 1.0));

                // Underline
                if *underline > 0 {
                    let ul_color = underline_color.as_ref().unwrap_or(fg);
                    let ul_y = baseline_y + ul_pos;
                    let line_thickness = ul_thick.max(1.0);

                    match underline {
                        1 => {
                            // Single solid line
                            self.add_rect(
                                &mut decoration_vertices,
                                gx,
                                ul_y,
                                *width,
                                line_thickness,
                                ul_color,
                            );
                        }
                        2 => {
                            // Wave underline
                            let amplitude: f32 = 2.0;
                            let wavelength: f32 = 8.0;
                            let seg_w: f32 = 1.0;
                            let mut cx = gx;
                            while cx < gx + *width {
                                let sw = seg_w.min(gx + *width - cx);
                                let phase = (cx - gx) * std::f32::consts::TAU / wavelength;
                                let wave_offset = phase.sin() * amplitude;
                                self.add_rect(
                                    &mut decoration_vertices,
                                    cx,
                                    ul_y + wave_offset,
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
                                gx,
                                ul_y,
                                *width,
                                line_thickness,
                                ul_color,
                            );
                            self.add_rect(
                                &mut decoration_vertices,
                                gx,
                                ul_y + line_thickness + 1.0,
                                *width,
                                line_thickness,
                                ul_color,
                            );
                        }
                        4 => {
                            // Dotted
                            let mut cx = gx;
                            while cx < gx + *width {
                                let dw = line_thickness.min(gx + *width - cx);
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
                            // Dashed
                            let mut cx = gx;
                            while cx < gx + *width {
                                let dw = 4.0_f32.min(gx + *width - cx);
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
                            self.add_rect(
                                &mut decoration_vertices,
                                gx,
                                ul_y,
                                *width,
                                line_thickness,
                                ul_color,
                            );
                        }
                    }
                }

                // Overline
                if *overline > 0 {
                    let ol_color = overline_color.as_ref().unwrap_or(fg);
                    self.add_rect(
                        &mut decoration_vertices,
                        gx,
                        gy,
                        *width,
                        ul_thick.max(1.0),
                        ol_color,
                    );
                }

                // Strikethrough
                if *strike_through > 0 {
                    let st_color = strike_through_color.as_ref().unwrap_or(fg);
                    let st_y = baseline_y - *ascent / 3.0;
                    self.add_rect(
                        &mut decoration_vertices,
                        gx,
                        st_y,
                        *width,
                        ul_thick.max(1.0),
                        st_color,
                    );
                }
            }
        }

        // --- Step 4: Box borders (sharp and rounded, from merged spans) ---
        let mut sharp_border_vertices: Vec<RectVertex> = Vec::new();
        let mut rounded_border_vertices: Vec<RoundedRectVertex> = Vec::new();
        let mut rounded_fill_vertices: Vec<RoundedRectVertex> = Vec::new();

        for (idx, span) in box_spans.iter().enumerate() {
            if let Some(face) = faces.get(&span.face_id) {
                let bx_color = face.box_color.as_ref().unwrap_or(&face.foreground);
                let bw = face.box_line_width as f32;

                // Rounded box background fill
                if face.box_corner_radius > 0 {
                    if let Some(ref bg_color) = span.bg {
                        let radius = (face.box_corner_radius as f32)
                            .min(span.height * 0.45)
                            .min(span.width * 0.45);
                        let fill_bw = span.height.max(span.width);
                        self.add_rounded_rect(
                            &mut rounded_fill_vertices,
                            span.x + offset_x,
                            span.y + offset_y,
                            span.width,
                            span.height,
                            fill_bw,
                            radius,
                            bg_color,
                        );
                    }
                }

                // Box border
                if face.box_corner_radius > 0 {
                    let radius = (face.box_corner_radius as f32)
                        .min(span.height * 0.45)
                        .min(span.width * 0.45);
                    let color2 = face.box_color2.as_ref().unwrap_or(bx_color);
                    self.add_rounded_rect_styled(
                        &mut rounded_border_vertices,
                        span.x + offset_x,
                        span.y + offset_y,
                        span.width,
                        span.height,
                        bw,
                        radius,
                        bx_color,
                        face.box_border_style,
                        face.box_border_speed,
                        color2,
                    );
                    // Note: animated border flag is set in render_frame_glyphs (glyphs.rs)
                } else {
                    // Sharp border — check for neighbor suppression
                    let has_left_neighbor = idx > 0 && {
                        let prev = &box_spans[idx - 1];
                        (prev.y - span.y).abs() < 0.5
                            && ((prev.x + prev.width) - span.x).abs() < 1.5
                    };
                    let has_right_neighbor = idx + 1 < box_spans.len() && {
                        let next = &box_spans[idx + 1];
                        (next.y - span.y).abs() < 0.5
                            && (next.x - (span.x + span.width)).abs() < 1.5
                    };

                    // 3D box edge colors
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

                    let sx = span.x + offset_x;
                    let sy = span.y + offset_y;

                    // Top
                    self.add_rect(
                        &mut sharp_border_vertices,
                        sx,
                        sy,
                        span.width,
                        bw,
                        &top_left_color,
                    );
                    // Bottom
                    self.add_rect(
                        &mut sharp_border_vertices,
                        sx,
                        sy + span.height - bw,
                        span.width,
                        bw,
                        &bottom_right_color,
                    );
                    // Left
                    if !has_left_neighbor {
                        self.add_rect(
                            &mut sharp_border_vertices,
                            sx,
                            sy,
                            bw,
                            span.height,
                            &top_left_color,
                        );
                    }
                    // Right
                    if !has_right_neighbor {
                        self.add_rect(
                            &mut sharp_border_vertices,
                            sx + span.width - bw,
                            sy,
                            bw,
                            span.height,
                            &bottom_right_color,
                        );
                    }
                }
            }
        }

        // === GPU submission: single encoder, single submit ===
        // Select pipelines: stencil-aware variants when clipping to rounded corners
        let use_stencil = clip_corner_radius > 0.0;
        let rect_pl = if use_stencil {
            &self.stencil_rect_pipeline
        } else {
            &self.rect_pipeline
        };
        let rounded_rect_pl = if use_stencil {
            &self.stencil_rounded_rect_pipeline
        } else {
            &self.rounded_rect_pipeline
        };
        let glyph_pl = if use_stencil {
            &self.stencil_glyph_pipeline
        } else {
            &self.glyph_pipeline
        };
        let subpixel_pl = if use_stencil {
            &self.stencil_subpixel_glyph_pipeline
        } else {
            &self.subpixel_glyph_pipeline
        };
        let image_pl = if use_stencil {
            &self.stencil_image_pipeline
        } else {
            &self.image_pipeline
        };
        let _opaque_image_pl = if use_stencil {
            &self.stencil_opaque_image_pipeline
        } else {
            &self.opaque_image_pipeline
        };

        let stencil_attachment = if use_stencil {
            Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.stencil_view,
                depth_ops: None,
                stencil_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
            })
        } else {
            None
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Frame Content Encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Frame Content Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: stencil_attachment,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            if use_stencil {
                pass.set_stencil_reference(1);
            }

            // --- Draw backgrounds ---
            if !bg_vertices.is_empty() {
                let buffer = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Content BG Buffer"),
                        contents: bytemuck::cast_slice(&bg_vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                pass.set_pipeline(rect_pl);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..bg_vertices.len() as u32, 0..1);
            }

            // Filled-box cursor backgrounds must be below text so the covered
            // glyph can be drawn with inverse foreground on top.
            if !cursor_bg_vertices.is_empty() {
                let buffer = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Content Cursor BG Buffer"),
                        contents: bytemuck::cast_slice(&cursor_bg_vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                pass.set_pipeline(rect_pl);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..cursor_bg_vertices.len() as u32, 0..1);
            }

            // --- Draw rounded box fills ---
            if !rounded_fill_vertices.is_empty() {
                let buffer = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Content Box Fill Buffer"),
                        contents: bytemuck::cast_slice(&rounded_fill_vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                pass.set_pipeline(rounded_rect_pl);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..rounded_fill_vertices.len() as u32, 0..1);
            }

            // --- Draw mask text glyphs ---
            if !mask_data.is_empty() {
                mask_data.sort_by(|(a, _), (b, _)| {
                    a.face_id
                        .cmp(&b.face_id)
                        .then(a.font_size_bits.cmp(&b.font_size_bits))
                        .then(a.charcode.cmp(&b.charcode))
                });

                let all_vertices: Vec<GlyphVertex> = mask_data
                    .iter()
                    .flat_map(|(_, verts)| verts.iter().copied())
                    .collect();

                let buffer = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Content Mask Glyph Buffer"),
                        contents: bytemuck::cast_slice(&all_vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    });

                pass.set_pipeline(glyph_pl);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, buffer.slice(..));

                let mut i = 0;
                while i < mask_data.len() {
                    let (ref key, _) = mask_data[i];
                    if let Some(cached) = glyph_atlas.get(key, enable_subpixel) {
                        let batch_start = i;
                        i += 1;
                        while i < mask_data.len() && mask_data[i].0 == *key {
                            i += 1;
                        }
                        let vert_start = (batch_start * 6) as u32;
                        let vert_end = (i * 6) as u32;
                        pass.set_bind_group(1, &cached.bind_group, &[]);
                        pass.draw(vert_start..vert_end, 0..1);
                    } else {
                        i += 1;
                    }
                }
            }

            // --- Draw subpixel LCD text glyphs ---
            if !subpixel_data.is_empty() {
                subpixel_data.sort_by(|(a, _), (b, _)| {
                    a.face_id
                        .cmp(&b.face_id)
                        .then(a.font_size_bits.cmp(&b.font_size_bits))
                        .then(a.charcode.cmp(&b.charcode))
                });

                let all_vertices: Vec<SubpixelGlyphVertex> = subpixel_data
                    .iter()
                    .flat_map(|(_, verts)| verts.iter().copied())
                    .collect();

                let buffer = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Content Subpixel Glyph Buffer"),
                        contents: bytemuck::cast_slice(&all_vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    });

                pass.set_pipeline(subpixel_pl);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, buffer.slice(..));

                let mut i = 0;
                while i < subpixel_data.len() {
                    let (ref key, _) = subpixel_data[i];
                    if let Some(cached) = glyph_atlas.get(key, enable_subpixel) {
                        let batch_start = i;
                        i += 1;
                        while i < subpixel_data.len() && subpixel_data[i].0 == *key {
                            i += 1;
                        }
                        let vert_start = (batch_start * 6) as u32;
                        let vert_end = (i * 6) as u32;
                        pass.set_bind_group(1, &cached.bind_group, &[]);
                        pass.draw(vert_start..vert_end, 0..1);
                    } else {
                        i += 1;
                    }
                }
            }

            // --- Draw color text glyphs (emoji) ---
            if !color_data.is_empty() {
                color_data.sort_by(|(a, _), (b, _)| {
                    a.face_id
                        .cmp(&b.face_id)
                        .then(a.font_size_bits.cmp(&b.font_size_bits))
                        .then(a.charcode.cmp(&b.charcode))
                });

                let all_vertices: Vec<GlyphVertex> = color_data
                    .iter()
                    .flat_map(|(_, verts)| verts.iter().copied())
                    .collect();

                let buffer = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Content Color Glyph Buffer"),
                        contents: bytemuck::cast_slice(&all_vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    });

                pass.set_pipeline(image_pl);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, buffer.slice(..));

                let mut i = 0;
                while i < color_data.len() {
                    let (ref key, _) = color_data[i];
                    if let Some(cached) = glyph_atlas.get(key, enable_subpixel) {
                        let batch_start = i;
                        i += 1;
                        while i < color_data.len() && color_data[i].0 == *key {
                            i += 1;
                        }
                        let vert_start = (batch_start * 6) as u32;
                        let vert_end = (i * 6) as u32;
                        pass.set_bind_group(1, &cached.bind_group, &[]);
                        pass.draw(vert_start..vert_end, 0..1);
                    } else {
                        i += 1;
                    }
                }
            }

            // --- Draw composed mask glyphs ---
            if !composed_mask_data.is_empty() {
                pass.set_pipeline(glyph_pl);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);

                for (ckey, verts) in &composed_mask_data {
                    if let Some(cached) = glyph_atlas.get_composed(ckey, enable_subpixel) {
                        let vbuf =
                            self.device
                                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                    label: Some("Content Composed Mask VB"),
                                    contents: bytemuck::cast_slice(verts),
                                    usage: wgpu::BufferUsages::VERTEX,
                                });
                        pass.set_vertex_buffer(0, vbuf.slice(..));
                        pass.set_bind_group(1, &cached.bind_group, &[]);
                        pass.draw(0..6, 0..1);
                    }
                }
            }

            // --- Draw composed subpixel glyphs ---
            if !composed_subpixel_data.is_empty() {
                pass.set_pipeline(subpixel_pl);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);

                for (ckey, verts) in &composed_subpixel_data {
                    if let Some(cached) = glyph_atlas.get_composed(ckey, enable_subpixel) {
                        let vbuf =
                            self.device
                                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                    label: Some("Content Composed Subpixel VB"),
                                    contents: bytemuck::cast_slice(verts),
                                    usage: wgpu::BufferUsages::VERTEX,
                                });
                        pass.set_vertex_buffer(0, vbuf.slice(..));
                        pass.set_bind_group(1, &cached.bind_group, &[]);
                        pass.draw(0..6, 0..1);
                    }
                }
            }

            // --- Draw composed color glyphs (emoji ZWJ sequences) ---
            if !composed_color_data.is_empty() {
                pass.set_pipeline(image_pl);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);

                for (ckey, verts) in &composed_color_data {
                    if let Some(cached) = glyph_atlas.get_composed(ckey, enable_subpixel) {
                        let vbuf =
                            self.device
                                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                    label: Some("Content Composed Color VB"),
                                    contents: bytemuck::cast_slice(verts),
                                    usage: wgpu::BufferUsages::VERTEX,
                                });
                        pass.set_vertex_buffer(0, vbuf.slice(..));
                        pass.set_bind_group(1, &cached.bind_group, &[]);
                        pass.draw(0..6, 0..1);
                    }
                }
            }

            // --- Draw text decorations ---
            if !decoration_vertices.is_empty() {
                let buffer = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Content Decoration Buffer"),
                        contents: bytemuck::cast_slice(&decoration_vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                pass.set_pipeline(rect_pl);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..decoration_vertices.len() as u32, 0..1);
            }

            // --- Draw sharp box borders ---
            if !sharp_border_vertices.is_empty() {
                let buffer = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Content Sharp Box Border Buffer"),
                        contents: bytemuck::cast_slice(&sharp_border_vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                pass.set_pipeline(rect_pl);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..sharp_border_vertices.len() as u32, 0..1);
            }

            // --- Draw rounded box borders ---
            if !rounded_border_vertices.is_empty() {
                let buffer = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Content Rounded Box Border Buffer"),
                        contents: bytemuck::cast_slice(&rounded_border_vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                pass.set_pipeline(rounded_rect_pl);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..rounded_border_vertices.len() as u32, 0..1);
            }

            // --- Draw inline images ---
            pass.set_pipeline(image_pl);
            pass.set_bind_group(0, &self.uniform_bind_group, &[]);

            for glyph in &frame.glyphs {
                if let FrameGlyph::Image {
                    image_id,
                    x,
                    y,
                    width,
                    height,
                    ..
                } = glyph
                {
                    if let Some(cached) = self.image_cache.get(*image_id) {
                        let ix = *x + offset_x;
                        let iy = *y + offset_y;
                        tracing::debug!(
                            "render_frame_content: image {} at ({:.1},{:.1}) size {:.1}x{:.1}",
                            image_id,
                            ix,
                            iy,
                            width,
                            height,
                        );
                        let vertices = [
                            GlyphVertex {
                                position: [ix, iy],
                                tex_coords: [0.0, 0.0],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [ix + *width, iy],
                                tex_coords: [1.0, 0.0],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [ix + *width, iy + *height],
                                tex_coords: [1.0, 1.0],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [ix, iy],
                                tex_coords: [0.0, 0.0],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [ix + *width, iy + *height],
                                tex_coords: [1.0, 1.0],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                            GlyphVertex {
                                position: [ix, iy + *height],
                                tex_coords: [0.0, 1.0],
                                color: [1.0, 1.0, 1.0, 1.0],
                            },
                        ];
                        let buffer =
                            self.device
                                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                    label: Some("Content Image Buffer"),
                                    contents: bytemuck::cast_slice(&vertices),
                                    usage: wgpu::BufferUsages::VERTEX,
                                });
                        pass.set_bind_group(1, &cached.bind_group, &[]);
                        pass.set_vertex_buffer(0, buffer.slice(..));
                        pass.draw(0..6, 0..1);
                    }
                }
            }

            // --- Draw inline videos ---
            #[cfg(feature = "video")]
            {
                for glyph in &frame.glyphs {
                    if let FrameGlyph::Video {
                        video_id,
                        x,
                        y,
                        width,
                        height,
                        ..
                    } = glyph
                    {
                        if let Some(cached) = self.video_cache.get(*video_id) {
                            if let Some(ref bind_group) = cached.bind_group {
                                let vx = *x + offset_x;
                                let vy = *y + offset_y;
                                tracing::debug!(
                                    "render_frame_content: video {} at ({:.1},{:.1}) size {:.1}x{:.1}",
                                    video_id,
                                    vx,
                                    vy,
                                    width,
                                    height,
                                );
                                let vertices = [
                                    GlyphVertex {
                                        position: [vx, vy],
                                        tex_coords: [0.0, 0.0],
                                        color: [1.0, 1.0, 1.0, 1.0],
                                    },
                                    GlyphVertex {
                                        position: [vx + *width, vy],
                                        tex_coords: [1.0, 0.0],
                                        color: [1.0, 1.0, 1.0, 1.0],
                                    },
                                    GlyphVertex {
                                        position: [vx + *width, vy + *height],
                                        tex_coords: [1.0, 1.0],
                                        color: [1.0, 1.0, 1.0, 1.0],
                                    },
                                    GlyphVertex {
                                        position: [vx, vy],
                                        tex_coords: [0.0, 0.0],
                                        color: [1.0, 1.0, 1.0, 1.0],
                                    },
                                    GlyphVertex {
                                        position: [vx + *width, vy + *height],
                                        tex_coords: [1.0, 1.0],
                                        color: [1.0, 1.0, 1.0, 1.0],
                                    },
                                    GlyphVertex {
                                        position: [vx, vy + *height],
                                        tex_coords: [0.0, 1.0],
                                        color: [1.0, 1.0, 1.0, 1.0],
                                    },
                                ];
                                let buffer = self.device.create_buffer_init(
                                    &wgpu::util::BufferInitDescriptor {
                                        label: Some("Content Video Buffer"),
                                        contents: bytemuck::cast_slice(&vertices),
                                        usage: wgpu::BufferUsages::VERTEX,
                                    },
                                );
                                pass.set_bind_group(1, bind_group, &[]);
                                pass.set_vertex_buffer(0, buffer.slice(..));
                                pass.draw(0..6, 0..1);
                            }
                        }
                    }
                }
            }

            // --- Draw inline webkit views ---
            #[cfg(feature = "wpe-webkit")]
            {
                pass.set_pipeline(_opaque_image_pl);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);

                for glyph in &frame.glyphs {
                    if let FrameGlyph::WebKit {
                        webkit_id,
                        x,
                        y,
                        width,
                        height,
                        ..
                    } = glyph
                    {
                        if let Some(cached) = self.webkit_cache.get(*webkit_id) {
                            let wx = *x + offset_x;
                            let wy = *y + offset_y;
                            tracing::debug!(
                                "render_frame_content: webkit {} at ({:.1},{:.1}) size {:.1}x{:.1}",
                                webkit_id,
                                wx,
                                wy,
                                width,
                                height,
                            );
                            let vertices = [
                                GlyphVertex {
                                    position: [wx, wy],
                                    tex_coords: [0.0, 0.0],
                                    color: [1.0, 1.0, 1.0, 1.0],
                                },
                                GlyphVertex {
                                    position: [wx + *width, wy],
                                    tex_coords: [1.0, 0.0],
                                    color: [1.0, 1.0, 1.0, 1.0],
                                },
                                GlyphVertex {
                                    position: [wx + *width, wy + *height],
                                    tex_coords: [1.0, 1.0],
                                    color: [1.0, 1.0, 1.0, 1.0],
                                },
                                GlyphVertex {
                                    position: [wx, wy],
                                    tex_coords: [0.0, 0.0],
                                    color: [1.0, 1.0, 1.0, 1.0],
                                },
                                GlyphVertex {
                                    position: [wx + *width, wy + *height],
                                    tex_coords: [1.0, 1.0],
                                    color: [1.0, 1.0, 1.0, 1.0],
                                },
                                GlyphVertex {
                                    position: [wx, wy + *height],
                                    tex_coords: [0.0, 1.0],
                                    color: [1.0, 1.0, 1.0, 1.0],
                                },
                            ];
                            let buffer =
                                self.device
                                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                        label: Some("Content WebKit Buffer"),
                                        contents: bytemuck::cast_slice(&vertices),
                                        usage: wgpu::BufferUsages::VERTEX,
                                    });
                            pass.set_bind_group(1, &cached.bind_group, &[]);
                            pass.set_vertex_buffer(0, buffer.slice(..));
                            pass.draw(0..6, 0..1);
                        }
                    }
                }
            }

            // --- Draw cursors and borders (on top of everything) ---
            if !cursor_vertices.is_empty() {
                let buffer = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Content Cursor Buffer"),
                        contents: bytemuck::cast_slice(&cursor_vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                pass.set_pipeline(rect_pl);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..cursor_vertices.len() as u32, 0..1);
            }

            // --- Draw scroll bar thumbs (rounded) ---
            if !scroll_bar_thumbs.is_empty() {
                let mut rounded_verts: Vec<RoundedRectVertex> = Vec::new();
                for (tx, ty, tw, th, radius, color) in &scroll_bar_thumbs {
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
                let buffer = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Content Scroll Thumb Buffer"),
                        contents: bytemuck::cast_slice(&rounded_verts),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                pass.set_pipeline(rounded_rect_pl);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..rounded_verts.len() as u32, 0..1);
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        tracing::debug!("render_frame_content: submitted (1 encoder, 1 pass)");
    }

    /// Merge adjacent boxed glyphs into spans for proper border rendering.
    ///
    /// All box faces get span-merged. Rounded boxes (corner_radius > 0) get SDF
    /// treatment; standard boxes (corner_radius = 0) get rect borders.
    fn merge_box_spans(
        &self,
        frame: &FrameGlyphBuffer,
        faces: &HashMap<u32, Face>,
    ) -> Vec<BoxSpan> {
        let mut box_spans: Vec<BoxSpan> = Vec::new();

        for glyph in &frame.glyphs {
            let (gx, gy, gw, gh, gface_id, g_bg) = match glyph {
                FrameGlyph::Char {
                    x,
                    y,
                    width,
                    height,
                    face_id,
                    bg,
                    ..
                } => (*x, *y, *width, *height, *face_id, *bg),
                FrameGlyph::Stretch {
                    x,
                    y,
                    width,
                    height,
                    face_id,
                    bg,
                    ..
                } => (*x, *y, *width, *height, *face_id, Some(*bg)),
                _ => continue,
            };

            // Only include glyphs whose face has BOX attribute
            match faces.get(&gface_id) {
                Some(f) if f.attributes.contains(FaceAttributes::BOX) && f.box_line_width > 0 => {}
                _ => continue,
            };

            let is_rounded = faces
                .get(&gface_id)
                .map(|f| f.box_corner_radius > 0)
                .unwrap_or(false);

            let merged = if let Some(last) = box_spans.last_mut() {
                let same_row = (last.y - gy).abs() < 0.5 && (last.height - gh).abs() < 0.5;
                let adjacent = (gx - (last.x + last.width)).abs() < 1.0;
                let same_face = last.face_id == gface_id;

                let last_is_rounded = faces
                    .get(&last.face_id)
                    .map(|f| f.box_corner_radius > 0)
                    .unwrap_or(false);
                let face_ok = if is_rounded || last_is_rounded {
                    same_face
                } else {
                    same_face
                };

                if same_row && adjacent && face_ok {
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
                    bg: g_bg,
                });
            }
        }

        box_spans
    }
}

#[cfg(test)]
mod tests {
    use super::window_cursor_visual_matches_phys;
    use neomacs_display_protocol::frame_glyphs::{
        CursorStyle, DisplaySlotId, PhysCursor, WindowCursorVisual,
    };
    use neomacs_display_protocol::types::Color;

    #[test]
    fn window_cursor_visual_match_uses_slot_identity() {
        let slot_id = DisplaySlotId::from_pixels(7, 32.0, 16.0, 8.0, 16.0);
        let phys = PhysCursor {
            window_id: 7,
            charpos: 0,
            row: slot_id.row as usize,
            col: slot_id.col,
            slot_id,
            x: 32.0,
            y: 16.0,
            width: 8.0,
            height: 16.0,
            ascent: 12.0,
            style: CursorStyle::FilledBox,
            color: Color::WHITE,
            cursor_fg: Color::BLACK,
        };
        let matching = WindowCursorVisual {
            window_id: 7,
            slot_id,
            x: 4.0,
            y: 0.0,
            width: 20.0,
            height: 30.0,
            style: CursorStyle::Hollow,
            color: Color::WHITE,
        };
        let mismatched = WindowCursorVisual {
            window_id: 7,
            slot_id: DisplaySlotId::from_pixels(7, 40.0, 16.0, 8.0, 16.0),
            x: 32.0,
            y: 16.0,
            width: 8.0,
            height: 16.0,
            style: CursorStyle::Hollow,
            color: Color::WHITE,
        };

        assert!(window_cursor_visual_matches_phys(&matching, &phys));
        assert!(!window_cursor_visual_matches_phys(&mismatched, &phys));
    }
}

/// A merged span of adjacent boxed glyphs on the same row.
struct BoxSpan {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    face_id: u32,
    bg: Option<Color>,
}
