//! Status line types and rendering for the Rust layout engine.
//!
//! Handles mode-line, header-line, and tab-line: type definitions,
//! face run parsing, and rendering into FrameGlyphBuffer.

use super::emacs_ffi::*;
use super::engine::LayoutEngine;
use super::types::*;
use super::unicode::decode_utf8;
use neomacs_display_protocol::frame_glyphs::FrameGlyphBuffer;
use neomacs_display_protocol::types::Color;

/// Which kind of status line to render.
pub(crate) enum StatusLineKind {
    ModeLine,
    HeaderLine,
    TabLine,
}

/// A face run within an overlay/display string: byte offset + fg/bg colors.
pub(crate) struct OverlayFaceRun {
    pub byte_offset: u16,
    pub fg: u32,
    pub bg: u32,
    /// Face has :extend attribute (bg extends to end of visual line)
    pub extend: bool,
}

/// Parse face runs appended after text in a buffer.
/// Runs are stored as 10-byte records: u16 byte_offset + u32 fg + u32 bg.
/// Bit 31 of bg encodes the :extend flag (1 = extends to end of line).
pub(crate) fn parse_overlay_face_runs(
    buf: &[u8],
    text_len: usize,
    nruns: i32,
) -> Vec<OverlayFaceRun> {
    let mut runs = Vec::with_capacity(nruns as usize);
    let runs_start = text_len;
    for ri in 0..nruns as usize {
        let off = runs_start + ri * 10;
        if off + 10 <= buf.len() {
            let byte_offset = u16::from_ne_bytes([buf[off], buf[off + 1]]);
            let fg = u32::from_ne_bytes([buf[off + 2], buf[off + 3], buf[off + 4], buf[off + 5]]);
            let raw_bg =
                u32::from_ne_bytes([buf[off + 6], buf[off + 7], buf[off + 8], buf[off + 9]]);
            let extend = (raw_bg & 0x80000000) != 0;
            let bg = raw_bg & 0x00FFFFFF;
            runs.push(OverlayFaceRun {
                byte_offset,
                fg,
                bg,
                extend,
            });
        }
    }
    runs
}

/// An align-to entry within an overlay string: byte offset + target column.
pub(crate) struct OverlayAlignEntry {
    pub byte_offset: u16,
    pub align_to_cols: f32,
}

/// Parse align-to entries appended after face runs in a buffer.
/// Entries are stored as 6-byte records: u16 byte_offset + f32 align_to_cols.
pub(crate) fn parse_overlay_align_entries(
    buf: &[u8],
    text_len: usize,
    nruns: i32,
    naligns: i32,
) -> Vec<OverlayAlignEntry> {
    let mut entries = Vec::with_capacity(naligns as usize);
    let aligns_start = text_len + nruns as usize * 10;
    for ai in 0..naligns as usize {
        let off = aligns_start + ai * 6;
        if off + 6 <= buf.len() {
            let byte_offset = u16::from_ne_bytes([buf[off], buf[off + 1]]);
            let align_to_cols =
                f32::from_ne_bytes([buf[off + 2], buf[off + 3], buf[off + 4], buf[off + 5]]);
            entries.push(OverlayAlignEntry {
                byte_offset,
                align_to_cols,
            });
        }
    }
    entries
}

/// Get the background color from the overlay face run covering the given byte index.
/// Returns the run's bg color if it has one, otherwise returns `fallback`.
/// This is used for align-to stretches within overlay strings to avoid
/// inheriting the buffer position's face (e.g., minibuffer-prompt).
pub(crate) fn overlay_run_bg_at(
    runs: &[OverlayFaceRun],
    byte_idx: usize,
    fallback: Color,
) -> Color {
    if runs.is_empty() {
        return fallback;
    }
    // Find the run covering byte_idx
    let mut cr = 0;
    while cr + 1 < runs.len() && byte_idx >= runs[cr + 1].byte_offset as usize {
        cr += 1;
    }
    if byte_idx >= runs[cr].byte_offset as usize && runs[cr].bg != 0 {
        Color::from_pixel(runs[cr].bg)
    } else {
        fallback
    }
}

/// Get the background color and extend flag from the overlay face run at byte_idx.
/// Returns (bg_color, extend) if a run covers byte_idx, otherwise None.
pub(crate) fn overlay_run_bg_extend_at(
    runs: &[OverlayFaceRun],
    byte_idx: usize,
) -> Option<(Color, bool)> {
    if runs.is_empty() {
        return None;
    }
    let mut cr = 0;
    while cr + 1 < runs.len() && byte_idx >= runs[cr + 1].byte_offset as usize {
        cr += 1;
    }
    if byte_idx >= runs[cr].byte_offset as usize && runs[cr].bg != 0 {
        Some((Color::from_pixel(runs[cr].bg), runs[cr].extend))
    } else {
        None
    }
}

/// Apply the face run covering the current byte index.
/// Returns the updated current_run index.
pub(crate) fn apply_overlay_face_run(
    runs: &[OverlayFaceRun],
    byte_idx: usize,
    current_run: usize,
    frame_glyphs: &mut FrameGlyphBuffer,
) -> usize {
    let mut cr = current_run;
    // Advance to the correct run
    while cr + 1 < runs.len() && byte_idx >= runs[cr + 1].byte_offset as usize {
        cr += 1;
    }
    if byte_idx >= runs[cr].byte_offset as usize {
        let run = &runs[cr];
        if run.fg != 0 || run.bg != 0 {
            let rfg = Color::from_pixel(run.fg);
            let rbg = Color::from_pixel(run.bg);
            frame_glyphs.set_face(0, rfg, Some(rbg), 400, false, 0, None, 0, None, 0, None);
        }
        // Pre-advance if next run starts at next byte
        if cr + 1 < runs.len() && byte_idx + 1 >= runs[cr + 1].byte_offset as usize {
            cr += 1;
        }
    }
    cr
}

/// A display property record extracted from a mode-line string.
/// Each record is 16 bytes: u16 byte_offset, u16 covers_bytes,
/// u32 gpu_id, u16 width, u16 height, u16 ascent, u16 pad.
struct DisplayPropRecord {
    byte_offset: u16,
    covers_bytes: u16,
    gpu_id: u32,
    width: u16,
    height: u16,
    ascent: u16,
}

/// Parse display property records appended after face runs in a buffer.
fn parse_display_props(buf: &[u8], start: usize, count: usize) -> Vec<DisplayPropRecord> {
    let mut props = Vec::with_capacity(count);
    for i in 0..count {
        let off = start + i * 16;
        if off + 16 <= buf.len() {
            props.push(DisplayPropRecord {
                byte_offset: u16::from_ne_bytes([buf[off], buf[off + 1]]),
                covers_bytes: u16::from_ne_bytes([buf[off + 2], buf[off + 3]]),
                gpu_id: u32::from_ne_bytes([
                    buf[off + 4],
                    buf[off + 5],
                    buf[off + 6],
                    buf[off + 7],
                ]),
                width: u16::from_ne_bytes([buf[off + 8], buf[off + 9]]),
                height: u16::from_ne_bytes([buf[off + 10], buf[off + 11]]),
                ascent: u16::from_ne_bytes([buf[off + 12], buf[off + 13]]),
            });
        }
    }
    props
}

impl LayoutEngine {
    /// Render a status line (mode-line, header-line, or tab-line).
    pub(crate) unsafe fn render_status_line(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        char_w: f32,
        ascent: f32,
        wp: &WindowParamsFFI,
        frame: EmacsFrame,
        frame_glyphs: &mut FrameGlyphBuffer,
        kind: StatusLineKind,
    ) {
        let mut line_face = FaceDataFFI::default();
        let buf_size = 4096usize;
        let mut line_buf = vec![0u8; buf_size];

        let bytes = match kind {
            StatusLineKind::TabLine => neomacs_layout_tab_line_text(
                wp.window_ptr,
                std::ptr::null_mut(),
                line_buf.as_mut_ptr(),
                buf_size as i64,
                &mut line_face,
            ),
            StatusLineKind::HeaderLine => neomacs_layout_header_line_text(
                wp.window_ptr,
                std::ptr::null_mut(),
                line_buf.as_mut_ptr(),
                buf_size as i64,
                &mut line_face,
            ),
            StatusLineKind::ModeLine => neomacs_layout_mode_line_text(
                wp.window_ptr,
                std::ptr::null_mut(),
                line_buf.as_mut_ptr(),
                buf_size as i64,
                &mut line_face,
            ),
        };

        // Apply face
        self.apply_face(&line_face, frame, frame_glyphs);
        let bg = Color::from_pixel(line_face.bg);
        let default_fg = Color::from_pixel(line_face.fg);

        // Use the mode-line face's own font metrics instead of the window's
        // text-scaled values.  text-scale-adjust changes the window's default
        // face size but the mode-line face renders at its own (unscaled) size.
        let char_w = if line_face.font_char_width > 0.0 {
            line_face.font_char_width
        } else {
            char_w
        };
        let ascent = if line_face.font_ascent > 0.0 {
            line_face.font_ascent
        } else {
            ascent
        };

        // Vertical text inset within the mode line area.
        // When box_h_line_width > 0, estimate_mode_line_height() added
        // 2 * box_h_line_width to the area height for top/bottom borders.
        // Text starts after the top border line.
        // When box_h_line_width <= 0 (drawn within, or no box), the mode
        // line area is exactly font height — no inset, text fills the area.
        let inset = if line_face.box_h_line_width > 0 {
            line_face.box_h_line_width as f32
        } else {
            0.0
        };
        let text_y = y + inset;

        // Draw background
        Self::add_stretch_for_face(
            &line_face,
            frame_glyphs,
            x,
            y,
            width,
            height,
            bg,
            line_face.face_id,
            true,
        );

        if bytes <= 0 {
            return;
        }

        // Extract text length, face run count, display prop count, and align
        // count from packed return value:
        //   bits  0-31 = text_len
        //   bits 32-47 = nruns (face runs)
        //   bits 48-55 = ndisplay (image display props)
        //   bits 56-63 = naligns (align-to entries)
        let text_len = (bytes & 0xFFFFFFFF) as usize;
        let nruns = ((bytes >> 32) & 0xFFFF) as usize;
        let ndisplay = ((bytes >> 48) & 0xFF) as usize;
        let naligns = ((bytes >> 56) & 0xFF) as usize;

        let text = &line_buf[..text_len];

        // Parse face runs: each run is 10 bytes (u16 byte_offset, u32 fg, u32 bg)
        // stored after text data
        struct FaceRun {
            byte_offset: u16,
            fg: u32,
            bg: u32,
        }
        let mut face_runs: Vec<FaceRun> = Vec::with_capacity(nruns);
        if nruns > 0 {
            let runs_start = text_len;
            for i in 0..nruns {
                let off = runs_start + i * 10;
                if off + 10 <= line_buf.len() {
                    let byte_offset = u16::from_ne_bytes([line_buf[off], line_buf[off + 1]]);
                    let fg = u32::from_ne_bytes([
                        line_buf[off + 2],
                        line_buf[off + 3],
                        line_buf[off + 4],
                        line_buf[off + 5],
                    ]);
                    let bg_val = u32::from_ne_bytes([
                        line_buf[off + 6],
                        line_buf[off + 7],
                        line_buf[off + 8],
                        line_buf[off + 9],
                    ]);
                    face_runs.push(FaceRun {
                        byte_offset,
                        fg,
                        bg: bg_val,
                    });
                }
            }
        }

        // Parse display property records (images) after face runs.
        // Each record is 16 bytes, stored after the face run area.
        let display_start = text_len + nruns * 10;
        let display_props = parse_display_props(&line_buf, display_start, ndisplay);

        // Parse align-to entries after display props.
        // Each entry is 6 bytes: u16 byte_offset + f32 align_to_cols.
        let align_start = display_start + ndisplay * 16;
        let align_entries = if naligns > 0 {
            let mut entries = Vec::with_capacity(naligns);
            for i in 0..naligns {
                let off = align_start + i * 6;
                if off + 6 <= line_buf.len() {
                    let byte_offset = u16::from_ne_bytes([line_buf[off], line_buf[off + 1]]);
                    let align_to_cols = f32::from_ne_bytes([
                        line_buf[off + 2],
                        line_buf[off + 3],
                        line_buf[off + 4],
                        line_buf[off + 5],
                    ]);
                    entries.push(OverlayAlignEntry {
                        byte_offset,
                        align_to_cols,
                    });
                }
            }
            entries
        } else {
            Vec::new()
        };

        // Use the mode-line face for character width queries
        let face_id = line_face.face_id;
        let window = wp.window_ptr;

        // Render text with face runs, display properties, and align-to entries
        let mut sl_x_offset: f32 = 0.0;
        let mut byte_idx = 0usize;
        let mut current_run = 0usize;
        let mut dp_idx = 0usize; // current display prop index
        let mut align_idx = 0usize; // current align-to entry index

        while byte_idx < text.len() && sl_x_offset < width {
            // Check if an align-to entry matches this byte position.
            // If so, jump x offset to the target column and skip the char.
            if align_idx < align_entries.len()
                && byte_idx == align_entries[align_idx].byte_offset as usize
            {
                let target_x = align_entries[align_idx].align_to_cols * char_w;
                if target_x > sl_x_offset {
                    let stretch_w = target_x - sl_x_offset;
                    Self::add_stretch_for_face(
                        &line_face,
                        frame_glyphs,
                        x + sl_x_offset,
                        y,
                        stretch_w,
                        height,
                        bg,
                        line_face.face_id,
                        true,
                    );
                    sl_x_offset = target_x;
                }
                align_idx += 1;
                // Skip the character that has the display property
                let (_ch, ch_len) = decode_utf8(&text[byte_idx..]);
                byte_idx += ch_len;
                continue;
            }

            // Check if a display property (image) covers this byte position.
            // If so, render the image and skip the covered bytes.
            if dp_idx < display_props.len() {
                let dp = &display_props[dp_idx];
                if byte_idx == dp.byte_offset as usize {
                    if dp.gpu_id != 0 && dp.width > 0 && dp.height > 0 {
                        let img_w = dp.width as f32;
                        let img_h = dp.height as f32;
                        let gx = x + sl_x_offset;

                        // Vertical alignment: use ascent-based positioning
                        let gy = if img_h <= height {
                            let img_ascent_px = if dp.ascent == 0xFFFF {
                                // Centered
                                (img_h + ascent - (height - ascent) + 1.0) / 2.0
                            } else {
                                // Percentage
                                img_h * (dp.ascent as f32 / 100.0)
                            };
                            text_y + ascent - img_ascent_px
                        } else {
                            text_y
                        };

                        frame_glyphs.add_image(dp.gpu_id, gx, gy, img_w, img_h);
                        sl_x_offset += img_w;
                    }
                    // Skip covered bytes
                    byte_idx = (dp.byte_offset + dp.covers_bytes) as usize;
                    dp_idx += 1;
                    continue;
                }
            }

            // Check if we need to switch face for this byte position
            if current_run < face_runs.len() {
                if byte_idx >= face_runs[current_run].byte_offset as usize {
                    // Check if next run starts here or we're in current run
                    if current_run + 1 < face_runs.len()
                        && byte_idx >= face_runs[current_run + 1].byte_offset as usize
                    {
                        current_run += 1;
                    }
                    let run = &face_runs[current_run];
                    if run.fg != 0 || run.bg != 0 {
                        let run_fg = Color::from_pixel(run.fg);
                        let run_bg = Color::from_pixel(run.bg);
                        frame_glyphs.set_face(
                            line_face.face_id,
                            run_fg,
                            Some(run_bg),
                            400,
                            false,
                            0,
                            None,
                            0,
                            None,
                            0,
                            None,
                        );
                    }
                }
            }

            let (ch, ch_len) = decode_utf8(&text[byte_idx..]);
            byte_idx += ch_len;

            if ch == '\n' || ch == '\r' {
                continue;
            }

            // Use actual glyph width from the font instead of fixed char_w.
            // This handles variable-width characters (icons, CJK) correctly.
            let advance = {
                let cp = ch as u32;
                if cp < 128 {
                    // ASCII: use cached width via text_extents()
                    let cache_key = (face_id, line_face.font_size);
                    if !self.ascii_width_cache.contains_key(&cache_key) {
                        let mut widths = [0.0f32; 128];
                        neomacs_layout_fill_ascii_widths(
                            window,
                            face_id as std::os::raw::c_int,
                            widths.as_mut_ptr(),
                        );
                        for w in widths.iter_mut() {
                            if *w < 0.0 {
                                *w = char_w;
                            }
                        }
                        self.ascii_width_cache.insert(cache_key, widths);
                    }
                    self.ascii_width_cache[&cache_key][cp as usize]
                } else {
                    // Non-ASCII: query individually
                    let w = neomacs_layout_char_width(
                        window,
                        cp as std::os::raw::c_int,
                        face_id as std::os::raw::c_int,
                    );
                    if w > 0.0 { w } else { char_w }
                }
            };

            let gx = x + sl_x_offset;
            frame_glyphs.add_char(ch, gx, text_y, advance, height, ascent, true);
            sl_x_offset += advance;
        }

        // Restore default mode-line face
        frame_glyphs.set_face(
            line_face.face_id,
            default_fg,
            Some(bg),
            400,
            false,
            0,
            None,
            0,
            None,
            0,
            None,
        );

        // Fill remaining with background
        if sl_x_offset < width {
            let gx = x + sl_x_offset;
            let remaining = width - sl_x_offset;
            Self::add_stretch_for_face(
                &line_face,
                frame_glyphs,
                gx,
                y,
                remaining,
                height,
                bg,
                line_face.face_id,
                true,
            );
        }

        // Box borders are rendered by the renderer's box span detection
        // (supports both sharp and SDF rounded corners).
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neomacs_display_protocol::frame_glyphs::FrameGlyph;
    use neomacs_display_protocol::types::Color;

    // ---------------------------------------------------------------
    // Helper: build a 10-byte face run record (native-endian)
    // ---------------------------------------------------------------
    fn make_run_bytes(byte_offset: u16, fg: u32, bg: u32) -> [u8; 10] {
        let mut rec = [0u8; 10];
        rec[0..2].copy_from_slice(&byte_offset.to_ne_bytes());
        rec[2..6].copy_from_slice(&fg.to_ne_bytes());
        rec[6..10].copy_from_slice(&bg.to_ne_bytes());
        rec
    }

    // ---------------------------------------------------------------
    // StatusLineKind enum
    // ---------------------------------------------------------------

    #[test]
    fn status_line_kind_variants_exist() {
        // Ensure all three variants can be constructed (compile-time check
        // made explicit).
        let _ml = StatusLineKind::ModeLine;
        let _hl = StatusLineKind::HeaderLine;
        let _tl = StatusLineKind::TabLine;
    }

    #[test]
    fn status_line_kind_is_distinct() {
        // Discriminants should differ (match each variant).
        let check = |k: &StatusLineKind| -> u8 {
            match k {
                StatusLineKind::ModeLine => 0,
                StatusLineKind::HeaderLine => 1,
                StatusLineKind::TabLine => 2,
            }
        };
        assert_eq!(check(&StatusLineKind::ModeLine), 0);
        assert_eq!(check(&StatusLineKind::HeaderLine), 1);
        assert_eq!(check(&StatusLineKind::TabLine), 2);
    }

    // ---------------------------------------------------------------
    // OverlayFaceRun struct
    // ---------------------------------------------------------------

    #[test]
    fn overlay_face_run_construction_defaults() {
        let run = OverlayFaceRun {
            byte_offset: 0,
            fg: 0,
            bg: 0,
            extend: false,
        };
        assert_eq!(run.byte_offset, 0);
        assert_eq!(run.fg, 0);
        assert_eq!(run.bg, 0);
        assert_eq!(run.extend, false);
    }

    #[test]
    fn overlay_face_run_construction_max_values() {
        let run = OverlayFaceRun {
            byte_offset: u16::MAX,
            fg: u32::MAX,
            bg: u32::MAX,
            extend: true,
        };
        assert_eq!(run.byte_offset, u16::MAX);
        assert_eq!(run.fg, u32::MAX);
        assert_eq!(run.bg, u32::MAX);
        assert_eq!(run.extend, true);
    }

    #[test]
    fn overlay_face_run_construction_typical() {
        // Typical Emacs color values: 0x00RRGGBB
        let run = OverlayFaceRun {
            byte_offset: 42,
            fg: 0x00FFFFFF,
            bg: 0x00000000,
            extend: false,
        };
        assert_eq!(run.byte_offset, 42);
        assert_eq!(run.fg, 0x00FFFFFF);
        assert_eq!(run.bg, 0x00000000);
        assert_eq!(run.extend, false);
    }

    // ---------------------------------------------------------------
    // parse_overlay_face_runs: empty / zero
    // ---------------------------------------------------------------

    #[test]
    fn parse_empty_buffer_zero_runs() {
        let buf: &[u8] = &[];
        let runs = parse_overlay_face_runs(buf, 0, 0);
        assert!(runs.is_empty());
    }

    #[test]
    fn parse_zero_runs_with_text() {
        // Buffer has text but no face runs requested.
        let buf = b"Hello, world!";
        let runs = parse_overlay_face_runs(buf, buf.len(), 0);
        assert!(runs.is_empty());
    }

    // ---------------------------------------------------------------
    // parse_overlay_face_runs: single run
    // ---------------------------------------------------------------

    #[test]
    fn parse_single_run() {
        let text = b"Hello";
        let text_len = text.len(); // 5
        let rec = make_run_bytes(0, 0x00FF0000, 0x0000FF00);

        let mut buf = Vec::from(&text[..]);
        buf.extend_from_slice(&rec);

        let runs = parse_overlay_face_runs(&buf, text_len, 1);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].byte_offset, 0);
        assert_eq!(runs[0].fg, 0x00FF0000);
        assert_eq!(runs[0].bg, 0x0000FF00);
    }

    #[test]
    fn parse_single_run_nonzero_offset() {
        let text = b"ABCDEF";
        let text_len = text.len(); // 6
        // Use 24-bit bg (realistic sRGB). Bit 31 = 0 → extend = false.
        let rec = make_run_bytes(3, 0xAABBCCDD, 0x00223344);

        let mut buf = Vec::from(&text[..]);
        buf.extend_from_slice(&rec);

        let runs = parse_overlay_face_runs(&buf, text_len, 1);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].byte_offset, 3);
        assert_eq!(runs[0].fg, 0xAABBCCDD);
        assert_eq!(runs[0].bg, 0x00223344);
        assert_eq!(runs[0].extend, false);
    }

    // ---------------------------------------------------------------
    // parse_overlay_face_runs: multiple runs
    // ---------------------------------------------------------------

    #[test]
    fn parse_multiple_runs() {
        let text = b"mode-line text here";
        let text_len = text.len();

        let r0 = make_run_bytes(0, 0x00FFFFFF, 0x00000000);
        let r1 = make_run_bytes(10, 0x0000FF00, 0x00FF0000);
        let r2 = make_run_bytes(15, 0x000000FF, 0x00FFFF00);

        let mut buf = Vec::from(&text[..]);
        buf.extend_from_slice(&r0);
        buf.extend_from_slice(&r1);
        buf.extend_from_slice(&r2);

        let runs = parse_overlay_face_runs(&buf, text_len, 3);
        assert_eq!(runs.len(), 3);

        assert_eq!(runs[0].byte_offset, 0);
        assert_eq!(runs[0].fg, 0x00FFFFFF);
        assert_eq!(runs[0].bg, 0x00000000);

        assert_eq!(runs[1].byte_offset, 10);
        assert_eq!(runs[1].fg, 0x0000FF00);
        assert_eq!(runs[1].bg, 0x00FF0000);

        assert_eq!(runs[2].byte_offset, 15);
        assert_eq!(runs[2].fg, 0x000000FF);
        assert_eq!(runs[2].bg, 0x00FFFF00);
    }

    // ---------------------------------------------------------------
    // parse_overlay_face_runs: truncated data
    // ---------------------------------------------------------------

    #[test]
    fn parse_truncated_single_run() {
        // Buffer has text but only 5 bytes of run data (needs 10).
        let text = b"ABC";
        let text_len = text.len();
        let mut buf = Vec::from(&text[..]);
        buf.extend_from_slice(&[0u8; 5]); // only half a record

        let runs = parse_overlay_face_runs(&buf, text_len, 1);
        assert!(runs.is_empty(), "truncated record should be skipped");
    }

    #[test]
    fn parse_truncated_second_run() {
        // First record is complete, second is truncated.
        let text = b"ABCD";
        let text_len = text.len();
        let rec0 = make_run_bytes(0, 0x11111111, 0x22222222);

        let mut buf = Vec::from(&text[..]);
        buf.extend_from_slice(&rec0);
        buf.extend_from_slice(&[0xFFu8; 7]); // 7 bytes, need 10

        let runs = parse_overlay_face_runs(&buf, text_len, 2);
        assert_eq!(runs.len(), 1, "only the first complete record should parse");
        assert_eq!(runs[0].fg, 0x11111111);
    }

    #[test]
    fn parse_nruns_exceeds_buffer() {
        // nruns claims 5 records but buffer only has space for 2.
        let text = b"XY";
        let text_len = text.len();
        let r0 = make_run_bytes(0, 1, 2);
        let r1 = make_run_bytes(1, 3, 4);

        let mut buf = Vec::from(&text[..]);
        buf.extend_from_slice(&r0);
        buf.extend_from_slice(&r1);

        let runs = parse_overlay_face_runs(&buf, text_len, 5);
        assert_eq!(runs.len(), 2, "should only parse records that fit");
        assert_eq!(runs[0].fg, 1);
        assert_eq!(runs[1].fg, 3);
    }

    // ---------------------------------------------------------------
    // parse_overlay_face_runs: zero text_len (runs start at offset 0)
    // ---------------------------------------------------------------

    #[test]
    fn parse_zero_text_len() {
        // No text at all; runs start at offset 0 in the buffer.
        // 0xCAFEBABE has bit 31 set → extend = true, bg = lower 24 bits.
        let rec = make_run_bytes(0, 0xDEADBEEF, 0xCAFEBABE);
        let buf = Vec::from(&rec[..]);

        let runs = parse_overlay_face_runs(&buf, 0, 1);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].fg, 0xDEADBEEF);
        assert_eq!(runs[0].bg, 0x00FEBABE); // lower 24 bits of 0xCAFEBABE
        assert_eq!(runs[0].extend, true); // bit 31 was set
    }

    // ---------------------------------------------------------------
    // parse_overlay_face_runs: endianness verification
    // ---------------------------------------------------------------

    #[test]
    fn parse_verifies_native_endian_u16() {
        // The u16 byte_offset is stored as native-endian bytes.
        // Build a buffer where byte_offset = 0x0102 and verify it
        // decodes correctly on the current platform.
        let expected: u16 = 0x0102;
        let rec = make_run_bytes(expected, 0, 0);
        let buf = Vec::from(&rec[..]);

        let runs = parse_overlay_face_runs(&buf, 0, 1);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].byte_offset, expected);
    }

    #[test]
    fn parse_verifies_native_endian_u32() {
        // Similarly for u32 fg/bg.
        // Use 24-bit bg to avoid extend bit masking.
        let fg_expected: u32 = 0x01020304;
        let bg_expected: u32 = 0x00060708;
        let rec = make_run_bytes(0, fg_expected, bg_expected);
        let buf = Vec::from(&rec[..]);

        let runs = parse_overlay_face_runs(&buf, 0, 1);
        assert_eq!(runs[0].fg, fg_expected);
        assert_eq!(runs[0].bg, bg_expected);
        assert_eq!(runs[0].extend, false);
    }

    // ---------------------------------------------------------------
    // parse_overlay_face_runs: exact boundary (off + 10 == buf.len())
    // ---------------------------------------------------------------

    #[test]
    fn parse_exact_fit() {
        // Buffer is exactly text_len + 10 bytes — the run should parse.
        let text = b"T";
        let text_len = text.len(); // 1
        let rec = make_run_bytes(0, 42, 99);
        let mut buf = Vec::from(&text[..]);
        buf.extend_from_slice(&rec);
        assert_eq!(buf.len(), text_len + 10);

        let runs = parse_overlay_face_runs(&buf, text_len, 1);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].fg, 42);
        assert_eq!(runs[0].bg, 99);
    }

    #[test]
    fn parse_one_byte_short() {
        // Buffer is text_len + 9 bytes — one byte short, run should NOT parse.
        let text = b"T";
        let text_len = text.len();
        let mut buf = Vec::from(&text[..]);
        buf.extend_from_slice(&[0u8; 9]);
        assert_eq!(buf.len(), text_len + 9);

        let runs = parse_overlay_face_runs(&buf, text_len, 1);
        assert!(runs.is_empty());
    }

    // ---------------------------------------------------------------
    // apply_overlay_face_run: basic advancement
    // ---------------------------------------------------------------

    #[test]
    fn apply_overlay_single_run_before_offset() {
        // byte_idx < run.byte_offset  =>  no face change, cr unchanged.
        let runs = vec![OverlayFaceRun {
            byte_offset: 5,
            fg: 0x00FF0000,
            bg: 0x00000000,
            extend: false,
        }];
        let mut fgb = FrameGlyphBuffer::new();

        // byte_idx = 0, which is < 5
        let cr = apply_overlay_face_run(&runs, 0, 0, &mut fgb);
        // Since byte_idx (0) < runs[0].byte_offset (5), the condition at
        // line 57 (`byte_idx >= runs[cr].byte_offset`) is false,
        // so the function just returns cr unchanged.
        assert_eq!(cr, 0);
    }

    #[test]
    fn apply_overlay_single_run_at_offset() {
        // byte_idx == run.byte_offset  =>  face applied, cr stays 0.
        let runs = vec![OverlayFaceRun {
            byte_offset: 5,
            fg: 0x00FF0000,
            bg: 0x0000FF00,
            extend: false,
        }];
        let mut fgb = FrameGlyphBuffer::new();

        let cr = apply_overlay_face_run(&runs, 5, 0, &mut fgb);
        assert_eq!(cr, 0);
    }

    #[test]
    fn apply_overlay_single_run_past_offset() {
        let runs = vec![OverlayFaceRun {
            byte_offset: 5,
            fg: 0x00FF0000,
            bg: 0x0000FF00,
            extend: false,
        }];
        let mut fgb = FrameGlyphBuffer::new();

        let cr = apply_overlay_face_run(&runs, 10, 0, &mut fgb);
        assert_eq!(cr, 0);
    }

    #[test]
    fn apply_overlay_multiple_runs_advance() {
        let runs = vec![
            OverlayFaceRun {
                byte_offset: 0,
                fg: 0x00FF0000,
                bg: 0x00000000,
                extend: false,
            },
            OverlayFaceRun {
                byte_offset: 5,
                fg: 0x0000FF00,
                bg: 0x00000000,
                extend: false,
            },
            OverlayFaceRun {
                byte_offset: 10,
                fg: 0x000000FF,
                bg: 0x00000000,
                extend: false,
            },
        ];
        let mut fgb = FrameGlyphBuffer::new();

        // byte_idx=0 => should stay at run 0
        let cr = apply_overlay_face_run(&runs, 0, 0, &mut fgb);
        assert_eq!(cr, 0);

        // byte_idx=5 => should advance to run 1
        let cr = apply_overlay_face_run(&runs, 5, 0, &mut fgb);
        assert_eq!(cr, 1);

        // byte_idx=10 => should advance to run 2
        let cr = apply_overlay_face_run(&runs, 10, 0, &mut fgb);
        assert_eq!(cr, 2);
    }

    #[test]
    fn apply_overlay_pre_advance_to_next_byte() {
        // Test the pre-advance logic: if byte_idx + 1 >= next run's byte_offset,
        // cr is pre-advanced.
        let runs = vec![
            OverlayFaceRun {
                byte_offset: 0,
                fg: 1,
                bg: 0,
                extend: false,
            },
            OverlayFaceRun {
                byte_offset: 5,
                fg: 2,
                bg: 0,
                extend: false,
            },
        ];
        let mut fgb = FrameGlyphBuffer::new();

        // byte_idx=4, cr=0: byte_idx(4) >= runs[0].byte_offset(0) => face applied.
        // Pre-advance: byte_idx+1=5 >= runs[1].byte_offset(5) => cr becomes 1.
        let cr = apply_overlay_face_run(&runs, 4, 0, &mut fgb);
        assert_eq!(cr, 1, "should pre-advance when byte_idx+1 reaches next run");
    }

    /// Helper: add a dummy char glyph and return its (fg, bg) from the glyph.
    fn snapshot_face(fgb: &mut FrameGlyphBuffer) -> (Color, Option<Color>) {
        fgb.add_char('X', 0.0, 0.0, 8.0, 16.0, 12.0, false);
        let glyph = fgb.glyphs.last().unwrap();
        match glyph {
            FrameGlyph::Char { fg, bg, .. } => (*fg, *bg),
            _ => panic!("expected Char glyph"),
        }
    }

    #[test]
    fn apply_overlay_zero_fg_bg_no_face_change() {
        // When both fg and bg are 0, set_face should NOT be called
        // (the early-return `if run.fg != 0 || run.bg != 0` skips it).
        let runs = vec![OverlayFaceRun {
            byte_offset: 0,
            fg: 0,
            bg: 0,
            extend: false,
        }];
        let mut fgb = FrameGlyphBuffer::new();
        // Record initial state by snapshotting via a glyph
        let (initial_fg, initial_bg) = snapshot_face(&mut fgb);

        let cr = apply_overlay_face_run(&runs, 0, 0, &mut fgb);
        assert_eq!(cr, 0);

        // Snapshot again — should be unchanged
        let (after_fg, after_bg) = snapshot_face(&mut fgb);
        assert_eq!(after_fg, initial_fg);
        assert_eq!(after_bg, initial_bg);
    }

    #[test]
    fn apply_overlay_fg_nonzero_bg_zero_still_applies() {
        // fg != 0 || bg != 0 is true when only fg is nonzero
        let runs = vec![OverlayFaceRun {
            byte_offset: 0,
            fg: 0x00FF0000,
            bg: 0,
            extend: false,
        }];
        let mut fgb = FrameGlyphBuffer::new();
        let (initial_fg, _) = snapshot_face(&mut fgb);

        let _cr = apply_overlay_face_run(&runs, 0, 0, &mut fgb);

        let (after_fg, _) = snapshot_face(&mut fgb);
        // Face fg should have been changed (from_pixel(0x00FF0000) != initial WHITE)
        assert_ne!(after_fg, initial_fg);
    }

    #[test]
    fn apply_overlay_fg_zero_bg_nonzero_still_applies() {
        // fg != 0 || bg != 0 is true when only bg is nonzero
        let runs = vec![OverlayFaceRun {
            byte_offset: 0,
            fg: 0,
            bg: 0x00FF0000,
            extend: false,
        }];
        let mut fgb = FrameGlyphBuffer::new();

        let _cr = apply_overlay_face_run(&runs, 0, 0, &mut fgb);

        let (_, after_bg) = snapshot_face(&mut fgb);
        // bg should have been set to Some(...)
        assert!(after_bg.is_some());
    }

    // ---------------------------------------------------------------
    // parse_overlay_face_runs: stress / many runs
    // ---------------------------------------------------------------

    #[test]
    fn parse_many_runs() {
        let text = b"";
        let text_len = 0;
        let n = 100;

        let mut buf = Vec::new();
        for i in 0..n {
            let rec = make_run_bytes(i as u16, i as u32 * 100, i as u32 * 200);
            buf.extend_from_slice(&rec);
        }

        let runs = parse_overlay_face_runs(&buf, text_len, n);
        assert_eq!(runs.len(), n as usize);

        for i in 0..n as usize {
            assert_eq!(runs[i].byte_offset, i as u16);
            assert_eq!(runs[i].fg, i as u32 * 100);
            assert_eq!(runs[i].bg, i as u32 * 200);
        }
    }

    // ---------------------------------------------------------------
    // parse_overlay_face_runs: large text_len offset
    // ---------------------------------------------------------------

    #[test]
    fn parse_large_text_offset() {
        // Simulate a buffer where 500 bytes are text, followed by 1 run.
        let text_len = 500;
        let mut buf = vec![0x41u8; text_len]; // 'A' * 500
        let rec = make_run_bytes(100, 0xDEAD, 0xBEEF);
        buf.extend_from_slice(&rec);

        let runs = parse_overlay_face_runs(&buf, text_len, 1);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].byte_offset, 100);
        assert_eq!(runs[0].fg, 0xDEAD);
        assert_eq!(runs[0].bg, 0xBEEF);
    }

    // ---------------------------------------------------------------
    // apply_overlay_face_run: starting from non-zero current_run
    // ---------------------------------------------------------------

    #[test]
    fn apply_overlay_start_from_middle_run() {
        let runs = vec![
            OverlayFaceRun {
                byte_offset: 0,
                fg: 1,
                bg: 0,
                extend: false,
            },
            OverlayFaceRun {
                byte_offset: 5,
                fg: 2,
                bg: 0,
                extend: false,
            },
            OverlayFaceRun {
                byte_offset: 10,
                fg: 3,
                bg: 0,
                extend: false,
            },
        ];
        let mut fgb = FrameGlyphBuffer::new();

        // Start at current_run=1, byte_idx=10 => should advance to run 2
        let cr = apply_overlay_face_run(&runs, 10, 1, &mut fgb);
        assert_eq!(cr, 2);
    }

    #[test]
    fn apply_overlay_start_at_last_run() {
        let runs = vec![
            OverlayFaceRun {
                byte_offset: 0,
                fg: 1,
                bg: 0,
                extend: false,
            },
            OverlayFaceRun {
                byte_offset: 5,
                fg: 2,
                bg: 0,
                extend: false,
            },
        ];
        let mut fgb = FrameGlyphBuffer::new();

        // Already at last run, byte_idx well past it
        let cr = apply_overlay_face_run(&runs, 100, 1, &mut fgb);
        assert_eq!(cr, 1);
    }
}
