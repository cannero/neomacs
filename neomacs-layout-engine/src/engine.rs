//! The Rust layout engine — Phase 1+2: Monospace layout with face resolution.
//!
//! Reads buffer text via FFI, resolves faces per character position,
//! computes line breaks, positions glyphs on a fixed-width grid, and
//! produces FrameGlyphBuffer compatible with the existing wgpu renderer.

use std::ffi::CStr;
use std::ffi::c_int;

use super::bidi_layout::reorder_row_bidi;
use super::emacs_ffi::*;
use super::font_metrics::FontMetricsService;
use super::hit_test::*;
use super::status_line::*;
use super::types::*;
use super::unicode::*;
use neomacs_display_protocol::face::{BoxType, Face, FaceAttributes, UnderlineStyle};
use neomacs_display_protocol::frame_glyphs::{
    CursorStyle, FrameGlyphBuffer, GlyphRowRole, WindowEffectHint, WindowInfo,
    WindowTransitionHint, WindowTransitionKind,
};
use neomacs_display_protocol::types::{Color, Rect};
use neovm_core::buffer::BufferId;
use neovm_core::emacs_core::Value;
use neovm_core::emacs_core::expr::Expr;
use neovm_core::emacs_core::intern::intern;

/// Maximum number of characters in a ligature run before forced flush.
const MAX_LIGATURE_RUN_LEN: usize = 64;

/// Buffer for accumulating same-face text runs for ligature shaping.
struct LigatureRunBuffer {
    chars: Vec<char>,
    advances: Vec<f32>,
    start_x: f32,
    start_y: f32,
    face_h: f32,
    face_ascent: f32,
    face_id: u32,
    total_advance: f32,
    is_overlay: bool,
    height_scale: f32,
}

fn eval_status_line_format(
    evaluator: &mut neovm_core::emacs_core::Evaluator,
    format_symbol: &str,
    window_id: i64,
    buffer_id: u64,
) -> Option<String> {
    evaluator.setup_thread_locals();
    let expr = Expr::List(vec![
        Expr::Symbol(intern("format-mode-line")),
        Expr::Symbol(intern(format_symbol)),
        Expr::Bool(false),
        Expr::OpaqueValue(Value::Window(window_id as u64)),
        Expr::OpaqueValue(Value::Buffer(BufferId(buffer_id))),
    ]);
    evaluator
        .eval_expr(&expr)
        .ok()
        .and_then(|val| val.as_str_owned())
        .filter(|s| !s.is_empty())
}

impl LigatureRunBuffer {
    fn new() -> Self {
        Self {
            chars: Vec::with_capacity(MAX_LIGATURE_RUN_LEN),
            advances: Vec::with_capacity(MAX_LIGATURE_RUN_LEN),
            start_x: 0.0,
            start_y: 0.0,
            face_h: 0.0,
            face_ascent: 0.0,
            face_id: 0,
            total_advance: 0.0,
            is_overlay: false,
            height_scale: 0.0,
        }
    }

    fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    fn len(&self) -> usize {
        self.chars.len()
    }

    fn clear(&mut self) {
        self.chars.clear();
        self.advances.clear();
        self.total_advance = 0.0;
    }

    /// Push a character and its advance width into the run.
    fn push(&mut self, ch: char, advance: f32) {
        self.chars.push(ch);
        self.advances.push(advance);
        self.total_advance += advance;
    }

    /// Start a new run at the given position with the given face parameters.
    fn start(
        &mut self,
        x: f32,
        y: f32,
        face_h: f32,
        face_ascent: f32,
        face_id: u32,
        is_overlay: bool,
        height_scale: f32,
    ) {
        self.clear();
        self.start_x = x;
        self.start_y = y;
        self.face_h = face_h;
        self.face_ascent = face_ascent;
        self.face_id = face_id;
        self.is_overlay = is_overlay;
        self.height_scale = height_scale;
    }
}

/// Check if a character is a ligature-eligible symbol/punctuation.
/// Programming font ligatures only form between these characters.
#[inline]
fn is_ligature_char(ch: char) -> bool {
    matches!(
        ch,
        '!' | '#'
            | '$'
            | '%'
            | '&'
            | '*'
            | '+'
            | '-'
            | '.'
            | '/'
            | ':'
            | ';'
            | '<'
            | '='
            | '>'
            | '?'
            | '@'
            | '\\'
            | '^'
            | '|'
            | '~'
    )
}

/// Check if a run consists entirely of ligature-eligible characters.
/// Mixed runs (e.g., "arrow:" or "Font:") should NOT be composed,
/// only pure symbol runs (e.g., "->", "!=", "===").
#[inline]
fn run_is_pure_ligature(run: &LigatureRunBuffer) -> bool {
    run.chars.iter().all(|&ch| is_ligature_char(ch))
}

/// Flush the accumulated ligature run as either individual chars or a composed glyph.
fn flush_run(run: &LigatureRunBuffer, frame_glyphs: &mut FrameGlyphBuffer, ligatures: bool) {
    if run.is_empty() {
        return;
    }
    // Only compose runs of pure ligature-forming characters (punctuation/symbols).
    // Alphabetic/numeric runs are emitted as individual chars.
    let compose = ligatures && run.len() > 1 && run_is_pure_ligature(run);
    if !compose {
        // Emit individual chars (fallback / ligatures disabled / single char)
        let mut x = run.start_x;
        for (i, &ch) in run.chars.iter().enumerate() {
            let adv = run.advances[i];
            if run.height_scale > 0.0 && run.height_scale != 1.0 {
                let orig_size = frame_glyphs.font_size();
                frame_glyphs.set_font_size(orig_size * run.height_scale);
                frame_glyphs.add_char(
                    ch,
                    x,
                    run.start_y,
                    adv,
                    run.face_h,
                    run.face_ascent,
                    run.is_overlay,
                );
                frame_glyphs.set_font_size(orig_size);
            } else {
                frame_glyphs.add_char(
                    ch,
                    x,
                    run.start_y,
                    adv,
                    run.face_h,
                    run.face_ascent,
                    run.is_overlay,
                );
            }
            x += adv;
        }
    } else {
        // Emit as composed glyph — render thread will shape via HarfBuzz
        let text: String = run.chars.iter().collect();
        let base_char = run.chars[0];
        if run.height_scale > 0.0 && run.height_scale != 1.0 {
            let orig_size = frame_glyphs.font_size();
            frame_glyphs.set_font_size(orig_size * run.height_scale);
            frame_glyphs.add_composed_char(
                &text,
                base_char,
                run.start_x,
                run.start_y,
                run.total_advance,
                run.face_h,
                run.face_ascent,
                run.is_overlay,
            );
            frame_glyphs.set_font_size(orig_size);
        } else {
            frame_glyphs.add_composed_char(
                &text,
                base_char,
                run.start_x,
                run.start_y,
                run.total_advance,
                run.face_h,
                run.face_ascent,
                run.is_overlay,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Display property helpers
// ---------------------------------------------------------------------------

/// Check if a Value is a space display spec: a cons whose car is the symbol `space`.
/// e.g., `(space :width 5)` or `(space :align-to 40)`
fn is_display_space_spec(val: &neovm_core::emacs_core::Value) -> bool {
    if let neovm_core::emacs_core::Value::Cons(id) = val {
        let pair = neovm_core::emacs_core::value::read_cons(*id);
        return pair.car.is_symbol_named("space");
    }
    false
}

/// Parse width from a space display spec.
/// Handles `(space :width N)` and `(space :align-to COL)`.
/// `current_x` and `content_x` are used for `:align-to` calculations.
fn parse_display_space_width(
    val: &neovm_core::emacs_core::Value,
    char_w: f32,
    current_x: f32,
    content_x: f32,
) -> f32 {
    if let Some(items) = neovm_core::emacs_core::value::list_to_vec(val) {
        // items[0] is the symbol 'space', rest is plist
        let mut i = 1;
        while i + 1 < items.len() {
            if items[i].is_symbol_named(":width") {
                match items[i + 1] {
                    neovm_core::emacs_core::Value::Int(n) => return n as f32 * char_w,
                    neovm_core::emacs_core::Value::Float(f, _) => return f as f32 * char_w,
                    _ => {}
                }
            }
            if items[i].is_symbol_named(":align-to") {
                match items[i + 1] {
                    neovm_core::emacs_core::Value::Int(n) => {
                        let target_x = content_x + n as f32 * char_w;
                        return (target_x - current_x).max(0.0);
                    }
                    neovm_core::emacs_core::Value::Float(f, _) => {
                        let target_x = content_x + f as f32 * char_w;
                        return (target_x - current_x).max(0.0);
                    }
                    _ => {}
                }
            }
            i += 2;
        }
    }
    char_w // default: one character width
}

/// Check if a Value is an image display spec: a cons whose car is the symbol `image`.
/// e.g., `(image :type png :file "/path/to/image.png")`
fn is_display_image_spec(val: &neovm_core::emacs_core::Value) -> bool {
    if let neovm_core::emacs_core::Value::Cons(id) = val {
        let pair = neovm_core::emacs_core::value::read_cons(*id);
        return pair.car.is_symbol_named("image");
    }
    false
}

#[inline]
fn next_tab_stop_col(current_col: usize, tab_width: i32, tab_stop_list: &[i32]) -> usize {
    if !tab_stop_list.is_empty() {
        if let Some(&stop) = tab_stop_list
            .iter()
            .find(|&&stop| (stop as usize) > current_col)
        {
            return stop as usize;
        }
        let last = *tab_stop_list.last().unwrap() as usize;
        let tab_w = tab_width.max(1) as usize;
        if current_col >= last {
            return last + ((current_col - last) / tab_w + 1) * tab_w;
        }
        return last;
    }

    let tab_w = tab_width.max(1) as usize;
    ((current_col / tab_w) + 1) * tab_w
}

#[inline]
fn cursor_point_columns(text: &[u8], byte_idx: usize, col: i32, params: &WindowParams) -> usize {
    if byte_idx >= text.len() {
        return 1;
    }

    let (ch, _) = decode_utf8(&text[byte_idx..]);
    match ch {
        '\t' => {
            let col_usize = col.max(0) as usize;
            let next_tab = next_tab_stop_col(col_usize, params.tab_width, &params.tab_stop_list)
                .max(col_usize + 1);
            next_tab - col_usize
        }
        '\n' | '\r' => 1,
        _ if is_wide_char(ch) => 2,
        _ => 1,
    }
}

#[inline]
fn cursor_width_for_style(
    style: CursorStyle,
    text: &[u8],
    byte_idx: usize,
    col: i32,
    params: &WindowParams,
    face_char_w: f32,
) -> f32 {
    match style {
        CursorStyle::Bar(w) => w,
        _ => cursor_point_columns(text, byte_idx, col, params) as f32 * face_char_w,
    }
}

#[inline]
unsafe fn cursor_point_advance(
    text: &[u8],
    byte_idx: usize,
    col: i32,
    params: &WindowParams,
    face_char_w: f32,
    face_space_w: f32,
    char_w: f32,
    face_id: u32,
    font_size: i32,
    window: EmacsWindow,
    font_family: &str,
    font_weight: u16,
    font_italic: bool,
    ascii_width_cache: &mut std::collections::HashMap<(u32, i32), [f32; 128]>,
    font_metrics_svc: &mut Option<FontMetricsService>,
) -> Option<f32> {
    if byte_idx >= text.len() {
        return None;
    }

    let face_w = if face_char_w > 0.0 {
        face_char_w
    } else {
        char_w
    };
    let (ch, _) = decode_utf8(&text[byte_idx..]);
    match ch {
        '\n' | '\r' => Some(face_w),
        '\t' => {
            let col_usize = col.max(0) as usize;
            let next_tab = next_tab_stop_col(col_usize, params.tab_width, &params.tab_stop_list)
                .max(col_usize + 1);
            let tab_cols = next_tab.saturating_sub(col_usize).max(1);
            let space_w = if face_space_w > 0.0 {
                face_space_w
            } else {
                face_w
            };
            Some(tab_cols as f32 * space_w)
        }
        _ if ch < ' ' || ch == '\x7F' => Some(face_w),
        _ => {
            let char_cols = if is_wide_char(ch) { 2 } else { 1 };
            Some(char_advance(
                ascii_width_cache,
                font_metrics_svc,
                ch,
                char_cols,
                char_w,
                face_id,
                font_size,
                face_char_w,
                window,
                font_family,
                font_weight,
                font_italic,
            ))
        }
    }
}

#[inline]
fn cursor_style_for_window(params: &WindowParams) -> Option<CursorStyle> {
    if params.selected {
        return CursorStyle::from_type(params.cursor_type, params.cursor_bar_width);
    }

    // Keep Emacs behavior for non-selected minibuffer/echo-area paths where
    // C side resolves cursor_type to NO_CURSOR (4).
    if params.cursor_type == 4 {
        return None;
    }

    if params.cursor_in_non_selected {
        Some(CursorStyle::Hollow)
    } else {
        None
    }
}

/// Parse `:raise` factor from a display property value.
///
/// Handles two forms:
/// 1. `(raise FACTOR)` — a list whose car is the symbol `raise`
/// 2. A plist containing `:raise FACTOR` (e.g., `(space :raise 0.3 :width 5)`)
///
/// Returns the raise factor as f32, or None if not a raise spec.
fn parse_display_raise_factor(prop_val: &neovm_core::emacs_core::Value) -> Option<f32> {
    // Form 1: (raise FACTOR)
    if let neovm_core::emacs_core::Value::Cons(id) = prop_val {
        let pair = neovm_core::emacs_core::value::read_cons(*id);
        if pair.car.is_symbol_named("raise") {
            // cdr should be (FACTOR . nil) or FACTOR
            if let neovm_core::emacs_core::Value::Cons(cdr_id) = pair.cdr {
                let cdr_pair = neovm_core::emacs_core::value::read_cons(cdr_id);
                if let Some(f) = cdr_pair.car.as_number_f64() {
                    return Some(f as f32);
                }
            } else if let Some(f) = pair.cdr.as_number_f64() {
                return Some(f as f32);
            }
        }
    }

    // Form 2: plist with :raise key
    if let Some(items) = neovm_core::emacs_core::value::list_to_vec(prop_val) {
        let mut i = 0;
        while i + 1 < items.len() {
            if items[i].is_symbol_named(":raise") {
                if let Some(f) = items[i + 1].as_number_f64() {
                    return Some(f as f32);
                }
            }
            i += 1;
        }
    }
    None
}

/// Parse `:height` factor from a display property value.
///
/// Handles two forms:
/// 1. `(height FACTOR)` — a list whose car is the symbol `height`
/// 2. A plist containing `:height FACTOR` (e.g., `(space :height 1.5)`)
///
/// Returns the height scale factor as f32, or None if not a height spec.
fn parse_display_height_factor(prop_val: &neovm_core::emacs_core::Value) -> Option<f32> {
    // Form 1: (height FACTOR)
    if let neovm_core::emacs_core::Value::Cons(id) = prop_val {
        let pair = neovm_core::emacs_core::value::read_cons(*id);
        if pair.car.is_symbol_named("height") {
            // cdr should be (FACTOR . nil) or FACTOR
            if let neovm_core::emacs_core::Value::Cons(cdr_id) = pair.cdr {
                let cdr_pair = neovm_core::emacs_core::value::read_cons(cdr_id);
                if let Some(f) = cdr_pair.car.as_number_f64() {
                    return Some(f as f32);
                }
            } else if let Some(f) = pair.cdr.as_number_f64() {
                return Some(f as f32);
            }
        }
    }

    // Form 2: plist with :height key
    if let Some(items) = neovm_core::emacs_core::value::list_to_vec(prop_val) {
        let mut i = 0;
        while i + 1 < items.len() {
            if items[i].is_symbol_named(":height") {
                if let Some(f) = items[i + 1].as_number_f64() {
                    return Some(f as f32);
                }
            }
            i += 1;
        }
    }
    None
}

/// Check if a character should be displayed as a glyphless character.
/// Returns: 0=normal, 1=thin_space, 2=empty_box, 3=hex_code, 5=zero_width
fn check_glyphless_char(ch: char) -> u8 {
    let cp = ch as u32;
    // C1 control characters: U+0080 to U+009F — show as hex code
    if cp >= 0x80 && cp <= 0x9F {
        return 3;
    }
    // Byte-order marks and zero-width chars
    if cp == 0xFEFF {
        return 5;
    } // BOM / ZWNBSP
    if cp == 0x200B {
        return 5;
    } // zero-width space
    if cp == 0x200C || cp == 0x200D {
        return 5;
    } // ZWNJ, ZWJ
    if cp == 0x200E || cp == 0x200F {
        return 5;
    } // LRM, RLM
    if cp == 0x2028 {
        return 5;
    } // line separator (in buffer text)
    if cp == 0x2029 {
        return 5;
    } // paragraph separator
    // Unicode specials block: U+FFF0-U+FFF8 (not assigned)
    if cp >= 0xFFF0 && cp <= 0xFFF8 {
        return 3;
    }
    // Object replacement character
    if cp == 0xFFFC {
        return 2;
    } // empty box
    // Language tags block U+E0001-U+E007F: zero-width
    if cp >= 0xE0001 && cp <= 0xE007F {
        return 5;
    }
    // Variation selectors supplement: zero-width
    if cp >= 0xE0100 && cp <= 0xE01EF {
        return 5;
    }
    // Basic variation selectors: zero-width
    if cp >= 0xFE00 && cp <= 0xFE0F {
        return 5;
    }
    0 // normal display
}

/// Render overlay string bytes into the frame glyph buffer.
/// Returns the number of pixels advanced in x.
fn render_overlay_string(
    text_bytes: &[u8],
    x: &mut f32,
    y: f32,
    col: &mut usize,
    face_char_w: f32,
    char_h: f32,
    font_ascent: f32,
    max_x: f32,
    frame_glyphs: &mut FrameGlyphBuffer,
    overlay_face: Option<&super::neovm_bridge::ResolvedFace>,
    current_face_id: &mut u32,
) {
    // Apply overlay face colors if provided
    if let Some(face) = overlay_face {
        let fg = Color::from_pixel(face.fg);
        let bg = Some(Color::from_pixel(face.bg));
        let ul_color = if face.underline_color != 0 {
            Some(Color::from_pixel(face.underline_color))
        } else {
            None
        };
        let st_color = if face.strike_through_color != 0 {
            Some(Color::from_pixel(face.strike_through_color))
        } else {
            None
        };
        let ol_color = if face.overline_color != 0 {
            Some(Color::from_pixel(face.overline_color))
        } else {
            None
        };
        frame_glyphs.set_face_with_font(
            *current_face_id,
            fg,
            bg,
            &face.font_family,
            face.font_weight,
            face.italic,
            face.font_size,
            face.underline_style,
            ul_color,
            if face.strike_through { 1 } else { 0 },
            st_color,
            if face.overline { 1 } else { 0 },
            ol_color,
            face.overstrike,
        );
        *current_face_id += 1;
    }

    let mut idx = 0;
    while idx < text_bytes.len() {
        let (ch, ch_len) = decode_utf8(&text_bytes[idx..]);
        let ch_advance = if is_wide_char(ch) {
            2.0 * face_char_w
        } else {
            face_char_w
        };
        if *x + ch_advance > max_x {
            break;
        }
        idx += ch_len;
        if ch == '\n' {
            continue; // Skip newlines in overlay strings
        }
        frame_glyphs.add_char(ch, *x, y, ch_advance, char_h, font_ascent, false);
        *x += ch_advance;
        *col += if is_wide_char(ch) { 2 } else { 1 };
    }
}

/// The main Rust layout engine.
///
/// Called on the Emacs thread during redisplay. Reads buffer data via FFI,
/// resolves faces, computes layout, and produces a FrameGlyphBuffer.
pub struct LayoutEngine {
    /// Reusable text buffer to avoid allocation per frame
    text_buf: Vec<u8>,
    /// Cached face data to avoid redundant FFI calls
    face_data: FaceDataFFI,
    /// Per-face ASCII width cache: actual glyph widths via text_extents().
    /// Key: (face_id, font_size), Value: advance widths for chars 0-127.
    pub(crate) ascii_width_cache: std::collections::HashMap<(u32, i32), [f32; 128]>,
    /// Hit-test data being built for current frame
    hit_data: Vec<WindowHitData>,
    /// Reusable ligature run buffer
    run_buf: LigatureRunBuffer,
    /// Whether ligatures are enabled
    pub ligatures_enabled: bool,
    /// Resolved font family name for the current face.
    /// When a font_file_path is available and cosmic-text metrics are active,
    /// this holds the fontdb-registered family name. Otherwise it mirrors
    /// the Emacs font_family. Avoids per-character String allocation.
    current_resolved_family: String,
    /// Face ID for which current_resolved_family was computed.
    /// Used to avoid re-resolving on every character.
    resolved_family_face_id: u32,
    /// Cosmic-text font metrics service (lazily initialized on first use)
    font_metrics: Option<FontMetricsService>,
    /// Whether to use cosmic-text for font metrics instead of C FFI
    pub use_cosmic_metrics: bool,
    /// Previous frame's per-window metadata for transition hint derivation.
    prev_window_infos: std::collections::HashMap<i64, WindowInfo>,
    /// Previous selected window id for switch-fade detection.
    prev_selected_window_id: i64,
    /// Previous frame background for theme-transition detection.
    prev_background: Option<(f32, f32, f32, f32)>,
}

impl LayoutEngine {
    /// Create a new layout engine.
    pub fn new() -> Self {
        Self {
            text_buf: Vec::with_capacity(64 * 1024), // 64KB initial
            face_data: FaceDataFFI::default(),
            ascii_width_cache: std::collections::HashMap::new(),
            hit_data: Vec::new(),
            run_buf: LigatureRunBuffer::new(),
            ligatures_enabled: false,
            current_resolved_family: String::new(),
            resolved_family_face_id: u32::MAX,
            font_metrics: None,
            use_cosmic_metrics: true,
            prev_window_infos: std::collections::HashMap::new(),
            prev_selected_window_id: 0,
            prev_background: None,
        }
    }

    fn record_transition_hint_from_latest_window_info(
        &self,
        frame_glyphs: &mut FrameGlyphBuffer,
        curr_window_infos: &mut std::collections::HashMap<i64, WindowInfo>,
    ) {
        if let Some(curr) = frame_glyphs.window_infos.last().cloned() {
            if let Some(prev) = self.prev_window_infos.get(&curr.window_id) {
                if let Some(hint) = FrameGlyphBuffer::derive_transition_hint(prev, &curr) {
                    frame_glyphs.add_transition_hint(hint);
                }
            }
            curr_window_infos.insert(curr.window_id, curr);
        }
    }

    fn record_effect_hints_from_latest_window_info(&self, frame_glyphs: &mut FrameGlyphBuffer) {
        let Some(curr) = frame_glyphs.window_infos.last().cloned() else {
            return;
        };
        if curr.is_minibuffer {
            return;
        }

        let Some(prev) = self.prev_window_infos.get(&curr.window_id) else {
            return;
        };
        if prev.buffer_id == 0 || curr.buffer_id == 0 {
            return;
        }

        if prev.buffer_id != curr.buffer_id {
            frame_glyphs.add_effect_hint(WindowEffectHint::TextFadeIn {
                window_id: curr.window_id,
                bounds: curr.bounds,
            });
            return;
        }

        if prev.window_start != curr.window_start {
            let direction = if curr.window_start > prev.window_start {
                1
            } else {
                -1
            };
            let delta = (curr.window_start - prev.window_start).unsigned_abs() as f32;
            frame_glyphs.add_effect_hint(WindowEffectHint::TextFadeIn {
                window_id: curr.window_id,
                bounds: curr.bounds,
            });
            frame_glyphs.add_effect_hint(WindowEffectHint::ScrollLineSpacing {
                window_id: curr.window_id,
                bounds: curr.bounds,
                direction,
            });
            frame_glyphs.add_effect_hint(WindowEffectHint::ScrollMomentum {
                window_id: curr.window_id,
                bounds: curr.bounds,
                direction,
            });
            frame_glyphs.add_effect_hint(WindowEffectHint::ScrollVelocityFade {
                window_id: curr.window_id,
                bounds: curr.bounds,
                delta,
            });
        }
    }

    fn find_window_cursor_y(frame_glyphs: &FrameGlyphBuffer, info: &WindowInfo) -> Option<f32> {
        for glyph in &frame_glyphs.glyphs {
            if let neomacs_display_protocol::frame_glyphs::FrameGlyph::Cursor {
                x, y, style, ..
            } = glyph
            {
                if *x >= info.bounds.x
                    && *x < info.bounds.x + info.bounds.width
                    && *y >= info.bounds.y
                    && *y < info.bounds.y + info.bounds.height
                    && !style.is_hollow()
                {
                    return Some(*y);
                }
            }
        }
        None
    }

    fn add_line_animation_hints(
        &self,
        frame_glyphs: &mut FrameGlyphBuffer,
        curr_window_infos: &std::collections::HashMap<i64, WindowInfo>,
    ) {
        for (window_id, curr) in curr_window_infos {
            if curr.is_minibuffer {
                continue;
            }
            let Some(prev) = self.prev_window_infos.get(window_id) else {
                continue;
            };
            if prev.buffer_id == 0 || curr.buffer_id == 0 {
                continue;
            }
            if prev.buffer_id == curr.buffer_id
                && prev.window_start == curr.window_start
                && prev.buffer_size != curr.buffer_size
            {
                if let Some(edit_y) = Self::find_window_cursor_y(frame_glyphs, curr) {
                    let offset = if curr.buffer_size > prev.buffer_size {
                        -curr.char_height
                    } else {
                        curr.char_height
                    };
                    frame_glyphs.add_effect_hint(WindowEffectHint::LineAnimation {
                        window_id: curr.window_id,
                        bounds: curr.bounds,
                        edit_y: edit_y + curr.char_height,
                        offset,
                    });
                }
            }
        }
    }

    fn update_window_switch_hint(&mut self, frame_glyphs: &mut FrameGlyphBuffer) {
        let new_selected = frame_glyphs
            .window_infos
            .iter()
            .find(|info| info.selected && !info.is_minibuffer)
            .map(|info| (info.window_id, info.bounds));
        if let Some((window_id, bounds)) = new_selected {
            if self.prev_selected_window_id != 0 && self.prev_selected_window_id != window_id {
                frame_glyphs
                    .add_effect_hint(WindowEffectHint::WindowSwitchFade { window_id, bounds });
            }
            self.prev_selected_window_id = window_id;
        }
    }

    fn update_theme_transition_hint(&mut self, frame_glyphs: &mut FrameGlyphBuffer) {
        let bg = &frame_glyphs.background;
        let new_bg = (bg.r, bg.g, bg.b, bg.a);
        if let Some(old_bg) = self.prev_background {
            let dr = (new_bg.0 - old_bg.0).abs();
            let dg = (new_bg.1 - old_bg.1).abs();
            let db = (new_bg.2 - old_bg.2).abs();
            if dr > 0.02 || dg > 0.02 || db > 0.02 {
                let full_h = frame_glyphs
                    .window_infos
                    .iter()
                    .find(|w| w.is_minibuffer)
                    .map_or(frame_glyphs.height, |w| w.bounds.y);
                frame_glyphs.add_effect_hint(WindowEffectHint::ThemeTransition {
                    bounds: Rect::new(0.0, 0.0, frame_glyphs.width, full_h),
                });
            }
        }
        self.prev_background = Some(new_bg);
    }

    fn maybe_add_topology_transition_hint(
        &self,
        frame_glyphs: &mut FrameGlyphBuffer,
        curr_window_infos: &std::collections::HashMap<i64, WindowInfo>,
    ) {
        if self.prev_window_infos.is_empty() {
            return;
        }

        let prev_non_mini: std::collections::HashSet<i64> = self
            .prev_window_infos
            .iter()
            .filter(|(_, info)| !info.is_minibuffer)
            .map(|(window_id, _)| *window_id)
            .collect();
        let curr_non_mini: std::collections::HashSet<i64> = curr_window_infos
            .iter()
            .filter(|(_, info)| !info.is_minibuffer)
            .map(|(window_id, _)| *window_id)
            .collect();

        if prev_non_mini.is_empty() || curr_non_mini.is_empty() || prev_non_mini == curr_non_mini {
            return;
        }

        if frame_glyphs
            .transition_hints
            .iter()
            .any(|hint| hint.window_id == 0 && matches!(hint.kind, WindowTransitionKind::Crossfade))
        {
            return;
        }

        let full_h = frame_glyphs
            .window_infos
            .iter()
            .find(|w| w.is_minibuffer)
            .map_or(frame_glyphs.height, |w| w.bounds.y);

        frame_glyphs.add_transition_hint(WindowTransitionHint {
            window_id: 0,
            bounds: Rect::new(0.0, 0.0, frame_glyphs.width, full_h),
            kind: WindowTransitionKind::Crossfade,
            effect: None,
            easing: None,
        });
    }

    // char_advance is a standalone function (below) to avoid borrow conflicts
    // with self.text_buf

    /// Perform layout for an entire frame.
    ///
    /// This is the main entry point, called from FFI when
    /// `neomacs-use-rust-display` is enabled.
    ///
    /// # Safety
    /// Must be called on the Emacs thread. The frame pointer must be valid.
    pub unsafe fn layout_frame(
        &mut self,
        frame: EmacsFrame,
        frame_params: &FrameParams,
        frame_glyphs: &mut FrameGlyphBuffer,
    ) {
        // Build a complete fresh frame every redisplay cycle.
        frame_glyphs.clear_all();
        let mut curr_window_infos: std::collections::HashMap<i64, WindowInfo> =
            std::collections::HashMap::new();

        // Set up frame dimensions
        frame_glyphs.width = frame_params.width;
        frame_glyphs.height = frame_params.height;
        frame_glyphs.char_width = frame_params.char_width;
        frame_glyphs.char_height = frame_params.char_height;
        frame_glyphs.font_pixel_size = frame_params.font_pixel_size;
        frame_glyphs.background = Color::from_pixel(frame_params.background);

        // Clear hit-test data for new frame
        self.hit_data.clear();

        // Lazy-initialize FontMetricsService when cosmic metrics are enabled
        if self.use_cosmic_metrics && self.font_metrics.is_none() {
            self.font_metrics = Some(FontMetricsService::new());
        } else if !self.use_cosmic_metrics && self.font_metrics.is_some() {
            // Drop the service when switching back to C metrics
            self.font_metrics = None;
        }

        // Always populate face_id=0 (DEFAULT_FACE_ID) in the faces map.
        // Many code paths use face_id=0 as a fallback: initial set_face(),
        // divider stretches, overlay strings without explicit face, and
        // the legacy menu/tool bar extraction.  Without this, glyphs with
        // face_id=0 have no Face entry and fall back to generic monospace.
        {
            let mut default_face = FaceDataFFI::default();
            let rc = neomacs_layout_default_face(frame, &mut default_face);
            if rc >= 0 {
                self.apply_face(&default_face, frame, frame_glyphs);
            }
        }

        // Render frame-level tab-bar (tab-bar-mode) via the status-line pipeline.
        let tab_bar_height = neomacs_layout_tab_bar_height(frame);
        if tab_bar_height > 0.0 {
            self.render_frame_tab_bar(frame, frame_params, frame_glyphs, tab_bar_height);
        }

        // Get number of windows (direct Rust struct access, no FFI call)
        let window_count = super::emacs_types::frame_window_count(frame as *const std::ffi::c_void);
        tracing::debug!(
            "layout_frame: {}x{} char={}x{} windows={}",
            frame_params.width,
            frame_params.height,
            frame_params.char_width,
            frame_params.char_height,
            window_count
        );

        for i in 0..window_count {
            let mut wp = WindowParamsFFI::default();
            let ret = neomacs_layout_get_window_params(frame, i, &mut wp);
            tracing::debug!(
                "  window[{}]: id={} mini={} bounds=({},{},{},{}) bufsz={} start={} point={}",
                i,
                wp.window_id,
                wp.is_minibuffer,
                wp.x,
                wp.y,
                wp.width,
                wp.height,
                wp.buffer_zv,
                wp.window_start,
                wp.point
            );
            if ret != 0 {
                continue;
            }

            // Read buffer metadata directly from Emacs struct (Phase 2: bypass C wrappers)
            let (rust_begv, rust_zv) = if !wp.buffer_ptr.is_null() {
                super::emacs_types::buffer_bounds(wp.buffer_ptr)
            } else {
                (1, 1)
            };
            let rust_point = if !wp.buffer_ptr.is_null() {
                super::emacs_types::buffer_point(wp.buffer_ptr)
            } else {
                1
            };
            let rust_tab_width = if !wp.buffer_ptr.is_null() {
                super::emacs_types::buffer_tab_width(wp.buffer_ptr)
            } else {
                8
            };
            let rust_truncate = if !wp.buffer_ptr.is_null() {
                super::emacs_types::buffer_truncate_lines(wp.buffer_ptr)
            } else {
                false
            };
            let rust_word_wrap = if !wp.buffer_ptr.is_null() {
                super::emacs_types::buffer_word_wrap(wp.buffer_ptr)
            } else {
                false
            };

            // Convert FFI params to our types
            // Buffer metadata fields use direct Rust struct reads instead of C values
            let params = WindowParams {
                window_id: wp.window_id,
                buffer_id: wp.buffer_id,
                bounds: Rect::new(wp.x, wp.y, wp.width, wp.height),
                text_bounds: Rect::new(wp.text_x, wp.text_y, wp.text_width, wp.text_height),
                selected: wp.selected != 0,
                is_minibuffer: wp.is_minibuffer != 0,
                window_start: wp.window_start,
                window_end: wp.window_end,
                point: rust_point,
                buffer_size: rust_zv,
                buffer_begv: rust_begv,
                hscroll: wp.hscroll,
                vscroll: wp.vscroll,
                truncate_lines: rust_truncate,
                word_wrap: rust_word_wrap,
                tab_width: rust_tab_width,
                tab_stop_list: Vec::new(), // C FFI path doesn't use tab-stop-list
                default_fg: wp.default_fg,
                default_bg: wp.default_bg,
                char_width: wp.char_width,
                char_height: wp.char_height,
                font_pixel_size: wp.font_pixel_size,
                font_ascent: wp.font_ascent,
                mode_line_height: wp.mode_line_height,
                header_line_height: wp.header_line_height,
                tab_line_height: wp.tab_line_height,
                cursor_type: wp.cursor_type,
                cursor_bar_width: wp.cursor_bar_width,
                cursor_color: wp.default_fg,
                left_fringe_width: wp.left_fringe_width,
                right_fringe_width: wp.right_fringe_width,
                indicate_empty_lines: wp.indicate_empty_lines,
                show_trailing_whitespace: wp.show_trailing_whitespace != 0,
                trailing_ws_bg: wp.trailing_ws_bg,
                fill_column_indicator: wp.fill_column_indicator,
                fill_column_indicator_char: char::from_u32(wp.fill_column_indicator_char as u32)
                    .unwrap_or('|'),
                fill_column_indicator_fg: wp.fill_column_indicator_fg,
                extra_line_spacing: wp.extra_line_spacing,
                cursor_in_non_selected: wp.cursor_in_non_selected != 0,
                selective_display: wp.selective_display,
                escape_glyph_fg: wp.escape_glyph_fg,
                nobreak_char_display: wp.nobreak_char_display,
                nobreak_char_fg: wp.nobreak_char_fg,
                glyphless_char_fg: wp.glyphless_char_fg,
                wrap_prefix: if wp.wrap_prefix_len > 0 {
                    wp.wrap_prefix[..wp.wrap_prefix_len as usize].to_vec()
                } else {
                    Vec::new()
                },
                line_prefix: if wp.line_prefix_len > 0 {
                    wp.line_prefix[..wp.line_prefix_len as usize].to_vec()
                } else {
                    Vec::new()
                },
                left_margin_width: wp.left_margin_width,
                right_margin_width: wp.right_margin_width,
            };

            // Add window background
            frame_glyphs.add_background(
                params.bounds.x,
                params.bounds.y,
                params.bounds.width,
                params.bounds.height,
                Color::from_pixel(params.default_bg),
            );

            // Add window info for animation detection
            // Extract buffer file name from FFI
            let buffer_file_name = if wp.buffer_file_name.is_null() {
                String::new()
            } else {
                CStr::from_ptr(wp.buffer_file_name)
                    .to_string_lossy()
                    .into_owned()
            };
            frame_glyphs.add_window_info(
                params.window_id,
                params.buffer_id,
                params.window_start,
                0, // window_end filled after layout
                params.buffer_size,
                params.bounds.x,
                params.bounds.y,
                params.bounds.width,
                params.bounds.height,
                params.mode_line_height,
                params.header_line_height,
                params.tab_line_height,
                params.selected,
                params.is_minibuffer,
                params.char_height,
                buffer_file_name,
                wp.modified != 0,
            );
            self.record_transition_hint_from_latest_window_info(
                frame_glyphs,
                &mut curr_window_infos,
            );
            self.record_effect_hints_from_latest_window_info(frame_glyphs);

            // Layout this window's content
            self.layout_window(&params, &wp, frame, frame_glyphs);

            // Draw window dividers or simple vertical border
            let right_edge = params.bounds.x + params.bounds.width;
            let bottom_edge = params.bounds.y + params.bounds.height;
            let is_rightmost = right_edge >= frame_params.width - 1.0;
            let is_bottommost = bottom_edge >= frame_params.height - 1.0;

            if frame_params.right_divider_width > 0 && !is_rightmost {
                // Draw right divider with first/last pixel faces
                let dw = frame_params.right_divider_width as f32;
                let x0 = right_edge - dw;
                let y0 = params.bounds.y;
                let h = params.bounds.height
                    - if frame_params.bottom_divider_width > 0 && !is_bottommost {
                        frame_params.bottom_divider_width as f32
                    } else {
                        0.0
                    };
                let first_fg = Color::from_pixel(frame_params.divider_first_fg);
                let mid_fg = Color::from_pixel(frame_params.divider_fg);
                let last_fg = Color::from_pixel(frame_params.divider_last_fg);
                if dw >= 3.0 {
                    frame_glyphs.add_stretch(x0, y0, 1.0, h, first_fg, 0, false);
                    frame_glyphs.add_stretch(x0 + 1.0, y0, dw - 2.0, h, mid_fg, 0, false);
                    frame_glyphs.add_stretch(x0 + dw - 1.0, y0, 1.0, h, last_fg, 0, false);
                } else if dw >= 2.0 {
                    frame_glyphs.add_stretch(x0, y0, 1.0, h, first_fg, 0, false);
                    frame_glyphs.add_stretch(x0 + 1.0, y0, 1.0, h, last_fg, 0, false);
                } else {
                    frame_glyphs.add_stretch(x0, y0, 1.0, h, mid_fg, 0, false);
                }
            } else if !is_rightmost {
                // Fallback: simple 1px vertical border
                let border_color = Color::from_pixel(frame_params.vertical_border_fg);
                frame_glyphs.add_stretch(
                    right_edge,
                    params.bounds.y,
                    1.0,
                    params.bounds.height,
                    border_color,
                    0,
                    false,
                );
            }

            if frame_params.bottom_divider_width > 0 && !is_bottommost {
                // Draw bottom divider with first/last pixel faces
                let dw = frame_params.bottom_divider_width as f32;
                let x0 = params.bounds.x;
                let y0 = bottom_edge - dw;
                let w = params.bounds.width
                    - if frame_params.right_divider_width > 0 && !is_rightmost {
                        frame_params.right_divider_width as f32
                    } else {
                        0.0
                    };
                let first_fg = Color::from_pixel(frame_params.divider_first_fg);
                let mid_fg = Color::from_pixel(frame_params.divider_fg);
                let last_fg = Color::from_pixel(frame_params.divider_last_fg);
                if dw >= 3.0 {
                    frame_glyphs.add_stretch(x0, y0, w, 1.0, first_fg, 0, false);
                    frame_glyphs.add_stretch(x0, y0 + 1.0, w, dw - 2.0, mid_fg, 0, false);
                    frame_glyphs.add_stretch(x0, y0 + dw - 1.0, w, 1.0, last_fg, 0, false);
                } else if dw >= 2.0 {
                    frame_glyphs.add_stretch(x0, y0, w, 1.0, first_fg, 0, false);
                    frame_glyphs.add_stretch(x0, y0 + 1.0, w, 1.0, last_fg, 0, false);
                } else {
                    frame_glyphs.add_stretch(x0, y0, w, 1.0, mid_fg, 0, false);
                }
            }
        }

        self.add_line_animation_hints(frame_glyphs, &curr_window_infos);
        self.update_window_switch_hint(frame_glyphs);
        self.update_theme_transition_hint(frame_glyphs);
        self.maybe_add_topology_transition_hint(frame_glyphs, &curr_window_infos);
        self.prev_window_infos = curr_window_infos;

        // Publish hit-test data for mouse interaction queries
        unsafe {
            *std::ptr::addr_of_mut!(FRAME_HIT_DATA) = Some(std::mem::take(&mut self.hit_data));
        }
    }

    /// Perform layout for a frame using neovm-core data (Rust-authoritative path).
    ///
    /// This is the Rust-native alternative to `layout_frame()` which reads from
    /// C struct pointers. It reads buffer text, window geometry, and buffer-local
    /// variables directly from the Evaluator's state.
    pub fn layout_frame_rust(
        &mut self,
        evaluator: &mut neovm_core::emacs_core::Evaluator,
        frame_id: neovm_core::window::FrameId,
        frame_glyphs: &mut FrameGlyphBuffer,
    ) {
        // Collect window and frame params from neovm-core
        let (frame_params, window_params_list) =
            match super::neovm_bridge::collect_layout_params(evaluator, frame_id) {
                Some(data) => data,
                None => {
                    tracing::error!("layout_frame_rust: frame {:?} not found", frame_id);
                    return;
                }
            };

        // --- Fontification pass ---
        // Run fontification for each window's visible region BEFORE the
        // read-only layout pass.  This triggers jit-lock / font-lock to set
        // font-lock-face text properties that the FaceResolver later reads.
        evaluator.setup_thread_locals();
        for params in &window_params_list {
            let buf_id = neovm_core::buffer::BufferId(params.buffer_id);
            let window_start = params.window_start.max(params.buffer_begv);
            let text_height = params.bounds.height - params.mode_line_height;
            let max_rows = if params.char_height > 0.0 {
                (text_height / params.char_height).ceil() as i64
            } else {
                50 // fallback
            };
            // Estimate the end of the visible region (generous: 200 chars/line).
            let fontify_end = (window_start + max_rows * 200).min(params.buffer_size);
            Self::ensure_fontified_rust(evaluator, buf_id, window_start, fontify_end);
        }

        // Clear previous frame's glyphs before building new frame
        frame_glyphs.clear_all();
        let mut curr_window_infos: std::collections::HashMap<i64, WindowInfo> =
            std::collections::HashMap::new();

        // Set up frame dimensions
        frame_glyphs.width = frame_params.width;
        frame_glyphs.height = frame_params.height;
        frame_glyphs.char_width = frame_params.char_width;
        frame_glyphs.char_height = frame_params.char_height;
        frame_glyphs.font_pixel_size = frame_params.font_pixel_size;
        frame_glyphs.background = Color::from_pixel(frame_params.background);

        // Clear hit-test data for new frame
        self.hit_data.clear();

        // Lazy-initialize FontMetricsService
        if self.use_cosmic_metrics && self.font_metrics.is_none() {
            self.font_metrics = Some(FontMetricsService::new());
        } else if !self.use_cosmic_metrics && self.font_metrics.is_some() {
            self.font_metrics = None;
        }

        // Create FaceResolver from neovm-core face table
        let face_resolver = super::neovm_bridge::FaceResolver::new(
            evaluator.face_table(),
            0x00FFFFFF,              // fallback fg
            frame_params.background, // fallback bg
            frame_params.font_pixel_size,
        );
        let default_resolved = face_resolver.default_face();
        let default_fg = Color::from_pixel(default_resolved.fg);
        let default_bg = Color::from_pixel(default_resolved.bg);

        // Update frame char metrics from the actual default face font.
        // This ensures mode-line height, window splits, etc. use the correct
        // font dimensions instead of stale hardcoded defaults.
        if let Some(ref mut svc) = self.font_metrics {
            let m = svc.font_metrics(
                &default_resolved.font_family,
                default_resolved.font_weight,
                default_resolved.italic,
                default_resolved.font_size,
            );
            if let Some(frame) = evaluator.frame_manager_mut().get_mut(frame_id) {
                frame.char_width = m.char_width;
                frame.char_height = m.line_height;
                frame.font_pixel_size = default_resolved.font_size;
            }
            frame_glyphs.char_width = m.char_width;
            frame_glyphs.char_height = m.line_height;
            frame_glyphs.font_pixel_size = default_resolved.font_size;
        }

        // Set default face (face_id=0) from FaceResolver
        frame_glyphs.set_face_with_font(
            0, // DEFAULT_FACE_ID
            default_fg,
            Some(default_bg),
            &default_resolved.font_family,
            default_resolved.font_weight,
            default_resolved.italic,
            default_resolved.font_size,
            default_resolved.underline_style,
            if default_resolved.underline_color != 0 {
                Some(Color::from_pixel(default_resolved.underline_color))
            } else {
                None
            },
            if default_resolved.strike_through {
                1
            } else {
                0
            },
            if default_resolved.strike_through_color != 0 {
                Some(Color::from_pixel(default_resolved.strike_through_color))
            } else {
                None
            },
            if default_resolved.overline { 1 } else { 0 },
            if default_resolved.overline_color != 0 {
                Some(Color::from_pixel(default_resolved.overline_color))
            } else {
                None
            },
            default_resolved.overstrike,
        );

        // Query actual font metrics for the default face from FontMetricsService.
        // This ensures frame_glyphs.char_width reflects the cosmic-text measurement
        // rather than the C-side font metrics, eliminating width mismatches.
        if let Some(ref mut svc) = self.font_metrics {
            let default_metrics = svc.font_metrics(
                &default_resolved.font_family,
                default_resolved.font_weight,
                default_resolved.italic,
                default_resolved.font_size,
            );
            frame_glyphs.char_width = default_metrics.char_width;
            // Keep frame_glyphs.char_height from frame params (vertical metrics
            // are more stable and less likely to mismatch).
        }

        tracing::debug!(
            "layout_frame_rust: {}x{} char={}x{} windows={}",
            frame_params.width,
            frame_params.height,
            frame_params.char_width,
            frame_params.char_height,
            window_params_list.len()
        );

        for params in &window_params_list {
            tracing::debug!(
                "layout window: id={} buf={} bounds=({:.0},{:.0},{:.0},{:.0}) mini={} selected={} mode_line_h={:.0}",
                params.window_id,
                params.buffer_id,
                params.bounds.x,
                params.bounds.y,
                params.bounds.width,
                params.bounds.height,
                params.is_minibuffer,
                params.selected,
                params.mode_line_height,
            );
            // Add window background
            frame_glyphs.add_background(
                params.bounds.x,
                params.bounds.y,
                params.bounds.width,
                params.bounds.height,
                Color::from_pixel(params.default_bg),
            );

            // Add window info for animation detection
            let buffer_file_name = {
                let buf_id = neovm_core::buffer::BufferId(params.buffer_id);
                evaluator
                    .buffer_manager()
                    .get(buf_id)
                    .and_then(|b| b.file_name.as_ref())
                    .cloned()
                    .unwrap_or_default()
            };
            let modified = {
                let buf_id = neovm_core::buffer::BufferId(params.buffer_id);
                evaluator
                    .buffer_manager()
                    .get(buf_id)
                    .map(|b| b.modified)
                    .unwrap_or(false)
            };
            frame_glyphs.add_window_info(
                params.window_id,
                params.buffer_id,
                params.window_start,
                0, // window_end filled after layout
                params.buffer_size,
                params.bounds.x,
                params.bounds.y,
                params.bounds.width,
                params.bounds.height,
                params.mode_line_height,
                params.header_line_height,
                params.tab_line_height,
                params.selected,
                params.is_minibuffer,
                params.char_height,
                buffer_file_name,
                modified,
            );
            self.record_transition_hint_from_latest_window_info(
                frame_glyphs,
                &mut curr_window_infos,
            );
            self.record_effect_hints_from_latest_window_info(frame_glyphs);

            // Simplified layout for this window (no face resolution, no overlays)
            self.layout_window_rust(
                evaluator,
                frame_id,
                params,
                &frame_params,
                frame_glyphs,
                &face_resolver,
            );

            // Draw window dividers
            let right_edge = params.bounds.x + params.bounds.width;
            let bottom_edge = params.bounds.y + params.bounds.height;
            let is_rightmost = right_edge >= frame_params.width - 1.0;
            let is_bottommost = bottom_edge >= frame_params.height - 1.0;

            if frame_params.right_divider_width > 0 && !is_rightmost {
                let dw = frame_params.right_divider_width as f32;
                let x0 = right_edge - dw;
                let y0 = params.bounds.y;
                let h = params.bounds.height
                    - if frame_params.bottom_divider_width > 0 && !is_bottommost {
                        frame_params.bottom_divider_width as f32
                    } else {
                        0.0
                    };
                let mid_fg = Color::from_pixel(frame_params.divider_fg);
                frame_glyphs.add_stretch(x0, y0, dw, h, mid_fg, 0, false);
            } else if !is_rightmost {
                let border_color = Color::from_pixel(frame_params.vertical_border_fg);
                frame_glyphs.add_stretch(
                    right_edge,
                    params.bounds.y,
                    1.0,
                    params.bounds.height,
                    border_color,
                    0,
                    false,
                );
            }

            if frame_params.bottom_divider_width > 0 && !is_bottommost {
                let dw = frame_params.bottom_divider_width as f32;
                let x0 = params.bounds.x;
                let y0 = bottom_edge - dw;
                let w = params.bounds.width;
                let mid_fg = Color::from_pixel(frame_params.divider_fg);
                frame_glyphs.add_stretch(x0, y0, w, dw, mid_fg, 0, false);
            }
        }

        self.add_line_animation_hints(frame_glyphs, &curr_window_infos);
        self.update_window_switch_hint(frame_glyphs);
        self.update_theme_transition_hint(frame_glyphs);
        self.maybe_add_topology_transition_hint(frame_glyphs, &curr_window_infos);
        self.prev_window_infos = curr_window_infos;
    }

    /// Simplified window layout using neovm-core data.
    ///
    /// Renders buffer text as a monospace grid with face resolution.
    /// Queries FontMetricsService for per-face character metrics when available.
    /// Note: fontification (jit-lock / font-lock) is triggered by
    /// `layout_frame_rust()` before this function is called, so text
    /// properties are already up-to-date when we read them here.
    fn layout_window_rust(
        &mut self,
        evaluator: &mut neovm_core::emacs_core::Evaluator,
        frame_id: neovm_core::window::FrameId,
        params: &WindowParams,
        _frame_params: &FrameParams,
        frame_glyphs: &mut FrameGlyphBuffer,
        face_resolver: &super::neovm_bridge::FaceResolver,
    ) {
        let buf_id = neovm_core::buffer::BufferId(params.buffer_id);
        let buffer = match evaluator.buffer_manager().get(buf_id) {
            Some(b) => b,
            None => {
                tracing::debug!("layout_window_rust: buffer {} not found", params.buffer_id);
                return;
            }
        };

        // Capture buffer name as owned String for use in mode-line fallback.
        // This avoids holding a borrow on `evaluator` through eval calls.
        let buffer_name = buffer.name.clone();

        let buf_access = super::neovm_bridge::RustBufferAccess::new(buffer);

        let char_w = params.char_width;
        let char_h = params.char_height;
        let font_ascent = params.font_ascent;
        let echo_message = if params.is_minibuffer {
            evaluator
                .current_message_text()
                .filter(|message| !message.is_empty())
                .map(|message| message.to_string())
        } else {
            None
        };

        // Text area (excluding fringes, margins, mode-line)
        let text_x = params.text_bounds.x;
        let text_y = params.text_bounds.y + params.header_line_height + params.tab_line_height;
        let text_width = params.text_bounds.width;
        let text_height = params.bounds.height
            - params.mode_line_height
            - params.header_line_height
            - params.tab_line_height;

        // Authoritative draw context for this window's content rows.
        frame_glyphs.set_draw_context(
            params.window_id,
            if params.is_minibuffer {
                GlyphRowRole::Minibuffer
            } else {
                GlyphRowRole::Text
            },
            Some(Rect::new(text_x, text_y, text_width, text_height.max(0.0))),
        );

        // Apply vertical scroll: shift content up by vscroll pixels.
        // In Emacs, w->vscroll is a Y offset, always <= 0 (negative = up):
        //   set-window-vscroll(100) -> w->vscroll = -100
        // Negate to get the positive pixel shift, then reduce text_height.
        // When shift >= text_height the window renders empty
        // (used by vertico-posframe to hide the minibuffer).
        let vscroll = (-params.vscroll).max(0) as f32;
        let text_height = (text_height - vscroll).max(0.0);

        if text_height <= 0.0 || text_width <= 0.0 {
            return;
        }

        // Line number configuration from buffer-local variables
        let lnum_mode = match super::neovm_bridge::buffer_display_line_numbers_mode(buffer) {
            super::neovm_bridge::DisplayLineNumbersMode::Off => 0,
            super::neovm_bridge::DisplayLineNumbersMode::Absolute => 1,
            super::neovm_bridge::DisplayLineNumbersMode::Relative => 2,
            super::neovm_bridge::DisplayLineNumbersMode::Visual => 3,
        };
        let lnum_enabled = lnum_mode > 0;
        let lnum_offset =
            super::neovm_bridge::buffer_local_int(buffer, "display-line-numbers-offset", 0);
        let lnum_major_tick =
            super::neovm_bridge::buffer_local_int(buffer, "display-line-numbers-major-tick", 0)
                as i32;
        let _lnum_minor_tick =
            super::neovm_bridge::buffer_local_int(buffer, "display-line-numbers-minor-tick", 0)
                as i32;
        let lnum_current_absolute =
            super::neovm_bridge::buffer_local_bool(buffer, "display-line-numbers-current-absolute");
        let lnum_widen =
            super::neovm_bridge::buffer_local_bool(buffer, "display-line-numbers-widen");
        let lnum_min_width =
            super::neovm_bridge::buffer_local_int(buffer, "display-line-numbers-width", 0) as i32;

        // Selective display: integer N = hide lines with > N indent + CR hides rest of line;
        // t (True) = only CR hides rest of line (mapped to i32::MAX so indent check never triggers)
        let selective_display = super::neovm_bridge::buffer_selective_display(buffer);

        // Line/wrap prefix: read from buffer-local variables
        let line_prefix_str = super::neovm_bridge::buffer_local_string_owned(buffer, "line-prefix");
        let wrap_prefix_str = super::neovm_bridge::buffer_local_string_owned(buffer, "wrap-prefix");
        let has_prefix = line_prefix_str.is_some() || wrap_prefix_str.is_some();

        // Compute line number column width
        let lnum_cols = if lnum_enabled {
            let total_lines = buf_access.count_lines(0, buf_access.zv()) + 1;
            let digit_count = format!("{}", total_lines).len() as i32;
            let min = lnum_min_width.max(1);
            digit_count.max(min) + 1 // +1 for trailing space separator
        } else {
            0
        };
        let lnum_pixel_width = lnum_cols as f32 * char_w;

        let max_rows = (text_height / char_h).floor() as usize;
        // The minibuffer must always render at least 1 row.  Its pixel
        // height may be fractionally smaller than char_h (e.g. 24px vs
        // 24.15 with line-spacing) causing floor() to yield 0.
        // Exception: when vscroll is active, don't force 1 row -- vscroll
        // is used (e.g. by vertico-posframe) to intentionally hide content.
        let max_rows =
            if params.is_minibuffer && max_rows == 0 && text_height > 0.0 && vscroll == 0.0 {
                1
            } else {
                max_rows
            };
        let cols = ((text_width - lnum_pixel_width) / char_w).floor() as usize;
        let content_x = text_x + lnum_pixel_width;

        // Read buffer text starting from window_start.
        // Auto-adjust window_start when point is above the visible region.
        let window_start = {
            let mut ws = params.window_start.max(params.buffer_begv);
            if params.point >= params.buffer_begv && params.point < ws {
                // Point is above the visible region: scroll backward.
                // Target: show point about 25% of the way down from the top.
                let target_rows_above = (max_rows / 4).max(1) as i64;
                let mut lines_back: i64 = 0;
                let mut scan_pos = params.point;
                // Scan backward through buffer text counting newlines
                while scan_pos > params.buffer_begv && lines_back < target_rows_above {
                    scan_pos -= 1;
                    let bp = buf_access.charpos_to_bytepos(scan_pos);
                    if buf_access.byte_at(bp) == Some(b'\n') {
                        lines_back += 1;
                    }
                }
                ws = scan_pos.max(params.buffer_begv);
                tracing::debug!(
                    "layout_window_rust: adjusted window_start {} -> {} (point={})",
                    params.window_start,
                    ws,
                    params.point
                );
            }
            ws
        };
        let read_chars =
            (params.buffer_size - window_start + 1).min(cols as i64 * max_rows as i64 * 2);

        let bytes_read = if read_chars <= 0 {
            0i64
        } else {
            let text_end = (window_start + read_chars).min(params.buffer_size);
            let byte_from = buf_access.charpos_to_bytepos(window_start);
            let byte_to = buf_access.charpos_to_bytepos(text_end);
            buf_access.copy_text(byte_from, byte_to, &mut self.text_buf);
            self.text_buf.len() as i64
        };

        let text = if bytes_read > 0 {
            &self.text_buf[..bytes_read as usize]
        } else {
            &[]
        };

        tracing::debug!(
            "  layout_window_rust id={}: text_y={:.1} text_h={:.1} max_rows={} bytes_read={}",
            params.window_id,
            text_y,
            text_height,
            max_rows,
            bytes_read
        );

        // Use face_resolver's default face for this window
        let default_resolved = face_resolver.default_face();
        let default_fg = Color::from_pixel(default_resolved.fg);
        let default_bg = Color::from_pixel(default_resolved.bg);

        // Query default face metrics from FontMetricsService if available.
        // These serve as the baseline and fallback when no per-face metrics are available.
        let (default_face_char_w, default_face_h, default_face_ascent) =
            if let Some(ref mut svc) = self.font_metrics {
                let m = svc.font_metrics(
                    &default_resolved.font_family,
                    default_resolved.font_weight,
                    default_resolved.italic,
                    default_resolved.font_size,
                );
                (m.char_width, m.line_height, m.ascent)
            } else {
                (char_w, char_h, font_ascent)
            };

        tracing::debug!(
            "layout font metrics: family={:?} weight={} italic={} size={} char_w={:.2} char_h={:.2} ascent={:.2} (window char_w={:.2} char_h={:.2})",
            default_resolved.font_family,
            default_resolved.font_weight,
            default_resolved.italic,
            default_resolved.font_size,
            default_face_char_w,
            default_face_h,
            default_face_ascent,
            char_w,
            char_h,
        );

        // Per-face metrics — start with defaults, updated on face change
        let mut face_char_w = default_face_char_w;
        let mut face_h = default_face_h;
        let mut face_ascent_val = default_face_ascent;

        // Face resolution state
        let mut face_next_check: usize = 0;
        let mut current_face_id: u32 = 1; // 0 is reserved for default face
        let mut current_fg: Color = default_fg; // tracks foreground across face changes
        let mut current_bg: Color = default_bg; // tracks background across face changes

        if let Some(echo_message) = echo_message {
            self.render_rust_status_line_plain(
                text_x,
                text_y,
                text_width,
                text_height.max(char_h),
                params.window_id,
                char_w,
                default_face_ascent,
                0,
                default_resolved,
                echo_message,
                frame_glyphs,
                StatusLineKind::Minibuffer,
            );
            return;
        }

        // Line number state
        let window_start_byte = buf_access.charpos_to_bytepos(window_start);
        let begin_byte = if lnum_widen { 0 } else { buf_access.begv() };
        let mut current_line: i64 = if lnum_enabled {
            buf_access.count_lines(begin_byte, window_start_byte) + 1
        } else {
            1
        };
        let point_line: i64 = if lnum_enabled && lnum_mode >= 2 {
            let pt_byte = buf_access.charpos_to_bytepos(params.point);
            buf_access.count_lines(begin_byte, pt_byte) + 1
        } else {
            0
        };
        let mut need_line_number = lnum_enabled;

        // Simple monospace text layout
        let mut x = content_x;
        let mut y = text_y;
        let mut row = 0usize;
        let mut col = 0usize;
        let mut byte_idx = 0usize;
        let mut charpos = window_start;
        let mut invis_next_check: i64 = window_start; // Next position where visibility might change
        let mut display_next_check: i64 = window_start; // Next position where display props might change

        // Display :raise property: vertical Y offset for glyphs
        let mut raise_y_offset: f32 = 0.0;
        let mut raise_end: i64 = window_start;

        // Display :height property: font scale factor
        let mut height_scale: f32 = 0.0; // 0.0 = no scaling
        let mut height_end: i64 = window_start;

        // Fringe state tracking
        let left_fringe_x = params.text_bounds.x - params.left_fringe_width;
        let right_fringe_x = params.text_bounds.x + params.text_bounds.width;
        let mut row_continued = vec![false; max_rows];
        let mut row_truncated = vec![false; max_rows];
        let mut row_continuation = vec![false; max_rows];

        // Horizontal scroll: skip first hscroll columns on each line
        let hscroll = if params.truncate_lines {
            params.hscroll.max(0) as i32
        } else {
            0
        };
        let show_left_trunc = hscroll > 0;
        let mut hscroll_remaining = hscroll;

        // Word-wrap break tracking
        let mut wrap_break_byte_idx = 0usize;
        let mut wrap_break_charpos = window_start;
        let mut _wrap_break_x: f32 = 0.0;
        let mut _wrap_break_col = 0usize;
        let mut wrap_break_glyph_count = 0usize;
        let mut wrap_has_break = false;

        // Line/wrap prefix tracking: 0=none, 1=line-prefix, 2=wrap-prefix
        let mut need_prefix: u8 = if has_prefix && line_prefix_str.is_some() {
            1
        } else {
            0
        };

        let avail_width = text_width - lnum_pixel_width;

        // Variable-height row tracking
        let mut row_max_height: f32 = char_h; // max glyph height on current row
        let mut row_max_ascent: f32 = default_face_ascent; // max ascent on current row
        let mut row_extra_y: f32 = 0.0; // cumulative extra height from previous rows
        let mut row_y_positions: Vec<f32> = Vec::with_capacity(max_rows);
        row_y_positions.push(text_y); // row 0
        // Bidi reordering: track glyph range for each row
        let mut row_glyph_start: usize = frame_glyphs.glyphs.len();
        // Trailing whitespace tracking
        let trailing_ws_bg = if params.show_trailing_whitespace {
            Some(Color::from_pixel(params.trailing_ws_bg))
        } else {
            None
        };
        let mut trailing_ws_start_col: i32 = -1; // -1 = no trailing ws
        let mut trailing_ws_start_x: f32 = 0.0;
        let mut trailing_ws_row: usize = 0;

        // Check if the buffer has any overlays (optimization: skip per-char overlay checks if empty)
        let has_overlays = !buffer.overlays.is_empty();

        // Face :extend tracking — extends face background to end of line
        let mut row_extend_bg: Option<(Color, u32)> = None; // (bg_color, face_id)
        let mut row_extend_row: i32 = -1;

        // Box face tracking: track active :box face regions
        let mut box_active = false;
        let mut box_start_x: f32 = 0.0;
        let mut box_row: usize = 0;

        // Cursor metrics captured during the main layout loop.
        // (cx, cy, face_w, face_h, face_ascent, fg_color, byte_idx, col)
        let mut cursor_info: Option<(f32, f32, f32, f32, f32, Color, Color, usize, usize)> = None;

        // Hit-test data for this window
        let mut hit_rows: Vec<HitRow> = Vec::new();
        let mut hit_row_charpos_start: i64 = window_start;

        let ligatures = self.ligatures_enabled;
        self.run_buf.clear();

        // Margin state tracking
        let has_margins = params.left_margin_width > 0.0 || params.right_margin_width > 0.0;

        // Clear margin backgrounds with default face background so they don't
        // show visual artifacts.  Default Emacs layout (fringes-outside-margins
        // nil): | LEFT_MARGIN | LEFT_FRINGE | TEXT_AREA | RIGHT_FRINGE | RIGHT_MARGIN |
        // So left margin is outermost (before fringe), right margin is outermost
        // (after fringe).
        if has_margins {
            if params.left_margin_width > 0.0 {
                let margin_x = text_x - params.left_fringe_width - params.left_margin_width;
                frame_glyphs.add_stretch(
                    margin_x,
                    text_y,
                    params.left_margin_width,
                    text_height,
                    default_bg,
                    0,
                    false,
                );
            }
            if params.right_margin_width > 0.0 {
                let margin_x = text_x + text_width + params.right_fringe_width;
                frame_glyphs.add_stretch(
                    margin_x,
                    text_y,
                    params.right_margin_width,
                    text_height,
                    default_bg,
                    0,
                    false,
                );
            }
        }

        while byte_idx < text.len() && row < max_rows && y + row_max_height <= text_y + text_height
        {
            // Render line number at start of each visual line
            if need_line_number && lnum_enabled {
                let display_num = match lnum_mode {
                    2 | 3 => {
                        // Relative/visual mode
                        if lnum_current_absolute && current_line == point_line {
                            (current_line + lnum_offset).abs()
                        } else {
                            (current_line - point_line).abs()
                        }
                    }
                    _ => {
                        // Absolute mode
                        (current_line + lnum_offset).abs()
                    }
                };

                // Resolve line number face
                let is_current = current_line == point_line;
                let lnum_face = if is_current {
                    face_resolver.resolve_named_face("line-number-current-line")
                } else if lnum_major_tick > 0 && current_line % lnum_major_tick as i64 == 0 {
                    face_resolver.resolve_named_face("line-number-major-tick")
                } else {
                    face_resolver.resolve_named_face("line-number")
                };
                let lnum_bg = Color::from_pixel(lnum_face.bg);
                let lnum_fg = Color::from_pixel(lnum_face.fg);

                // Set line number face
                frame_glyphs.set_face_with_font(
                    current_face_id,
                    lnum_fg,
                    Some(lnum_bg),
                    &lnum_face.font_family,
                    lnum_face.font_weight,
                    lnum_face.italic,
                    lnum_face.font_size,
                    0,
                    None,
                    0,
                    None,
                    0,
                    None,
                    false,
                );
                let lnum_face_id = current_face_id;
                current_face_id += 1;

                // Format number right-aligned
                let num_str = format!("{}", display_num);
                let num_chars = num_str.len() as i32;
                let padding = (lnum_cols - 1) - num_chars; // -1 for trailing space

                let gy = y;

                // Leading padding (stretch)
                if padding > 0 {
                    frame_glyphs.add_stretch(
                        text_x,
                        gy,
                        padding as f32 * char_w,
                        char_h,
                        lnum_bg,
                        lnum_face_id,
                        false,
                    );
                }

                // Number digits
                for (i, ch) in num_str.chars().enumerate() {
                    let dx = text_x + (padding.max(0) + i as i32) as f32 * char_w;
                    frame_glyphs.add_char(ch, dx, gy, char_w, char_h, font_ascent, false);
                }

                // Trailing space separator
                let space_x = text_x + (lnum_cols - 1) as f32 * char_w;
                frame_glyphs.add_stretch(space_x, gy, char_w, char_h, lnum_bg, lnum_face_id, false);

                // Force face resolution to re-apply text face after line number face
                face_next_check = 0;

                need_line_number = false;
            }

            // --- Line/wrap prefix rendering ---
            if need_prefix > 0 {
                // Check text property prefix first (overrides buffer-local)
                let text_props = super::neovm_bridge::RustTextPropAccess::new(buffer);
                let prefix = if need_prefix == 2 {
                    text_props
                        .get_text_prop_string(charpos, "wrap-prefix")
                        .or_else(|| wrap_prefix_str.as_deref().map(|s| s.to_string()))
                } else {
                    text_props
                        .get_text_prop_string(charpos, "line-prefix")
                        .or_else(|| line_prefix_str.as_deref().map(|s| s.to_string()))
                };

                if let Some(prefix_text) = prefix {
                    // Flush ligature run before prefix
                    flush_run(&self.run_buf, frame_glyphs, ligatures);
                    self.run_buf.clear();

                    let right_limit = content_x + avail_width;
                    for pch in prefix_text.chars() {
                        if pch == '\n' || pch == '\r' {
                            continue;
                        }
                        let p_cols = if is_wide_char(pch) { 2 } else { 1 };
                        let p_adv = p_cols as f32 * face_char_w;
                        if x + p_adv > right_limit {
                            break;
                        }
                        frame_glyphs.add_char(pch, x, y, p_adv, char_h, face_ascent_val, false);
                        x += p_adv;
                        col += p_cols as usize;
                    }
                }
                need_prefix = 0;
            }

            // --- Invisible text check ---
            // Only call check_invisible at property change boundaries for efficiency
            if charpos >= invis_next_check {
                let text_props = super::neovm_bridge::RustTextPropAccess::new(buffer);
                let (is_invisible, next_visible) = text_props.check_invisible(charpos);
                if is_invisible {
                    // Check if ellipsis should be shown BEFORE skipping.
                    // In Emacs, invisible property `t` means hide completely (no ellipsis),
                    // while symbol values (e.g. `outline`, `hs`) typically indicate that
                    // ellipsis should be shown (via buffer-invisibility-spec).
                    let show_ellipsis = match text_props.get_property(charpos, "invisible") {
                        Some(neovm_core::emacs_core::Value::True) => false,
                        Some(neovm_core::emacs_core::Value::Nil) | None => false,
                        Some(_) => true,
                    };

                    // Skip to next_visible position
                    let skip_to = next_visible.min(params.buffer_size);
                    while charpos < skip_to && byte_idx < text.len() {
                        let (_ch, ch_len) = decode_utf8(&text[byte_idx..]);
                        byte_idx += ch_len;
                        charpos += 1;
                    }
                    invis_next_check = next_visible;

                    // Render "..." ellipsis for non-t invisible property values
                    if show_ellipsis {
                        flush_run(&self.run_buf, frame_glyphs, ligatures);
                        self.run_buf.clear();
                        let right_limit = content_x + avail_width;
                        for _ in 0..3 {
                            if x + face_char_w > right_limit {
                                break;
                            }
                            frame_glyphs.add_char(
                                '.',
                                x,
                                y,
                                face_char_w,
                                char_h,
                                face_ascent_val,
                                false,
                            );
                            x += face_char_w;
                            col += 1;
                        }
                    }

                    // Check for overlay strings at invisible region boundary.
                    // Packages like org-mode use overlay after-strings at invisible
                    // boundaries to show fold indicators (e.g. "[N lines]").
                    if has_overlays {
                        let invis_text_props = super::neovm_bridge::RustTextPropAccess::new(buffer);
                        let (_before_strings, after_strings) =
                            invis_text_props.overlay_strings_at(charpos);
                        if !after_strings.is_empty() {
                            flush_run(&self.run_buf, frame_glyphs, ligatures);
                            self.run_buf.clear();
                            let right_limit = content_x + avail_width;
                            for (string_bytes, overlay_id) in &after_strings {
                                let ov_face = buffer
                                    .overlays
                                    .overlay_get(*overlay_id, "face")
                                    .and_then(|val| face_resolver.resolve_face_from_value(val));
                                render_overlay_string(
                                    string_bytes,
                                    &mut x,
                                    y + raise_y_offset,
                                    &mut col,
                                    face_char_w,
                                    char_h,
                                    face_ascent_val,
                                    right_limit,
                                    frame_glyphs,
                                    ov_face.as_ref(),
                                    &mut current_face_id,
                                );
                            }
                        }
                    }

                    flush_run(&self.run_buf, frame_glyphs, ligatures);
                    self.run_buf.clear();
                    continue;
                }
                invis_next_check = next_visible;
            }

            // Handle hscroll: skip columns consumed by horizontal scroll
            if hscroll_remaining > 0 {
                flush_run(&self.run_buf, frame_glyphs, ligatures);
                self.run_buf.clear();
                let (ch, ch_len) = decode_utf8(&text[byte_idx..]);
                byte_idx += ch_len;
                charpos += 1;

                if ch == '\n' {
                    // Newline within hscroll region: advance to next row
                    if row_max_height > char_h {
                        row_extra_y += row_max_height - char_h;
                    }
                    x = content_x;
                    // Record hit-test row (hscroll newline)
                    hit_rows.push(HitRow {
                        y_start: y,
                        y_end: y + row_max_height,
                        charpos_start: hit_row_charpos_start,
                        charpos_end: charpos,
                    });
                    hit_row_charpos_start = charpos;
                    row_extend_bg = None;
                    row_extend_row = -1;

                    reorder_row_bidi(
                        frame_glyphs,
                        row_glyph_start,
                        frame_glyphs.glyphs.len(),
                        content_x,
                    );
                    row_glyph_start = frame_glyphs.glyphs.len();
                    row += 1;
                    y = text_y + row as f32 * char_h + row_extra_y;
                    row_max_height = char_h;
                    row_max_ascent = default_face_ascent;
                    row_y_positions.push(y);
                    col = 0;
                    current_line += 1;
                    need_line_number = lnum_enabled;
                    hscroll_remaining = hscroll; // reset for next line
                    trailing_ws_start_col = -1;
                    if has_prefix {
                        need_prefix = 1;
                    }
                } else {
                    let ch_cols: i32 = if ch == '\t' {
                        let tab_w = params.tab_width.max(1) as i32;
                        let consumed = hscroll - hscroll_remaining;
                        ((consumed / tab_w + 1) * tab_w) - consumed
                    } else if is_wide_char(ch) {
                        2
                    } else {
                        1
                    };
                    hscroll_remaining -= ch_cols.min(hscroll_remaining);

                    // When hscroll is exhausted, show $ indicator at left edge
                    if hscroll_remaining <= 0 && show_left_trunc {
                        frame_glyphs.add_char(
                            '$',
                            content_x,
                            y,
                            char_w,
                            char_h,
                            font_ascent,
                            false,
                        );
                        col = 1; // $ takes 1 column
                        x = content_x + char_w;
                    }
                }
                continue;
            }

            // --- Display property check ---
            // Only call check_display_prop at property change boundaries for efficiency
            if charpos >= display_next_check {
                let display_prop_val: Option<neovm_core::emacs_core::Value> = {
                    let text_props = super::neovm_bridge::RustTextPropAccess::new(buffer);
                    let (dp, next_change) = text_props.check_display_prop(charpos);
                    display_next_check = next_change;
                    dp.copied() // Value is Copy, so extract from reference
                };

                if let Some(prop_val) = display_prop_val {
                    flush_run(&self.run_buf, frame_glyphs, ligatures);
                    self.run_buf.clear();
                    // Case 1: String replacement — render the string instead of buffer text
                    if let Some(replacement) = prop_val.as_str() {
                        if !replacement.is_empty() {
                            let right_limit = content_x + (text_width - lnum_pixel_width);
                            for rch in replacement.chars() {
                                let rch_advance = if is_wide_char(rch) {
                                    2.0 * face_char_w
                                } else {
                                    face_char_w
                                };
                                if x + rch_advance > right_limit {
                                    break;
                                }
                                frame_glyphs.add_char(
                                    rch,
                                    x,
                                    y,
                                    rch_advance,
                                    char_h,
                                    face_ascent_val,
                                    false,
                                );
                                x += rch_advance;
                                col += if is_wide_char(rch) { 2 } else { 1 };
                            }
                        }

                        // Skip the buffer text that this display property covers
                        let skip_to = display_next_check.min(params.buffer_size);
                        while charpos < skip_to && byte_idx < text.len() {
                            let (_ch, ch_len) = decode_utf8(&text[byte_idx..]);
                            byte_idx += ch_len;
                            charpos += 1;
                        }
                        continue;
                    }

                    // Case 2: Space spec — (space :width N) or (space :align-to COL)
                    if is_display_space_spec(&prop_val) {
                        let space_width =
                            parse_display_space_width(&prop_val, face_char_w, x, content_x);
                        if space_width > 0.0 {
                            let bg = Color::from_pixel(default_resolved.bg);
                            frame_glyphs.add_stretch(x, y, space_width, char_h, bg, 0, false);
                            x += space_width;
                            col += (space_width / face_char_w).ceil() as usize;
                        }

                        // Skip covered buffer text
                        let skip_to = display_next_check.min(params.buffer_size);
                        while charpos < skip_to && byte_idx < text.len() {
                            let (_ch, ch_len) = decode_utf8(&text[byte_idx..]);
                            byte_idx += ch_len;
                            charpos += 1;
                        }
                        continue;
                    }

                    // Case 3: Image — show [img] placeholder
                    if is_display_image_spec(&prop_val) {
                        let placeholder = "[img]";
                        let right_limit = content_x + (text_width - lnum_pixel_width);
                        for rch in placeholder.chars() {
                            if x + face_char_w > right_limit {
                                break;
                            }
                            frame_glyphs.add_char(
                                rch,
                                x,
                                y,
                                face_char_w,
                                char_h,
                                face_ascent_val,
                                false,
                            );
                            x += face_char_w;
                            col += 1;
                        }

                        // Skip covered buffer text
                        let skip_to = display_next_check.min(params.buffer_size);
                        while charpos < skip_to && byte_idx < text.len() {
                            let (_ch, ch_len) = decode_utf8(&text[byte_idx..]);
                            byte_idx += ch_len;
                            charpos += 1;
                        }
                        continue;
                    }

                    // Case 4: Raise — (raise FACTOR) or plist with :raise
                    if let Some(factor) = parse_display_raise_factor(&prop_val) {
                        raise_y_offset = -(factor * char_h);
                        raise_end = display_next_check;
                    }

                    // Case 5: Height — (height FACTOR) or plist with :height
                    if let Some(factor) = parse_display_height_factor(&prop_val) {
                        if factor > 0.0 {
                            height_scale = factor;
                            height_end = display_next_check;
                        }
                    }
                    // Other display property types: fall through to normal rendering
                }
            }

            // Decode UTF-8 character
            let ch = match std::str::from_utf8(&text[byte_idx..]) {
                Ok(s) => {
                    let ch = s.chars().next().unwrap_or('\u{FFFD}');
                    byte_idx += ch.len_utf8();
                    ch
                }
                Err(e) => {
                    // Partial valid UTF-8: try decoding from the valid prefix
                    let valid_up_to = e.valid_up_to();
                    if valid_up_to > 0 {
                        if let Ok(s) = std::str::from_utf8(&text[byte_idx..byte_idx + valid_up_to])
                        {
                            let ch = s.chars().next().unwrap_or('\u{FFFD}');
                            byte_idx += ch.len_utf8();
                            ch
                        } else {
                            byte_idx += 1;
                            '\u{FFFD}'
                        }
                    } else {
                        byte_idx += 1;
                        '\u{FFFD}'
                    }
                }
            };

            // Selective display: \r hides rest of line until \n
            if selective_display > 0 && ch == '\r' {
                flush_run(&self.run_buf, frame_glyphs, ligatures);
                self.run_buf.clear();
                // Show ... ellipsis indicator
                let ellipsis = "...";
                for ech in ellipsis.chars() {
                    if x + face_char_w <= content_x + avail_width {
                        frame_glyphs.add_char(
                            ech,
                            x,
                            y + raise_y_offset,
                            face_char_w,
                            char_h,
                            face_ascent_val,
                            false,
                        );
                        x += face_char_w;
                        col += 1;
                    }
                }
                // Skip remaining chars until newline
                charpos += 1;
                while byte_idx < text.len() {
                    let (skip_ch, skip_len) = decode_utf8(&text[byte_idx..]);
                    byte_idx += skip_len;
                    charpos += 1;
                    if skip_ch == '\n' {
                        // Advance to next row (same as newline handler)
                        if row_max_height > char_h {
                            row_extra_y += row_max_height - char_h;
                        }
                        x = content_x;
                        hit_rows.push(HitRow {
                            y_start: y,
                            y_end: y + row_max_height,
                            charpos_start: hit_row_charpos_start,
                            charpos_end: charpos,
                        });
                        hit_row_charpos_start = charpos;
                        row_extend_bg = None;
                        row_extend_row = -1;
                        if box_active {
                            box_start_x = content_x;
                            box_row = row + 1;
                        }
                        reorder_row_bidi(
                            frame_glyphs,
                            row_glyph_start,
                            frame_glyphs.glyphs.len(),
                            content_x,
                        );
                        row_glyph_start = frame_glyphs.glyphs.len();
                        row += 1;
                        y = text_y + row as f32 * char_h + row_extra_y;
                        row_max_height = char_h;
                        row_max_ascent = default_face_ascent;
                        row_y_positions.push(y);
                        col = 0;
                        current_line += 1;
                        need_line_number = lnum_enabled;
                        hscroll_remaining = hscroll;
                        wrap_has_break = false;
                        trailing_ws_start_col = -1;
                        if has_prefix {
                            need_prefix = 1;
                        }
                        break;
                    }
                }
                continue;
            }

            if ch == '\n' {
                flush_run(&self.run_buf, frame_glyphs, ligatures);
                self.run_buf.clear();
                // Highlight trailing whitespace before advancing to next row
                if let Some(tw_bg) = trailing_ws_bg {
                    if trailing_ws_start_col >= 0 && trailing_ws_row == row {
                        let tw_x = trailing_ws_start_x;
                        let tw_w = x - tw_x;
                        if tw_w > 0.0 {
                            frame_glyphs.add_stretch(tw_x, y, tw_w, char_h, tw_bg, 0, false);
                        }
                    }
                }
                trailing_ws_start_col = -1;

                // Face :extend: fill rest of row with extending face background
                if let Some((ext_bg, ext_face_id)) = row_extend_bg {
                    if row_extend_row == row as i32 {
                        let right_edge = content_x + avail_width;
                        if x < right_edge {
                            frame_glyphs.add_stretch(
                                x,
                                y,
                                right_edge - x,
                                row_max_height,
                                ext_bg,
                                ext_face_id,
                                false,
                            );
                        }
                    }
                }
                row_extend_bg = None;
                row_extend_row = -1;

                // Box face tracking: box stays active across line breaks
                if box_active {
                    box_start_x = content_x;
                }

                // Newline: advance to next row
                if row_max_height > char_h {
                    row_extra_y += row_max_height - char_h;
                }
                charpos += 1;

                // Check line-spacing text property on the newline we just consumed.
                // Text property overrides buffer-local line-spacing for that line.
                let text_prop_spacing = {
                    let nl_pos = charpos - 1; // the newline char
                    let buffer_ref = evaluator.buffer_manager().get(buf_id).unwrap();
                    let text_props = super::neovm_bridge::RustTextPropAccess::new(buffer_ref);
                    text_props.check_line_spacing(nl_pos, char_h)
                };
                if text_prop_spacing > 0.0 {
                    row_extra_y += text_prop_spacing;
                } else if params.extra_line_spacing > 0.0 {
                    // Fall back to buffer-local line-spacing
                    row_extra_y += params.extra_line_spacing;
                }

                x = content_x;
                // Record hit-test row (newline ends the row)
                hit_rows.push(HitRow {
                    y_start: y,
                    y_end: y + row_max_height,
                    charpos_start: hit_row_charpos_start,
                    charpos_end: charpos,
                });
                hit_row_charpos_start = charpos;

                reorder_row_bidi(
                    frame_glyphs,
                    row_glyph_start,
                    frame_glyphs.glyphs.len(),
                    content_x,
                );
                row_glyph_start = frame_glyphs.glyphs.len();
                row += 1;
                y = text_y + row as f32 * char_h + row_extra_y;
                row_max_height = char_h;
                row_max_ascent = default_face_ascent;
                row_y_positions.push(y);
                if box_active {
                    box_row = row;
                }
                col = 0;
                current_line += 1;
                need_line_number = lnum_enabled;
                hscroll_remaining = hscroll;
                wrap_has_break = false;
                if has_prefix {
                    need_prefix = 1;
                }
                // Selective display: skip lines indented beyond threshold
                if selective_display > 0 && selective_display < i32::MAX && byte_idx < text.len() {
                    let mut shown_ellipsis = false;
                    loop {
                        if byte_idx >= text.len() {
                            break;
                        }
                        // Peek at indentation of next line
                        let mut indent = 0i32;
                        let mut peek = byte_idx;
                        while peek < text.len() {
                            let b = text[peek];
                            if b == b' ' {
                                indent += 1;
                                peek += 1;
                            } else if b == b'\t' {
                                let tab_w = params.tab_width.max(1) as i32;
                                indent = ((indent / tab_w) + 1) * tab_w;
                                peek += 1;
                            } else {
                                break;
                            }
                        }
                        if indent > selective_display {
                            // Show ... ellipsis once for the hidden block
                            if !shown_ellipsis && row > 0 {
                                let prev_row_y = row_y_positions
                                    .get(row - 1)
                                    .copied()
                                    .unwrap_or(text_y + (row - 1) as f32 * char_h);
                                for dot_i in 0..3 {
                                    let dot_x = content_x + dot_i as f32 * face_char_w;
                                    if dot_x + face_char_w <= content_x + avail_width {
                                        frame_glyphs.add_char(
                                            '.',
                                            dot_x,
                                            prev_row_y,
                                            face_char_w,
                                            char_h,
                                            face_ascent_val,
                                            false,
                                        );
                                    }
                                }
                                shown_ellipsis = true;
                            }
                            // Skip this hidden line
                            while byte_idx < text.len() {
                                let (skip_ch, skip_len) = decode_utf8(&text[byte_idx..]);
                                byte_idx += skip_len;
                                charpos += 1;
                                if skip_ch == '\n' {
                                    current_line += 1;
                                    break;
                                }
                            }
                        } else {
                            break; // Next line is visible
                        }
                    }
                }
                continue;
            }

            if ch == '\t' {
                flush_run(&self.run_buf, frame_glyphs, ligatures);
                self.run_buf.clear();
                // Tab: advance to next tab stop using per-face char width
                let x_before_tab = x;
                let next_tab = if !params.tab_stop_list.is_empty() {
                    // Custom tab stops from tab-stop-list
                    params
                        .tab_stop_list
                        .iter()
                        .find(|&&stop| (stop as usize) > col)
                        .map(|&stop| stop as usize)
                        .unwrap_or_else(|| {
                            // Past last defined stop: use fixed tabs from last stop
                            let last = *params.tab_stop_list.last().unwrap() as usize;
                            let tab_w = params.tab_width.max(1) as usize;
                            if col >= last {
                                last + ((col - last) / tab_w + 1) * tab_w
                            } else {
                                last
                            }
                        })
                } else {
                    let tab_w = params.tab_width as usize;
                    if tab_w > 0 {
                        ((col / tab_w) + 1) * tab_w
                    } else {
                        col + 1
                    }
                };
                // Ensure tab advances at least one column
                let next_tab = next_tab.max(col + 1);
                let spaces = next_tab - col;
                x += spaces as f32 * face_char_w;
                col = next_tab;
                charpos += 1;
                // Tab is a breakpoint for word-wrap
                if params.word_wrap {
                    _wrap_break_col = col;
                    _wrap_break_x = x - content_x;
                    wrap_break_byte_idx = byte_idx;
                    wrap_break_charpos = charpos;
                    flush_run(&self.run_buf, frame_glyphs, ligatures);
                    self.run_buf.clear();
                    wrap_break_glyph_count = frame_glyphs.glyphs.len();
                    wrap_has_break = true;
                }
                // Track trailing whitespace (tab counts as whitespace)
                if trailing_ws_bg.is_some() && trailing_ws_start_col < 0 {
                    trailing_ws_start_col = col as i32;
                    trailing_ws_start_x = x_before_tab;
                    trailing_ws_row = row;
                }
                continue;
            }

            // Control characters: render as ^X notation
            if ch < ' ' || ch == '\x7F' {
                flush_run(&self.run_buf, frame_glyphs, ligatures);
                self.run_buf.clear();
                let ctrl_ch = if ch == '\x7F' {
                    '?'
                } else {
                    char::from((ch as u8) + b'@')
                };
                let needed_width = 2.0 * face_char_w;

                // Check if we have room for ^X (2 columns)
                if x + needed_width > content_x + (text_width - lnum_pixel_width) {
                    // Doesn't fit — wrap or truncate
                    if params.truncate_lines {
                        if row < max_rows {
                            row_truncated[row] = true;
                        }
                        while byte_idx < text.len() {
                            let b = text[byte_idx];
                            byte_idx += 1;
                            charpos += 1;
                            if b == b'\n' {
                                current_line += 1;
                                need_line_number = lnum_enabled;
                                break;
                            }
                        }
                        if row_max_height > char_h {
                            row_extra_y += row_max_height - char_h;
                        }
                        x = content_x;
                        // Record hit-test row (wrap/truncation break)
                        hit_rows.push(HitRow {
                            y_start: y,
                            y_end: y + row_max_height,
                            charpos_start: hit_row_charpos_start,
                            charpos_end: charpos,
                        });
                        hit_row_charpos_start = charpos;
                        row_extend_bg = None;
                        row_extend_row = -1;
                        reorder_row_bidi(
                            frame_glyphs,
                            row_glyph_start,
                            frame_glyphs.glyphs.len(),
                            content_x,
                        );
                        row_glyph_start = frame_glyphs.glyphs.len();
                        row += 1;
                        y = text_y + row as f32 * char_h + row_extra_y;
                        row_max_height = char_h;
                        row_max_ascent = default_face_ascent;
                        row_y_positions.push(y);
                        col = 0;
                        trailing_ws_start_col = -1;
                        if has_prefix {
                            need_prefix = 1;
                        }
                        continue;
                    } else {
                        if row < max_rows {
                            row_continued[row] = true;
                        }
                        if row_max_height > char_h {
                            row_extra_y += row_max_height - char_h;
                        }
                        x = content_x;
                        // Record hit-test row (wrap/truncation break)
                        hit_rows.push(HitRow {
                            y_start: y,
                            y_end: y + row_max_height,
                            charpos_start: hit_row_charpos_start,
                            charpos_end: charpos,
                        });
                        hit_row_charpos_start = charpos;
                        row_extend_bg = None;
                        row_extend_row = -1;
                        reorder_row_bidi(
                            frame_glyphs,
                            row_glyph_start,
                            frame_glyphs.glyphs.len(),
                            content_x,
                        );
                        row_glyph_start = frame_glyphs.glyphs.len();
                        row += 1;
                        y = text_y + row as f32 * char_h + row_extra_y;
                        row_max_height = char_h;
                        row_max_ascent = default_face_ascent;
                        row_y_positions.push(y);
                        col = 0;
                        trailing_ws_start_col = -1;
                        if row < max_rows {
                            row_continuation[row] = true;
                        }
                        if has_prefix {
                            need_prefix = 2;
                        }
                        if row >= max_rows || y + row_max_height > text_y + text_height {
                            break;
                        }
                    }
                }

                // Render ^X with escape-glyph face color
                if params.escape_glyph_fg != 0 {
                    let escape_fg = Color::from_pixel(params.escape_glyph_fg);
                    frame_glyphs.set_face_with_font(
                        current_face_id,
                        escape_fg,
                        Some(default_bg),
                        &default_resolved.font_family,
                        default_resolved.font_weight,
                        default_resolved.italic,
                        default_resolved.font_size,
                        0,
                        None,
                        0,
                        None,
                        0,
                        None,
                        false,
                    );
                    current_face_id += 1;
                }
                frame_glyphs.add_char(
                    '^',
                    x,
                    y + raise_y_offset,
                    face_char_w,
                    char_h,
                    font_ascent,
                    false,
                );
                x += face_char_w;
                frame_glyphs.add_char(
                    ctrl_ch,
                    x,
                    y + raise_y_offset,
                    face_char_w,
                    char_h,
                    font_ascent,
                    false,
                );
                x += face_char_w;
                col += 2;
                charpos += 1;
                face_next_check = 0; // force face re-check to restore text face
                continue;
            }

            // Nobreak character display (U+00A0 non-breaking space, U+00AD soft hyphen)
            if params.nobreak_char_display > 0 && (ch == '\u{00A0}' || ch == '\u{00AD}') {
                flush_run(&self.run_buf, frame_glyphs, ligatures);
                self.run_buf.clear();
                match params.nobreak_char_display {
                    1 => {
                        // Highlight mode: render with nobreak face color
                        if params.nobreak_char_fg != 0 {
                            let nb_fg = Color::from_pixel(params.nobreak_char_fg);
                            frame_glyphs.set_face_with_font(
                                current_face_id,
                                nb_fg,
                                Some(default_bg),
                                &default_resolved.font_family,
                                default_resolved.font_weight,
                                default_resolved.italic,
                                default_resolved.font_size,
                                0,
                                None,
                                0,
                                None,
                                0,
                                None,
                                false,
                            );
                            current_face_id += 1;
                        }
                        // Render as visible space or hyphen
                        let display_ch = if ch == '\u{00A0}' { ' ' } else { '-' };
                        frame_glyphs.add_char(
                            display_ch,
                            x,
                            y + raise_y_offset,
                            face_char_w,
                            char_h,
                            face_ascent_val,
                            false,
                        );
                        x += face_char_w;
                        col += 1;
                        charpos += 1;
                        face_next_check = 0; // restore face on next char
                        continue;
                    }
                    2 => {
                        // Escape notation mode: show as "\\ " for NBSP, "\\-" for soft hyphen
                        let indicator = if ch == '\u{00A0}' { ' ' } else { '-' };
                        if params.nobreak_char_fg != 0 {
                            let nb_fg = Color::from_pixel(params.nobreak_char_fg);
                            frame_glyphs.set_face_with_font(
                                current_face_id,
                                nb_fg,
                                Some(default_bg),
                                &default_resolved.font_family,
                                default_resolved.font_weight,
                                default_resolved.italic,
                                default_resolved.font_size,
                                0,
                                None,
                                0,
                                None,
                                0,
                                None,
                                false,
                            );
                            current_face_id += 1;
                        }
                        // Check if 2 columns fit
                        let needed = 2.0 * face_char_w;
                        if x + needed <= content_x + avail_width {
                            frame_glyphs.add_char(
                                '\\',
                                x,
                                y + raise_y_offset,
                                face_char_w,
                                char_h,
                                face_ascent_val,
                                false,
                            );
                            x += face_char_w;
                            frame_glyphs.add_char(
                                indicator,
                                x,
                                y + raise_y_offset,
                                face_char_w,
                                char_h,
                                face_ascent_val,
                                false,
                            );
                            x += face_char_w;
                            col += 2;
                        }
                        charpos += 1;
                        face_next_check = 0;
                        continue;
                    }
                    _ => {} // mode 0 or unknown: fall through to normal rendering
                }
            }
            // Glyphless character detection (C1 controls, format chars, etc.)
            let glyphless = check_glyphless_char(ch);
            if glyphless > 0 {
                flush_run(&self.run_buf, frame_glyphs, ligatures);
                self.run_buf.clear();

                match glyphless {
                    1 => {
                        // Thin space: advance by a small amount
                        x += face_char_w * 0.25;
                        col += 1;
                    }
                    2 => {
                        // Empty box: render U+25A1 (□) character
                        if x + face_char_w <= content_x + avail_width {
                            frame_glyphs.add_char(
                                '\u{25A1}',
                                x,
                                y + raise_y_offset,
                                face_char_w,
                                char_h,
                                face_ascent_val,
                                false,
                            );
                            x += face_char_w;
                            col += 1;
                        }
                    }
                    3 => {
                        // Hex code: render as U+XXXX
                        let hex_str = if (ch as u32) < 0x10000 {
                            format!("U+{:04X}", ch as u32)
                        } else {
                            format!("U+{:06X}", ch as u32)
                        };
                        let needed = hex_str.len() as f32 * face_char_w;

                        // Use glyphless-char face color if available
                        if params.glyphless_char_fg != 0 {
                            let glyph_fg = Color::from_pixel(params.glyphless_char_fg);
                            frame_glyphs.set_face_with_font(
                                current_face_id,
                                glyph_fg,
                                Some(default_bg),
                                &default_resolved.font_family,
                                default_resolved.font_weight,
                                default_resolved.italic,
                                default_resolved.font_size,
                                0,
                                None,
                                0,
                                None,
                                0,
                                None,
                                false,
                            );
                            current_face_id += 1;
                        }

                        let right_limit = content_x + avail_width;
                        if x + needed <= right_limit {
                            for hch in hex_str.chars() {
                                frame_glyphs.add_char(
                                    hch,
                                    x,
                                    y + raise_y_offset,
                                    face_char_w,
                                    char_h,
                                    face_ascent_val,
                                    false,
                                );
                                x += face_char_w;
                            }
                            col += hex_str.len();
                        } else {
                            // Partial rendering: emit as many chars as fit
                            for hch in hex_str.chars() {
                                if x + face_char_w > right_limit {
                                    break;
                                }
                                frame_glyphs.add_char(
                                    hch,
                                    x,
                                    y + raise_y_offset,
                                    face_char_w,
                                    char_h,
                                    face_ascent_val,
                                    false,
                                );
                                x += face_char_w;
                                col += 1;
                            }
                        }
                        face_next_check = 0; // restore face on next char
                    }
                    5 => {
                        // Zero width: skip entirely (no visual output)
                    }
                    _ => {}
                }
                charpos += 1;
                continue;
            }

            // Check for line wrap / truncation using per-face char width

            // Compute wide-char advance: CJK chars occupy 2 columns
            let char_cols = if is_wide_char(ch) { 2 } else { 1 };
            let advance = char_cols as f32 * face_char_w;

            if x + advance > content_x + avail_width {
                flush_run(&self.run_buf, frame_glyphs, ligatures);
                self.run_buf.clear();
                if params.truncate_lines {
                    if row < max_rows {
                        row_truncated[row] = true;
                    }
                    // Skip remaining chars until newline
                    while byte_idx < text.len() {
                        let b = text[byte_idx];
                        byte_idx += 1;
                        charpos += 1;
                        if b == b'\n' {
                            current_line += 1;
                            need_line_number = lnum_enabled;
                            break;
                        }
                    }
                    if row_max_height > char_h {
                        row_extra_y += row_max_height - char_h;
                    }
                    x = content_x;
                    // Record hit-test row (wrap/truncation break)
                    hit_rows.push(HitRow {
                        y_start: y,
                        y_end: y + row_max_height,
                        charpos_start: hit_row_charpos_start,
                        charpos_end: charpos,
                    });
                    hit_row_charpos_start = charpos;
                    row_extend_bg = None;
                    row_extend_row = -1;
                    reorder_row_bidi(
                        frame_glyphs,
                        row_glyph_start,
                        frame_glyphs.glyphs.len(),
                        content_x,
                    );
                    row_glyph_start = frame_glyphs.glyphs.len();
                    row += 1;
                    y = text_y + row as f32 * char_h + row_extra_y;
                    row_max_height = char_h;
                    row_max_ascent = default_face_ascent;
                    row_y_positions.push(y);
                    col = 0;
                    wrap_has_break = false;
                    trailing_ws_start_col = -1;
                    if has_prefix {
                        need_prefix = 1;
                    }
                    continue;
                } else if params.word_wrap && wrap_has_break {
                    // Word-wrap: rewind to last break point
                    frame_glyphs.glyphs.truncate(wrap_break_glyph_count);
                    byte_idx = wrap_break_byte_idx;
                    charpos = wrap_break_charpos;
                    col = 0;

                    if row < max_rows {
                        row_continued[row] = true;
                    }
                    if row_max_height > char_h {
                        row_extra_y += row_max_height - char_h;
                    }
                    x = content_x;
                    // Record hit-test row (wrap/truncation break)
                    hit_rows.push(HitRow {
                        y_start: y,
                        y_end: y + row_max_height,
                        charpos_start: hit_row_charpos_start,
                        charpos_end: charpos,
                    });
                    hit_row_charpos_start = charpos;
                    row_extend_bg = None;
                    row_extend_row = -1;
                    reorder_row_bidi(
                        frame_glyphs,
                        row_glyph_start,
                        frame_glyphs.glyphs.len(),
                        content_x,
                    );
                    row_glyph_start = frame_glyphs.glyphs.len();
                    row += 1;
                    y = text_y + row as f32 * char_h + row_extra_y;
                    row_max_height = char_h;
                    row_max_ascent = default_face_ascent;
                    row_y_positions.push(y);
                    if row < max_rows {
                        row_continuation[row] = true;
                    }
                    wrap_has_break = false;
                    trailing_ws_start_col = -1;
                    if has_prefix {
                        need_prefix = 2;
                    }

                    // Force face re-check since we rewound
                    face_next_check = 0;

                    if row >= max_rows || y + row_max_height > text_y + text_height {
                        break;
                    }
                    continue;
                } else {
                    // Character wrap (no break point available)
                    if row < max_rows {
                        row_continued[row] = true;
                    }
                    if row_max_height > char_h {
                        row_extra_y += row_max_height - char_h;
                    }
                    x = content_x;
                    // Record hit-test row (wrap/truncation break)
                    hit_rows.push(HitRow {
                        y_start: y,
                        y_end: y + row_max_height,
                        charpos_start: hit_row_charpos_start,
                        charpos_end: charpos,
                    });
                    hit_row_charpos_start = charpos;
                    row_extend_bg = None;
                    row_extend_row = -1;
                    reorder_row_bidi(
                        frame_glyphs,
                        row_glyph_start,
                        frame_glyphs.glyphs.len(),
                        content_x,
                    );
                    row_glyph_start = frame_glyphs.glyphs.len();
                    row += 1;
                    y = text_y + row as f32 * char_h + row_extra_y;
                    row_max_height = char_h;
                    row_max_ascent = default_face_ascent;
                    row_y_positions.push(y);
                    col = 0;
                    trailing_ws_start_col = -1;
                    if row < max_rows {
                        row_continuation[row] = true;
                    }
                    if has_prefix {
                        need_prefix = 2;
                    }
                    if row >= max_rows || y + row_max_height > text_y + text_height {
                        break;
                    }
                }
            }

            // Reset raise offset when past the raise region
            if raise_end > window_start && charpos >= raise_end {
                raise_y_offset = 0.0;
                raise_end = window_start;
            }
            // Reset height scale when past the height region
            if height_end > window_start && charpos >= height_end {
                height_scale = 0.0;
                height_end = window_start;
            }
            // Resolve face at current position if needed
            if (charpos as usize) >= face_next_check {
                flush_run(&self.run_buf, frame_glyphs, ligatures);
                self.run_buf.clear();
                let buffer_ref = evaluator.buffer_manager().get(buf_id).unwrap();
                let resolved =
                    face_resolver.face_at_pos(buffer_ref, charpos as usize, &mut face_next_check);

                // Query per-face font metrics from FontMetricsService
                let metrics = self.font_metrics.as_mut().map(|svc| {
                    svc.font_metrics(
                        &resolved.font_family,
                        resolved.font_weight,
                        resolved.italic,
                        resolved.font_size,
                    )
                });
                if let Some(m) = metrics {
                    face_char_w = m.char_width;
                    face_h = m.line_height;
                    face_ascent_val = m.ascent;
                } else {
                    // No FontMetricsService — fall back to window defaults
                    face_char_w = char_w;
                    face_h = char_h;
                    face_ascent_val = font_ascent;
                }

                // Track max glyph height for variable-height rows
                if face_h > row_max_height {
                    row_max_height = face_h;
                }
                if face_ascent_val > row_max_ascent {
                    row_max_ascent = face_ascent_val;
                }

                let fg = Color::from_pixel(resolved.fg);
                current_fg = fg;
                let bg = Color::from_pixel(resolved.bg);
                current_bg = bg;
                let ul_color = if resolved.underline_style > 0 && resolved.underline_color != 0 {
                    Some(Color::from_pixel(resolved.underline_color))
                } else {
                    None
                };
                let st_color = if resolved.strike_through && resolved.strike_through_color != 0 {
                    Some(Color::from_pixel(resolved.strike_through_color))
                } else {
                    None
                };
                let ol_color = if resolved.overline && resolved.overline_color != 0 {
                    Some(Color::from_pixel(resolved.overline_color))
                } else {
                    None
                };

                frame_glyphs.set_face_with_font(
                    current_face_id,
                    fg,
                    Some(bg),
                    &resolved.font_family,
                    resolved.font_weight,
                    resolved.italic,
                    resolved.font_size,
                    resolved.underline_style,
                    ul_color,
                    if resolved.strike_through { 1 } else { 0 },
                    st_color,
                    if resolved.overline { 1 } else { 0 },
                    ol_color,
                    resolved.overstrike,
                );
                current_face_id += 1;

                // Track last face with :extend on this row
                if resolved.extend {
                    let ext_bg = Color::from_pixel(resolved.bg);
                    row_extend_bg = Some((ext_bg, current_face_id - 1));
                    row_extend_row = row as i32;
                }

                // Box face tracking: close previous box and open new one if face has :box
                if box_active && resolved.box_type == 0 {
                    box_active = false;
                }
                if resolved.box_type > 0 {
                    box_active = true;
                    box_start_x = x;
                    box_row = row;
                }
            }

            // Capture cursor metrics at point position during the main layout
            // so cursor emission uses the correct per-face height/width.
            if cursor_info.is_none() && charpos == params.point {
                cursor_info = Some((
                    x,
                    y,
                    face_char_w,
                    face_h,
                    face_ascent_val,
                    current_fg,
                    current_bg,
                    byte_idx,
                    col,
                ));
            }

            // --- Overlay before-strings ---
            if has_overlays {
                let text_props = super::neovm_bridge::RustTextPropAccess::new(buffer);
                let (before_strings, _) = text_props.overlay_strings_at(charpos);
                if !before_strings.is_empty() {
                    // Flush run buffer before emitting overlay chars
                    flush_run(&self.run_buf, frame_glyphs, ligatures);
                    self.run_buf.clear();
                    let right_limit = content_x + avail_width;
                    for (string_bytes, overlay_id) in &before_strings {
                        let ov_face = buffer
                            .overlays
                            .overlay_get(*overlay_id, "face")
                            .and_then(|val| face_resolver.resolve_face_from_value(val));
                        render_overlay_string(
                            string_bytes,
                            &mut x,
                            y + raise_y_offset,
                            &mut col,
                            face_char_w,
                            char_h,
                            face_ascent_val,
                            right_limit,
                            frame_glyphs,
                            ov_face.as_ref(),
                            &mut current_face_id,
                        );
                    }
                }
            }

            // Accumulate character into ligature run buffer
            if self.run_buf.is_empty() {
                let gy = y + raise_y_offset;
                self.run_buf.start(
                    x,
                    gy,
                    char_h,
                    face_ascent_val,
                    current_face_id.saturating_sub(1),
                    false,
                    height_scale,
                );
            }
            self.run_buf.push(ch, advance);

            // Flush if run is too long
            if self.run_buf.len() >= MAX_LIGATURE_RUN_LEN {
                flush_run(&self.run_buf, frame_glyphs, ligatures);
                self.run_buf.clear();
            }

            x += advance;
            col += char_cols as usize;
            charpos += 1;

            // --- Overlay after-strings ---
            if has_overlays {
                let text_props = super::neovm_bridge::RustTextPropAccess::new(buffer);
                let (_, after_strings) = text_props.overlay_strings_at(charpos);
                if !after_strings.is_empty() {
                    // Flush run buffer before emitting overlay chars
                    flush_run(&self.run_buf, frame_glyphs, ligatures);
                    self.run_buf.clear();
                    let right_limit = content_x + avail_width;
                    for (string_bytes, overlay_id) in &after_strings {
                        let ov_face = buffer
                            .overlays
                            .overlay_get(*overlay_id, "face")
                            .and_then(|val| face_resolver.resolve_face_from_value(val));
                        render_overlay_string(
                            string_bytes,
                            &mut x,
                            y + raise_y_offset,
                            &mut col,
                            face_char_w,
                            char_h,
                            face_ascent_val,
                            right_limit,
                            frame_glyphs,
                            ov_face.as_ref(),
                            &mut current_face_id,
                        );
                    }
                }
            }

            // Space is a breakpoint for word-wrap
            if params.word_wrap && ch == ' ' {
                _wrap_break_col = col;
                _wrap_break_x = x - content_x;
                wrap_break_byte_idx = byte_idx;
                wrap_break_charpos = charpos;
                flush_run(&self.run_buf, frame_glyphs, ligatures);
                self.run_buf.clear();
                wrap_break_glyph_count = frame_glyphs.glyphs.len();
                wrap_has_break = true;
            }

            // Track trailing whitespace
            if trailing_ws_bg.is_some() {
                if ch == ' ' || ch == '\t' {
                    if trailing_ws_start_col < 0 {
                        trailing_ws_start_col = (col as i32) - 1;
                        trailing_ws_start_x = x - advance;
                        trailing_ws_row = row;
                    }
                } else {
                    trailing_ws_start_col = -1;
                }
            }
        }

        flush_run(&self.run_buf, frame_glyphs, ligatures);
        self.run_buf.clear();

        let point_is_visible_eob =
            params.point == params.buffer_size + 1 && charpos == params.buffer_size;

        // Capture cursor at end-of-buffer position.
        // GNU Emacs shows point at point-max+1 as a real cursor location.
        if cursor_info.is_none() && (charpos == params.point || point_is_visible_eob) {
            if point_is_visible_eob {
                tracing::debug!(
                    "layout_window_rust: capturing EOB cursor at x={:.1} y={:.1} point={} point-max={}",
                    x,
                    y,
                    params.point,
                    params.buffer_size
                );
            }
            cursor_info = Some((
                x,
                y,
                face_char_w,
                face_h,
                face_ascent_val,
                current_fg,
                current_bg,
                byte_idx,
                col,
            ));
        }

        // Close any remaining box face region at end of text
        if box_active {
            let _ = (box_start_x, box_row); // suppress unused warnings
        }

        // EOB overlay strings: check for overlay strings at the end-of-buffer position
        if has_overlays && row < max_rows {
            let text_props = super::neovm_bridge::RustTextPropAccess::new(buffer);
            let (before_strings, after_strings) = text_props.overlay_strings_at(charpos);
            let right_limit = content_x + avail_width;
            for (string_bytes, overlay_id) in before_strings.iter().chain(after_strings.iter()) {
                let ov_face = buffer
                    .overlays
                    .overlay_get(*overlay_id, "face")
                    .and_then(|val| face_resolver.resolve_face_from_value(val));
                render_overlay_string(
                    string_bytes,
                    &mut x,
                    y + raise_y_offset,
                    &mut col,
                    face_char_w,
                    char_h,
                    face_ascent_val,
                    right_limit,
                    frame_glyphs,
                    ov_face.as_ref(),
                    &mut current_face_id,
                );
            }
        }

        // Reorder final partial row (bidi)
        reorder_row_bidi(
            frame_glyphs,
            row_glyph_start,
            frame_glyphs.glyphs.len(),
            content_x,
        );

        // Face :extend at end-of-buffer: fill remaining empty rows
        // with the last :extend face's background color
        if let Some((ext_bg, ext_face_id)) = row_extend_bg {
            let right_edge = content_x + avail_width;
            // First, extend the current (partially filled) row if text didn't fill it
            if x < right_edge && row < max_rows {
                let ry = row_y_positions
                    .get(row)
                    .copied()
                    .unwrap_or(text_y + row as f32 * char_h + row_extra_y);
                frame_glyphs.add_stretch(
                    x,
                    ry,
                    right_edge - x,
                    row_max_height,
                    ext_bg,
                    ext_face_id,
                    false,
                );
            }
            // Then fill completely empty rows below
            let start_row = (row + 1).min(max_rows);
            for r in start_row..max_rows {
                let ry = row_y_positions
                    .get(r)
                    .copied()
                    .unwrap_or(text_y + r as f32 * char_h + row_extra_y);
                if ry + char_h > text_y + text_height {
                    break;
                } // Don't extend past text area
                frame_glyphs.add_stretch(
                    content_x,
                    ry,
                    avail_width,
                    char_h,
                    ext_bg,
                    ext_face_id,
                    false,
                );
            }
        }

        // Render fringe indicators
        if params.left_fringe_width > 0.0 || params.right_fringe_width > 0.0 {
            let fringe_char_w = params.left_fringe_width.min(char_w).max(char_w * 0.5);

            for r in 0..row.min(max_rows) {
                let gy = row_y_positions
                    .get(r)
                    .copied()
                    .unwrap_or(text_y + r as f32 * char_h);

                // Right fringe: continuation arrow for wrapped lines
                if params.right_fringe_width > 0.0 && row_continued.get(r).copied().unwrap_or(false)
                {
                    frame_glyphs.add_char(
                        '\u{21B5}', // downwards arrow with corner leftwards
                        right_fringe_x,
                        gy,
                        fringe_char_w,
                        char_h,
                        font_ascent,
                        false,
                    );
                }

                // Right fringe: truncation indicator
                if params.right_fringe_width > 0.0 && row_truncated.get(r).copied().unwrap_or(false)
                {
                    frame_glyphs.add_char(
                        '\u{2192}', // rightwards arrow
                        right_fringe_x,
                        gy,
                        fringe_char_w,
                        char_h,
                        font_ascent,
                        false,
                    );
                }

                // Left fringe: continuation from previous line
                if params.left_fringe_width > 0.0
                    && row_continuation.get(r).copied().unwrap_or(false)
                {
                    frame_glyphs.add_char(
                        '\u{21AA}', // rightwards arrow with hook
                        left_fringe_x,
                        gy,
                        fringe_char_w,
                        char_h,
                        font_ascent,
                        false,
                    );
                }
            }

            // Empty line indicators (after buffer text ends)
            if params.indicate_empty_lines > 0 {
                let eob_start = row.min(max_rows);
                for r in eob_start..max_rows {
                    let gy = row_y_positions
                        .get(r)
                        .copied()
                        .unwrap_or(text_y + r as f32 * char_h + row_extra_y);
                    let fringe_x = if params.indicate_empty_lines == 2 {
                        right_fringe_x
                    } else {
                        left_fringe_x
                    };
                    let fringe_w = if params.indicate_empty_lines == 2 {
                        params.right_fringe_width
                    } else {
                        params.left_fringe_width
                    };
                    if fringe_w > 0.0 {
                        frame_glyphs.add_char(
                            '~', // tilde for empty lines (like vi)
                            fringe_x,
                            gy,
                            fringe_char_w,
                            char_h,
                            font_ascent,
                            false,
                        );
                    }
                }
            }
        }

        // Render fill-column indicator
        if params.fill_column_indicator > 0 {
            let fci_col = params.fill_column_indicator;
            let fci_char = params.fill_column_indicator_char;
            let fci_fg = if params.fill_column_indicator_fg != 0 {
                Color::from_pixel(params.fill_column_indicator_fg)
            } else {
                default_fg
            };

            frame_glyphs.set_face(
                0,
                fci_fg,
                Some(default_bg),
                400,
                false,
                0,
                None,
                0,
                None,
                0,
                None,
            );

            // Draw indicator character at the fill column on each row
            if (fci_col as usize) < cols {
                let indicator_x = content_x + fci_col as f32 * char_w;
                let total_rows = row.min(max_rows);
                for r in 0..total_rows {
                    let gy = row_y_positions
                        .get(r)
                        .copied()
                        .unwrap_or(text_y + r as f32 * char_h);
                    if indicator_x < content_x + avail_width {
                        frame_glyphs.add_char(
                            fci_char,
                            indicator_x,
                            gy,
                            char_w,
                            char_h,
                            font_ascent,
                            false,
                        );
                    }
                }
            }
        }

        // Emit cursor if point is within the visible region.
        // Use cursor_info captured during the main layout loop when available
        // (provides correct per-face metrics for variable-height faces).
        // Falls back to a re-scan with default face metrics otherwise.
        if params.point >= window_start && (params.point <= charpos || point_is_visible_eob) {
            let cursor_style = cursor_style_for_window(params);

            if let Some((
                cx,
                cy,
                cursor_face_w,
                cursor_face_h,
                _cursor_face_ascent,
                cursor_fg,
                cursor_face_bg,
                cbyte,
                ccol,
            )) = cursor_info
            {
                // Cursor position and face metrics captured during the main layout loop
                if cy >= text_y && cy + cursor_face_h <= text_y + text_height {
                    if let Some(style) = cursor_style {
                        let cursor_w = cursor_width_for_style(
                            style,
                            text,
                            cbyte,
                            ccol as i32,
                            params,
                            cursor_face_w,
                        );
                        frame_glyphs.add_cursor(
                            params.window_id as i32,
                            cx,
                            cy,
                            cursor_w,
                            cursor_face_h,
                            style,
                            cursor_fg,
                        );

                        if point_is_visible_eob {
                            tracing::debug!(
                                "layout_window_rust: emitting EOB cursor at x={:.1} y={:.1} w={:.1} h={:.1}",
                                cx,
                                cy,
                                cursor_w,
                                cursor_face_h
                            );
                        }

                        // For FilledBox cursor, use the renderer's cursor_inverse system
                        // to swap fg/bg of the character under the cursor.
                        if matches!(style, CursorStyle::FilledBox) {
                            tracing::debug!(
                                "cursor_inverse: cx={:.1} cy={:.1} w={:.1} h={:.1} fg=({:.3},{:.3},{:.3}) bg=({:.3},{:.3},{:.3})",
                                cx,
                                cy,
                                cursor_w,
                                cursor_face_h,
                                cursor_fg.r,
                                cursor_fg.g,
                                cursor_fg.b,
                                cursor_face_bg.r,
                                cursor_face_bg.g,
                                cursor_face_bg.b,
                            );
                            frame_glyphs.set_cursor_inverse(
                                cx,
                                cy,
                                cursor_w,
                                cursor_face_h,
                                cursor_fg,
                                cursor_face_bg,
                            );
                        }
                    }
                }
            } else {
                // Fallback: re-scan to find cursor position using default face metrics
                let mut cx = content_x;
                let mut cy = text_y;
                let mut cpos = window_start;
                let mut cbyte = 0usize;
                let mut ccol = 0usize;

                let cursor_char_w = default_face_char_w;

                let mut cinvis_next_check: i64 = window_start;
                let mut cdisplay_next_check: i64 = window_start;
                let mut c_hscroll_remaining = hscroll;

                while cbyte < text.len() && cpos < params.point {
                    // Skip invisible text in cursor scan
                    if cpos >= cinvis_next_check {
                        let text_props = super::neovm_bridge::RustTextPropAccess::new(buffer);
                        let (cinvis, cnext) = text_props.check_invisible(cpos);
                        if cinvis {
                            let skip_to = cnext.min(params.point);
                            while cpos < skip_to && cbyte < text.len() {
                                let (_ch, ch_len) = decode_utf8(&text[cbyte..]);
                                cbyte += ch_len;
                                cpos += 1;
                            }
                            cinvis_next_check = cnext;
                            continue;
                        }
                        cinvis_next_check = cnext;
                    }

                    // Handle hscroll in cursor scan: skip columns consumed by horizontal scroll
                    if c_hscroll_remaining > 0 {
                        let (cch, ch_len) = decode_utf8(&text[cbyte..]);
                        cbyte += ch_len;
                        cpos += 1;

                        if cch == '\n' {
                            cx = content_x;
                            cy += char_h;
                            ccol = 0;
                            c_hscroll_remaining = hscroll;
                        } else {
                            let ch_cols: i32 = if cch == '\t' {
                                let tab_w = params.tab_width.max(1) as i32;
                                let consumed = hscroll - c_hscroll_remaining;
                                ((consumed / tab_w + 1) * tab_w) - consumed
                            } else if is_wide_char(cch) {
                                2
                            } else {
                                1
                            };
                            c_hscroll_remaining -= ch_cols.min(c_hscroll_remaining);

                            // After hscroll is exhausted, account for the $ indicator
                            if c_hscroll_remaining <= 0 && show_left_trunc {
                                ccol = 1; // $ takes 1 column
                                cx = content_x + cursor_char_w;
                            }
                        }
                        continue;
                    }

                    // Account for display property width in cursor position
                    if cpos >= cdisplay_next_check {
                        let display_prop_val: Option<neovm_core::emacs_core::Value> = {
                            let text_props = super::neovm_bridge::RustTextPropAccess::new(buffer);
                            let (dp, next_change) = text_props.check_display_prop(cpos);
                            cdisplay_next_check = next_change;
                            dp.copied()
                        };

                        if let Some(prop_val) = display_prop_val {
                            if let Some(replacement) = prop_val.as_str() {
                                // String replacement: advance cursor by replacement width
                                let rep_cols: usize = replacement
                                    .chars()
                                    .map(|rc| if is_wide_char(rc) { 2 } else { 1 })
                                    .sum();
                                cx += rep_cols as f32 * cursor_char_w;
                                ccol += rep_cols;
                                // Skip covered buffer text
                                let skip_to = cdisplay_next_check.min(params.point);
                                while cpos < skip_to && cbyte < text.len() {
                                    let (_ch, ch_len) = decode_utf8(&text[cbyte..]);
                                    cbyte += ch_len;
                                    cpos += 1;
                                }
                                continue;
                            } else if is_display_space_spec(&prop_val) {
                                let space_width = parse_display_space_width(
                                    &prop_val,
                                    cursor_char_w,
                                    cx,
                                    content_x,
                                );
                                cx += space_width;
                                ccol += (space_width / cursor_char_w).ceil() as usize;
                                let skip_to = cdisplay_next_check.min(params.point);
                                while cpos < skip_to && cbyte < text.len() {
                                    let (_ch, ch_len) = decode_utf8(&text[cbyte..]);
                                    cbyte += ch_len;
                                    cpos += 1;
                                }
                                continue;
                            } else if is_display_image_spec(&prop_val) {
                                let placeholder_len = 5; // "[img]"
                                cx += placeholder_len as f32 * cursor_char_w;
                                ccol += placeholder_len;
                                let skip_to = cdisplay_next_check.min(params.point);
                                while cpos < skip_to && cbyte < text.len() {
                                    let (_ch, ch_len) = decode_utf8(&text[cbyte..]);
                                    cbyte += ch_len;
                                    cpos += 1;
                                }
                                continue;
                            }
                        }
                    }

                    let cch = match std::str::from_utf8(&text[cbyte..]) {
                        Ok(s) => {
                            let c = s.chars().next().unwrap_or('\u{FFFD}');
                            cbyte += c.len_utf8();
                            c
                        }
                        Err(e) => {
                            let valid_up_to = e.valid_up_to();
                            if valid_up_to > 0 {
                                if let Ok(s) =
                                    std::str::from_utf8(&text[cbyte..cbyte + valid_up_to])
                                {
                                    let c = s.chars().next().unwrap_or('\u{FFFD}');
                                    cbyte += c.len_utf8();
                                    c
                                } else {
                                    cbyte += 1;
                                    '\u{FFFD}'
                                }
                            } else {
                                cbyte += 1;
                                '\u{FFFD}'
                            }
                        }
                    };

                    if cch == '\n' {
                        cx = content_x;
                        cy += char_h;
                        ccol = 0;
                        c_hscroll_remaining = hscroll;
                    } else if cch == '\t' {
                        let next_tab =
                            next_tab_stop_col(ccol, params.tab_width, &params.tab_stop_list)
                                .max(ccol + 1);
                        cx += (next_tab - ccol) as f32 * cursor_char_w;
                        ccol = next_tab;
                    } else {
                        let c_cols = if is_wide_char(cch) { 2 } else { 1 };
                        let c_advance = c_cols as f32 * cursor_char_w;
                        if !params.truncate_lines
                            && cx + c_advance > content_x + (text_width - lnum_pixel_width)
                        {
                            cx = content_x;
                            cy += char_h;
                            ccol = 0;
                        }
                        cx += c_advance;
                        ccol += c_cols as usize;
                    }
                    cpos += 1;
                }

                // Only emit cursor if it's within visible area
                if cy >= text_y && cy + char_h <= text_y + text_height {
                    if let Some(style) = cursor_style {
                        let cursor_w = cursor_width_for_style(
                            style,
                            text,
                            cbyte,
                            ccol as i32,
                            params,
                            default_face_char_w,
                        );
                        frame_glyphs.add_cursor(
                            params.window_id as i32,
                            cx,
                            cy,
                            cursor_w,
                            char_h,
                            style,
                            Color::from_pixel(params.cursor_color),
                        );

                        // For FilledBox cursor, use the renderer's cursor_inverse system
                        // to swap fg/bg of the character under the cursor.
                        if matches!(style, CursorStyle::FilledBox) {
                            frame_glyphs.set_cursor_inverse(
                                cx,
                                cy,
                                cursor_w,
                                char_h,
                                Color::from_pixel(params.cursor_color),
                                default_bg,
                            );
                        }
                    }
                }
            } // end else (fallback re-scan)
        }

        // If point is beyond the computed window_end, scan backward from
        // point to compute a new window_start that places point ~75% down
        // the window.  We do this here (before mode-line evaluation) while
        // buf_access is still available; the result is applied in the
        // writeback block below.
        let scroll_down_ws: Option<i64> = if params.point > charpos
            && charpos > window_start
            && !params.is_minibuffer
        {
            let target_rows_above = ((max_rows * 3) / 4).max(1) as i64;
            let mut lines_back: i64 = 0;
            let mut scan_pos = params.point;

            while scan_pos > params.buffer_begv && lines_back < target_rows_above {
                scan_pos -= 1;
                let bp = buf_access.charpos_to_bytepos(scan_pos);
                if buf_access.byte_at(bp) == Some(b'\n') {
                    lines_back += 1;
                }
            }

            let new_ws = scan_pos.max(params.buffer_begv);
            tracing::debug!(
                "layout_window_rust: scroll-down, point={} beyond window_end={}, new window_start={}",
                params.point,
                charpos,
                new_ws
            );
            Some(new_ws)
        } else {
            None
        };

        // Mode-line: evaluate format-mode-line or fall back to buffer name
        if params.mode_line_height > 0.0 {
            let ml_y = params.bounds.y + params.bounds.height - params.mode_line_height;
            let ml_face_name = if params.selected {
                "mode-line"
            } else {
                "mode-line-inactive"
            };
            let ml_face = face_resolver.resolve_named_face(ml_face_name);
            tracing::info!(
                "mode-line face '{}': fg=0x{:06X} bg=0x{:06X}",
                ml_face_name, ml_face.fg, ml_face.bg
            );

            let mode_text = {
                let result = eval_status_line_format(
                    evaluator,
                    "mode-line-format",
                    params.window_id,
                    params.buffer_id,
                )
                .unwrap_or_else(|| format!(" {} ", buffer_name));
                tracing::debug!(
                    "mode-line eval result: {:?} (len={})",
                    &result[..result.len().min(120)],
                    result.len()
                );
                result
            };

            self.render_rust_status_line_plain(
                params.bounds.x,
                ml_y,
                params.bounds.width,
                params.mode_line_height,
                params.window_id,
                char_w,
                font_ascent,
                current_face_id,
                &ml_face,
                mode_text,
                frame_glyphs,
                StatusLineKind::ModeLine,
            );
            current_face_id += 1;
        }

        // Header-line: evaluate format-mode-line with header-line-format
        if params.header_line_height > 0.0 {
            let hl_y = params.bounds.y + params.tab_line_height;
            let hl_face = face_resolver.resolve_named_face("header-line");

            let header_text = eval_status_line_format(
                evaluator,
                "header-line-format",
                params.window_id,
                params.buffer_id,
            )
            .unwrap_or_default();

            self.render_rust_status_line_plain(
                params.bounds.x,
                hl_y,
                params.bounds.width,
                params.header_line_height,
                params.window_id,
                char_w,
                font_ascent,
                current_face_id,
                &hl_face,
                header_text,
                frame_glyphs,
                StatusLineKind::HeaderLine,
            );
            current_face_id += 1;
        }

        // Tab-line: evaluate format-mode-line with tab-line-format
        if params.tab_line_height > 0.0 {
            // Tab-line is above header-line (at the very top of the window)
            let tl_y = params.bounds.y;
            let tl_face = face_resolver.resolve_named_face("tab-line");

            let tab_text = eval_status_line_format(
                evaluator,
                "tab-line-format",
                params.window_id,
                params.buffer_id,
            )
            .unwrap_or_default();

            self.render_rust_status_line_plain(
                params.bounds.x,
                tl_y,
                params.bounds.width,
                params.tab_line_height,
                params.window_id,
                char_w,
                font_ascent,
                current_face_id,
                &tl_face,
                tab_text,
                frame_glyphs,
                StatusLineKind::TabLine,
            );
        }

        // Record last hit-test row (end of visible text)
        if row < max_rows && charpos > hit_row_charpos_start {
            let row_y_start = row_y_positions
                .get(row)
                .copied()
                .unwrap_or(text_y + row as f32 * char_h + row_extra_y);
            hit_rows.push(HitRow {
                y_start: row_y_start,
                y_end: row_y_start + row_max_height,
                charpos_start: hit_row_charpos_start,
                charpos_end: charpos,
            });
        }

        // Store hit-test data for this window
        self.hit_data.push(WindowHitData {
            window_id: params.window_id,
            content_x,
            char_w,
            rows: hit_rows,
        });

        tracing::debug!("  layout_window_rust: window_end charpos={}", charpos);

        // Write adjusted window_start and window_end back to the evaluator's
        // Window struct so that scrolling, (window-start), and (window-end)
        // reflect the layout results.  If a scroll-down adjustment was
        // computed above, apply it instead of the current window_start.
        {
            let win_id = neovm_core::window::WindowId(params.window_id as u64);
            let adjusted_ws = scroll_down_ws.unwrap_or(window_start) as usize;
            let window_end_charpos = charpos;

            if let Some(frame) = evaluator.frame_manager_mut().get_mut(frame_id) {
                let update_window = |w: &mut neovm_core::window::Window| {
                    if let neovm_core::window::Window::Leaf {
                        window_start: ws,
                        parameters: params_map,
                        ..
                    } = w
                    {
                        *ws = adjusted_ws;
                        params_map.insert(
                            "window-end".to_string(),
                            neovm_core::emacs_core::Value::Int(window_end_charpos),
                        );
                    }
                };

                if let Some(window) = frame.root_window.find_mut(win_id) {
                    update_window(window);
                } else if let Some(ref mut mini) = frame.minibuffer_leaf {
                    if mini.id() == win_id {
                        update_window(mini);
                    }
                }
            }
        }
    }

    /// Trigger fontification for a buffer region via the Rust Evaluator.
    ///
    /// Calls `(run-hook-with-args 'fontification-functions START)` if
    /// `fontification-functions` is bound and non-nil.  This is the same
    /// mechanism Emacs uses in `handle_fontified_prop` to ensure text
    /// properties (e.g. `font-lock-face`) are set before display.
    ///
    /// Errors are non-fatal: layout continues without fontification if
    /// the hook signals or is not configured.
    fn ensure_fontified_rust(
        evaluator: &mut neovm_core::emacs_core::Evaluator,
        _buf_id: neovm_core::buffer::BufferId,
        from: i64,
        _to: i64,
    ) {
        // Check if fontification-functions is bound and non-nil by evaluating
        // the symbol.  The Evaluator does not expose a get_variable() API, so
        // we parse and eval the symbol name.
        let has_fontification = match neovm_core::emacs_core::parse_forms("fontification-functions")
        {
            Ok(forms) if !forms.is_empty() => match evaluator.eval_expr(&forms[0]) {
                Ok(val) => !val.is_nil(),
                Err(_) => false,
            },
            _ => false,
        };

        if !has_fontification {
            return; // No fontification configured
        }

        // Call (run-hook-with-args 'fontification-functions FROM).
        // This is what Emacs does in handle_fontified_prop to trigger
        // jit-lock-fontify-now (via jit-lock-function on the hook).
        // The hook functions receive the buffer position and fontify the
        // surrounding region, setting font-lock-face text properties.
        let expr_str = format!("(run-hook-with-args 'fontification-functions {})", from);

        match neovm_core::emacs_core::parse_forms(&expr_str) {
            Ok(forms) => {
                for form in &forms {
                    if let Err(e) = evaluator.eval_expr(form) {
                        tracing::debug!("ensure_fontified_rust: fontification error: {:?}", e);
                        // Non-fatal: continue without fontification
                        break;
                    }
                }
            }
            Err(e) => {
                tracing::debug!("ensure_fontified_rust: parse error: {}", e);
            }
        }
    }

    /// Apply face data from FFI to the FrameGlyphBuffer's current face state.
    pub(crate) unsafe fn apply_face(
        &self,
        face: &FaceDataFFI,
        frame: EmacsFrame,
        frame_glyphs: &mut FrameGlyphBuffer,
    ) {
        let fg = Color::from_pixel(face.fg);
        let bg = Color::from_pixel(face.bg);
        let font_weight = face.font_weight as u16;
        let italic = face.italic != 0;
        let overstrike = face.overstrike != 0;

        // Get font family string from C pointer
        let font_family = if !face.font_family.is_null() {
            CStr::from_ptr(face.font_family)
                .to_str()
                .unwrap_or("monospace")
        } else {
            "monospace"
        };

        // Get font file path from C pointer (absolute path from Fontconfig)
        let font_file_path = if !face.font_file_path.is_null() {
            CStr::from_ptr(face.font_file_path)
                .to_str()
                .ok()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        } else {
            None
        };

        let underline_color = if face.underline_style > 0 {
            Some(Color::from_pixel(face.underline_color))
        } else {
            None
        };

        let strike_color = if face.strike_through > 0 {
            Some(Color::from_pixel(face.strike_through_color))
        } else {
            None
        };

        let overline_color = if face.overline > 0 {
            Some(Color::from_pixel(face.overline_color))
        } else {
            None
        };

        frame_glyphs.set_face_with_font(
            face.face_id,
            fg,
            Some(bg),
            font_family,
            font_weight,
            italic,
            face.font_size as f32,
            face.underline_style as u8,
            underline_color,
            face.strike_through as u8,
            strike_color,
            face.overline as u8,
            overline_color,
            overstrike,
        );

        // Build complete Face for this face_id so the render thread gets
        // all attributes (box, underline, etc.) in one shot per frame,
        // eliminating stale-cache bugs when Emacs reuses face IDs.
        let mut attrs = FaceAttributes::empty();
        if font_weight >= 700 {
            attrs |= FaceAttributes::BOLD;
        }
        if italic {
            attrs |= FaceAttributes::ITALIC;
        }
        if face.underline_style > 0 {
            attrs |= FaceAttributes::UNDERLINE;
        }
        if face.strike_through > 0 {
            attrs |= FaceAttributes::STRIKE_THROUGH;
        }
        if face.overline > 0 {
            attrs |= FaceAttributes::OVERLINE;
        }
        if face.box_type > 0 {
            attrs |= FaceAttributes::BOX;
        }

        frame_glyphs.faces.insert(
            face.face_id,
            Face {
                id: face.face_id,
                foreground: fg,
                background: bg,
                underline_color,
                overline_color,
                strike_through_color: strike_color,
                box_color: if face.box_type > 0 {
                    Some(Color::from_pixel(face.box_color))
                } else {
                    None
                },
                font_family: font_family.to_string(),
                font_size: face.font_size as f32,
                font_weight,
                attributes: attrs,
                underline_style: match face.underline_style {
                    1 => UnderlineStyle::Line,
                    2 => UnderlineStyle::Wave,
                    3 => UnderlineStyle::Double,
                    4 => UnderlineStyle::Dotted,
                    5 => UnderlineStyle::Dashed,
                    _ => UnderlineStyle::None,
                },
                box_type: if face.box_type == 1 {
                    BoxType::Line
                } else {
                    BoxType::None
                },
                box_line_width: face.box_line_width,
                box_corner_radius: face.box_corner_radius,
                box_border_style: face.box_border_style as u32,
                box_border_speed: face.box_border_speed as f32 / 100.0,
                box_color2: if face.box_color2 != 0 {
                    Some(Color::from_pixel(face.box_color2))
                } else {
                    None
                },
                font_file_path: font_file_path,
                font_ascent: face.font_ascent as i32,
                font_descent: face.font_descent,
                underline_position: face.underline_position.max(1),
                underline_thickness: face.underline_thickness.max(1),
            },
        );

        let _ = frame;
    }

    /// Apply a backend-neutral status-line face to the glyph buffer.
    pub(crate) unsafe fn apply_status_line_face(
        &self,
        face: &StatusLineFace,
        frame: Option<EmacsFrame>,
        frame_glyphs: &mut FrameGlyphBuffer,
    ) {
        frame_glyphs.set_face_with_font(
            face.face_id,
            face.foreground,
            Some(face.background),
            &face.font_family,
            face.font_weight,
            face.italic,
            face.font_size,
            face.underline_style,
            face.underline_color,
            if face.strike_through { 1 } else { 0 },
            face.strike_through_color,
            if face.overline { 1 } else { 0 },
            face.overline_color,
            face.overstrike,
        );
        frame_glyphs.faces.insert(face.face_id, face.render_face());

        let _ = frame;
    }

    /// Add a stretch glyph, automatically using stipple if the given face has one.
    pub(crate) fn add_stretch_for_face(
        face: &FaceDataFFI,
        frame_glyphs: &mut FrameGlyphBuffer,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        bg: Color,
        face_id: u32,
        is_overlay: bool,
    ) {
        if face.stipple > 0 {
            let fg = Color::from_pixel(face.fg);
            frame_glyphs.add_stretch_stipple(
                x,
                y,
                width,
                height,
                bg,
                fg,
                face_id,
                is_overlay,
                face.stipple,
            );
        } else {
            frame_glyphs.add_stretch(x, y, width, height, bg, face_id, is_overlay);
        }
    }

    /// Add a stretch glyph for a backend-neutral status-line face.
    pub(crate) fn add_stretch_for_status_line_face(
        face: &StatusLineFace,
        frame_glyphs: &mut FrameGlyphBuffer,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        bg: Color,
        face_id: u32,
        is_overlay: bool,
    ) {
        if face.stipple > 0 {
            frame_glyphs.add_stretch_stipple(
                x,
                y,
                width,
                height,
                bg,
                face.foreground,
                face_id,
                is_overlay,
                face.stipple,
            );
        } else {
            frame_glyphs.add_stretch(x, y, width, height, bg, face_id, is_overlay);
        }
    }

    /// Resolve the character width used by the Rust-native status-line path.
    pub(crate) fn status_line_char_width(
        &mut self,
        face: &StatusLineFace,
        fallback_char_width: f32,
    ) -> f32 {
        if face.font_char_width > 0.0 {
            return face.font_char_width;
        }
        if let Some(ref mut svc) = self.font_metrics {
            let metrics = svc.font_metrics(
                &face.font_family,
                face.font_weight,
                face.italic,
                face.font_size,
            );
            return metrics.char_width;
        }
        fallback_char_width
    }

    /// Measure the advance of a status-line glyph using the backend requested by the spec.
    pub(crate) unsafe fn status_line_advance(
        &mut self,
        advance_mode: &StatusLineAdvanceMode,
        face: &StatusLineFace,
        fallback_char_width: f32,
        ch: char,
    ) -> f32 {
        match advance_mode {
            StatusLineAdvanceMode::Fixed => fallback_char_width,
            StatusLineAdvanceMode::Measured { window } => char_advance(
                &mut self.ascii_width_cache,
                &mut self.font_metrics,
                ch,
                if is_wide_char(ch) { 2 } else { 1 },
                fallback_char_width,
                face.face_id,
                face.font_size.round() as i32,
                face.font_char_width,
                *window,
                &face.font_family,
                face.font_weight,
                face.italic,
            ),
        }
    }

    /// Render the frame-level tab-bar via the status-line pipeline.
    unsafe fn render_frame_tab_bar(
        &mut self,
        frame: EmacsFrame,
        frame_params: &FrameParams,
        frame_glyphs: &mut FrameGlyphBuffer,
        tab_bar_height: f32,
    ) {
        // Tab-bar is positioned at y=0 (topmost, no menu bar in Neomacs).
        let x = 0.0;
        let y = 0.0;
        let width = frame_params.width;
        // Use the tab-bar window pointer as a synthetic window_id for draw context.
        let window_id = frame as i64;

        let char_w = frame_params.char_width;
        let ascent = frame_params.char_height * 0.8; // approximate ascent

        if let Some(spec) = self.build_ffi_tab_bar_spec(
            x,
            y,
            width,
            tab_bar_height,
            window_id,
            char_w,
            ascent,
            frame,
        ) {
            self.render_status_line_spec(&spec, Some(frame), frame_glyphs);
        }
    }

    /// Layout a single window's buffer content.
    ///
    /// Phase 1+2: Monospace layout with per-character face resolution.
    /// - Fixed-width characters on a grid
    /// - Per-character face colors (syntax highlighting)
    /// - Tab expansion
    /// - Line wrapping or truncation
    /// - Cursor positioning
    unsafe fn layout_window(
        &mut self,
        params: &WindowParams,
        wp: &WindowParamsFFI,
        frame: EmacsFrame,
        frame_glyphs: &mut FrameGlyphBuffer,
    ) {
        let buffer = wp.buffer_ptr;
        let window = wp.window_ptr;
        if buffer.is_null() || window.is_null() {
            tracing::debug!(
                "  layout_window: EARLY RETURN — null buffer={:?} or window={:?}",
                buffer,
                window
            );
            return;
        }

        // Reset resolved family cache for this window (face IDs may map
        // to different fonts in different windows due to text-scale-adjust).
        self.resolved_family_face_id = u32::MAX;

        // Calculate available text area
        let text_x = params.text_bounds.x;
        let text_y = params.text_bounds.y + params.header_line_height + params.tab_line_height;
        let text_width = params.text_bounds.width;
        let text_height = params.text_bounds.height
            - params.header_line_height
            - params.tab_line_height
            - params.mode_line_height;

        // Authoritative draw context for this window's content rows.
        frame_glyphs.set_draw_context(
            params.window_id,
            if params.is_minibuffer {
                GlyphRowRole::Minibuffer
            } else {
                GlyphRowRole::Text
            },
            Some(Rect::new(text_x, text_y, text_width, text_height.max(0.0))),
        );

        // Apply vertical scroll: shift content up by vscroll pixels.
        // In Emacs, w->vscroll is a Y offset, always <= 0 (negative = up):
        //   set-window-vscroll(100) → w->vscroll = -100
        // Negate to get the positive pixel shift, then reduce text_height.
        // When shift >= text_height the window renders empty
        // (used by vertico-posframe to hide the minibuffer).
        let vscroll = (-params.vscroll).max(0) as f32;
        let text_height = (text_height - vscroll).max(0.0);

        // Guard against zero/negative dimensions from FFI
        let char_w = if params.char_width > 0.0 {
            params.char_width
        } else {
            8.0
        };
        let char_h = if params.char_height > 0.0 {
            params.char_height + params.extra_line_spacing
        } else {
            16.0
        };
        let ascent = if params.font_ascent > 0.0 {
            params.font_ascent
        } else {
            12.0
        };

        // Fringe dimensions (use actual widths from window params)
        let left_fringe_width = params.left_fringe_width;
        let left_fringe_x = params.text_bounds.x - left_fringe_width;
        let right_fringe_x = params.text_bounds.x + params.text_bounds.width;
        let right_fringe_width = params.right_fringe_width;

        // Check line number configuration
        let mut lnum_config = LineNumberConfigFFI::default();
        let lnum_enabled = neomacs_layout_line_number_config(
            window,
            buffer,
            params.buffer_size,
            (text_height / char_h).floor() as i32,
            &mut lnum_config,
        ) == 0
            && lnum_config.mode > 0;

        let lnum_cols = if lnum_enabled { lnum_config.width } else { 0 };
        let lnum_pixel_width = lnum_cols as f32 * char_w;

        // How many columns and rows fit (accounting for line numbers)
        let cols = ((text_width - lnum_pixel_width) / char_w).floor() as i32;
        let max_rows = (text_height / char_h).floor() as i32;

        // The minibuffer must always render at least 1 row.  Its pixel
        // height may be fractionally smaller than char_h (e.g. 24px vs
        // 24.15 with line-spacing) causing floor() to yield 0.
        // Exception: when vscroll is active, don't force 1 row — vscroll
        // is used (e.g. by vertico-posframe) to intentionally hide content.
        let max_rows =
            if params.is_minibuffer && max_rows <= 0 && text_height > 0.0 && vscroll == 0.0 {
                1
            } else {
                max_rows
            };

        if cols <= 0 || max_rows <= 0 {
            tracing::debug!(
                "  layout_window id={}: skip — cols={} max_rows={}",
                params.window_id,
                cols,
                max_rows
            );
            return;
        }

        // Effective text start X (shifted right for line numbers)
        let content_x = text_x + lnum_pixel_width;

        // --- Scroll adjustment ---
        let window_start =
            if params.point > 0 && params.point < params.window_start && !params.is_minibuffer {
                // Backward scroll: put point near top (1/4 down)
                let lines_above = (max_rows / 4).clamp(2, 10);
                let new_start = neomacs_layout_adjust_window_start(
                    wp.window_ptr,
                    wp.buffer_ptr,
                    params.point,
                    lines_above,
                );
                tracing::debug!(
                    "  scroll backward: point={} was before start={}, new start={}",
                    params.point,
                    params.window_start,
                    new_start
                );
                new_start
            } else if params.point > 0
                && params.window_end > 0
                && params.point > params.window_end
                && !params.is_minibuffer
            {
                // Forward scroll: put point near bottom (3/4 down)
                let lines_above = if max_rows <= 2 {
                    1
                } else {
                    (max_rows * 3 / 4).clamp(2, max_rows - 1)
                };
                let new_start = neomacs_layout_adjust_window_start(
                    wp.window_ptr,
                    wp.buffer_ptr,
                    params.point,
                    lines_above,
                );
                tracing::debug!(
                    "  scroll forward: point={} was past end={}, new start={}",
                    params.point,
                    params.window_end,
                    new_start
                );
                new_start
            } else {
                params.window_start
            };

        // Trigger fontification (jit-lock) for the visible region so that
        // face text properties are set before we read them.
        let read_chars =
            (params.buffer_size - window_start + 1).min(cols as i64 * max_rows as i64 * 2);
        let fontify_end = (window_start + read_chars).min(params.buffer_size);
        neomacs_layout_ensure_fontified(buffer, window_start, fontify_end);

        // Read buffer text directly from gap buffer (Phase 3: eliminates
        // per-character FFI overhead from the old neomacs_layout_buffer_text).
        let bytes_read = if read_chars <= 0 {
            0i64
        } else {
            let text_end = (window_start + read_chars).min(params.buffer_size);
            let byte_from = neomacs_buf_charpos_to_bytepos(buffer, window_start);
            let byte_to = neomacs_buf_charpos_to_bytepos(buffer, text_end);
            super::emacs_types::gap_buffer_copy_text(
                buffer as *const std::ffi::c_void,
                byte_from as isize,
                byte_to as isize,
                &mut self.text_buf,
            );
            self.text_buf.len() as i64
        };

        let text = if bytes_read > 0 {
            &self.text_buf[..bytes_read as usize]
        } else {
            &[]
        };

        tracing::debug!(
            "  layout_window id={}: text_y={:.1} text_h={:.1} char_h={:.1} max_rows={} bytes_read={} bufsz={} is_mini={}",
            params.window_id,
            text_y,
            text_height,
            char_h,
            max_rows,
            bytes_read,
            params.buffer_size,
            params.is_minibuffer
        );

        // Default face colors (fallback)
        let default_fg = Color::from_pixel(params.default_fg);
        let default_bg = Color::from_pixel(params.default_bg);

        // Set initial default face
        frame_glyphs.set_face(
            0, // DEFAULT_FACE_ID
            default_fg,
            Some(default_bg),
            400,
            false,
            0,
            None,
            0,
            None,
            0,
            None,
        );

        // Face resolution state: we only call face_at_pos when charpos >= next_face_check
        let mut current_face_id: i32 = -1; // force first lookup
        let mut next_face_check: i64 = 0;
        let mut face_fg = default_fg;
        let mut face_bg = default_bg;

        // Invisible text state: next charpos where we need to re-check
        let mut next_invis_check: i64 = window_start;

        // Display text property state
        let mut next_display_check: i64 = window_start;
        let mut display_prop = DisplayPropFFI::default();
        let mut display_str_buf = [0u8; 1024];

        // Overlay string buffers (4096 to handle fido-vertical-mode completions)
        let mut overlay_before_buf = [0u8; 4096];
        let mut overlay_after_buf = [0u8; 4096];
        let mut overlay_before_len: i32;
        let mut overlay_after_len: i32;
        let mut overlay_after_face: FaceDataFFI;
        let mut overlay_before_nruns: i32;
        let mut overlay_after_nruns: i32;
        let mut overlay_before_naligns: i32;
        let mut overlay_after_naligns: i32;

        // Line number state
        let mut current_line: i64 = if lnum_enabled {
            neomacs_layout_count_line_number(buffer, window_start, lnum_config.widen)
        } else {
            1
        };
        let point_line: i64 = if lnum_enabled && lnum_config.mode >= 2 {
            neomacs_layout_count_line_number(buffer, params.point, lnum_config.widen)
        } else {
            0
        };
        let mut lnum_face = FaceDataFFI::default();
        let mut need_line_number = lnum_enabled; // render on first row

        // Horizontal scroll: skip first hscroll columns
        let hscroll = if params.truncate_lines {
            params.hscroll.max(0)
        } else {
            0
        };
        // Reserve 1 column for truncation indicator when needed
        let show_left_trunc = hscroll > 0;

        // Available pixel width for text content (excluding line numbers)
        let avail_width = text_width - lnum_pixel_width;

        // Walk through text, placing characters on the grid
        let mut col = 0i32; // column counter (for tab stops, cursor feedback)
        let mut x_offset: f32 = 0.0; // pixel offset from content_x
        let mut row = 0i32;
        let mut charpos = window_start;
        let mut cursor_placed = false;
        let mut cursor_col = 0i32;
        let mut cursor_x: f32 = 0.0; // pixel X of cursor
        let mut cursor_row = 0i32;
        let mut window_end_charpos = window_start;
        let mut byte_idx = 0usize;
        // hscroll state: how many columns to skip on each line
        let mut hscroll_remaining = hscroll;
        // Track current face's space width and line metrics
        let mut face_space_w = char_w;
        let mut face_h: f32 = char_h; // current face's line height
        let mut face_ascent: f32 = ascent; // current face's font ascent

        // Fringe indicator tracking:
        // row_continued[row] = true if row wraps to next line (show \ in right fringe)
        // row_continuation[row] = true if row is a continuation from prev (show \ in left fringe)
        let mut row_continued = vec![false; max_rows as usize];
        let mut row_continuation = vec![false; max_rows as usize];
        let mut row_truncated = vec![false; max_rows as usize];
        // Per-row user fringe bitmaps from display properties
        // (bitmap_id, fg_color, bg_color): 0=none
        let mut row_left_fringe: Vec<(i32, u32, u32)> = vec![(0, 0, 0); max_rows as usize];
        let mut row_right_fringe: Vec<(i32, u32, u32)> = vec![(0, 0, 0); max_rows as usize];

        // Per-row Y positions — supports variable row heights from
        // line-height / line-spacing text properties.
        let row_capacity = (max_rows + 2) as usize;
        let mut row_y: Vec<f32> = (0..row_capacity)
            .map(|r| text_y + r as f32 * char_h)
            .collect();
        let mut row_extra_y: f32 = 0.0; // cumulative extra height from previous rows
        let mut row_max_height: f32 = char_h; // max glyph height on current row
        let mut row_max_ascent: f32 = ascent; // max ascent on current row

        // Trailing whitespace tracking
        let trailing_ws_bg = if params.show_trailing_whitespace {
            Some(Color::from_pixel(params.trailing_ws_bg))
        } else {
            None
        };
        let mut trailing_ws_start_col: i32 = -1; // -1 = no trailing ws
        let mut trailing_ws_start_x: f32 = 0.0; // pixel position of trailing ws start
        let mut trailing_ws_row: i32 = 0;

        // Word-wrap tracking: position after last breakable whitespace
        let mut _wrap_break_col = 0i32;
        let mut wrap_break_x: f32 = 0.0; // pixel position of wrap break
        let mut wrap_break_byte_idx = 0usize;
        let mut wrap_break_charpos = window_start;
        let mut wrap_break_glyph_count = 0usize;
        let mut wrap_has_break = false;

        // Line/wrap prefix tracking: 0=none, 1=line_prefix, 2=wrap_prefix
        let mut need_prefix: u8 = if !params.line_prefix.is_empty() { 1 } else { 0 };

        // Raise display property: Y offset applied to glyphs
        let mut raise_y_offset: f32 = 0.0;
        let mut raise_end: i64 = 0;
        // Height display property: font scale factor
        let mut height_scale: f32 = 0.0; // 0.0 = no scaling
        let mut height_end: i64 = 0;

        // Margin rendering: check at start of each visual line
        let has_margins = params.left_margin_width > 0.0 || params.right_margin_width > 0.0;
        let mut need_margin_check = has_margins;
        let mut margin_covers_to: i64 = 0;

        // Box face tracking: track active box regions for renderer's span detection
        let mut box_active = false;
        let mut _box_start_x: f32 = 0.0;
        let mut _box_row: i32 = 0;

        // Track the last face with :extend on the current row.
        // Used to fill end-of-line even when the newline itself lacks :extend.
        // (Matches upstream Emacs extend_face_to_end_of_line behavior.)
        let mut row_extend_bg: Option<(Color, u32)> = None; // (bg_color, face_id)
        let mut row_extend_row: i32 = -1; // which row the extend bg belongs to

        // Pixel Y limit: stop rendering when rows exceed the text area,
        // which can happen with variable-height faces pushing rows down.
        let text_y_limit = text_y + text_height;

        // Hit-test data for this window
        let mut hit_rows: Vec<HitRow> = Vec::new();
        let mut hit_row_charpos_start: i64 = window_start;

        // Ligature run accumulation
        let ligatures = self.ligatures_enabled;
        self.run_buf.clear();

        // Bidi reordering: track where each row's glyphs start in frame_glyphs.glyphs
        let mut row_glyph_start: usize = frame_glyphs.glyphs.len();
        // Track script transitions so multibyte chars can force per-char
        // face resolution (mirrors xdisp FACE_FOR_CHAR behavior).
        let mut prev_was_non_ascii = false;

        // Place cursor at the current visual position. Call this only at the
        // final draw position for the current buffer char (after wrap decisions).
        macro_rules! place_cursor_here {
            ($cursor_byte_idx:expr, $cursor_col:expr) => {{
                // Flush ligature run before cursor to split run at cursor position
                flush_run(&self.run_buf, frame_glyphs, ligatures);
                self.run_buf.clear();
                cursor_col = $cursor_col;
                cursor_x = x_offset;
                cursor_row = row;
                let cursor_px = content_x + x_offset;
                let cursor_y = row_y[row as usize];

                // Use face-specific dimensions so cursor matches variable-height faces
                let cursor_face_w = if self.face_data.font_char_width > 0.0 {
                    self.face_data.font_char_width
                } else {
                    char_w
                };

                let cursor_style = cursor_style_for_window(params);

                if let Some(style) = cursor_style {
                    let fallback_cursor_w = cursor_width_for_style(
                        style,
                        text,
                        $cursor_byte_idx,
                        $cursor_col,
                        params,
                        cursor_face_w,
                    );
                    let cursor_w = if matches!(style, CursorStyle::Bar(_)) {
                        fallback_cursor_w
                    } else {
                        // Match Emacs cursor geometry: use the actual width of the
                        // display element at point (glyph->pixel_width), not just
                        // columns * nominal face width.
                        let face_id = self.face_data.face_id;
                        if face_id != self.resolved_family_face_id {
                            let font_family = if !self.face_data.font_family.is_null() {
                                CStr::from_ptr(self.face_data.font_family)
                                    .to_str()
                                    .unwrap_or("")
                            } else {
                                ""
                            };
                            let font_file_path_str = if !self.face_data.font_file_path.is_null() {
                                CStr::from_ptr(self.face_data.font_file_path)
                                    .to_str()
                                    .ok()
                                    .filter(|s| !s.is_empty())
                            } else {
                                None
                            };
                            self.current_resolved_family =
                                if let Some(ref mut svc) = self.font_metrics {
                                    svc.resolve_family(font_family, font_file_path_str)
                                } else {
                                    font_family.to_string()
                                };
                            self.resolved_family_face_id = face_id;
                        }

                        cursor_point_advance(
                            text,
                            $cursor_byte_idx,
                            $cursor_col,
                            params,
                            cursor_face_w,
                            face_space_w,
                            char_w,
                            self.face_data.face_id,
                            self.face_data.font_size,
                            window,
                            &self.current_resolved_family,
                            self.face_data.font_weight as u16,
                            self.face_data.italic != 0,
                            &mut self.ascii_width_cache,
                            &mut self.font_metrics,
                        )
                        .unwrap_or(fallback_cursor_w)
                    }
                    .max(1.0);
                    frame_glyphs.add_cursor(
                        params.window_id as i32,
                        cursor_px,
                        cursor_y,
                        cursor_w,
                        face_h,
                        style,
                        Color::from_pixel(params.cursor_color),
                    );

                    if matches!(style, CursorStyle::FilledBox) {
                        frame_glyphs.set_cursor_inverse(
                            cursor_px,
                            cursor_y,
                            cursor_w,
                            face_h,
                            Color::from_pixel(params.cursor_color),
                            default_bg,
                        );
                    }
                }

                cursor_placed = true;
            }};
        }

        while byte_idx < bytes_read as usize && row < max_rows && row_y[row as usize] < text_y_limit
        {
            // Render line number at the start of each new row
            if need_line_number && lnum_enabled {
                // Determine displayed number based on mode
                let display_num = match lnum_config.mode {
                    2 => {
                        // Relative mode
                        if lnum_config.current_absolute != 0 && current_line == point_line {
                            current_line + lnum_config.offset as i64
                        } else {
                            (current_line - point_line).abs()
                        }
                    }
                    3 => {
                        // Visual mode: relative to point line
                        if lnum_config.current_absolute != 0 && current_line == point_line {
                            current_line + lnum_config.offset as i64
                        } else {
                            (current_line - point_line).abs()
                        }
                    }
                    _ => {
                        // Absolute mode
                        current_line + lnum_config.offset as i64
                    }
                };

                let is_current = if current_line == point_line { 1 } else { 0 };
                neomacs_layout_line_number_face(
                    window,
                    is_current,
                    current_line,
                    lnum_config.major_tick,
                    lnum_config.minor_tick,
                    &mut lnum_face,
                );

                // Apply line number face and render digits
                self.apply_face(&lnum_face, frame, frame_glyphs);
                let lnum_bg = Color::from_pixel(lnum_face.bg);

                // Format the number right-aligned
                let num_str = format!("{}", display_num);
                let num_chars = num_str.len() as i32;
                let padding = (lnum_cols - 1) - num_chars; // -1 for trailing space

                let gy = row_y[row as usize];

                // Leading padding
                if padding > 0 {
                    frame_glyphs.add_stretch(
                        text_x,
                        gy,
                        padding as f32 * char_w,
                        char_h,
                        lnum_bg,
                        lnum_face.face_id,
                        false,
                    );
                }

                // Number digits
                for (i, ch) in num_str.chars().enumerate() {
                    let dx = text_x + (padding.max(0) + i as i32) as f32 * char_w;
                    frame_glyphs.add_char(ch, dx, gy, char_w, char_h, ascent, false);
                }

                // Trailing space
                let space_x = text_x + (lnum_cols - 1) as f32 * char_w;
                frame_glyphs.add_stretch(
                    space_x,
                    gy,
                    char_w,
                    char_h,
                    lnum_bg,
                    lnum_face.face_id,
                    false,
                );

                // Restore text face
                if current_face_id >= 0 {
                    self.apply_face(&self.face_data, frame, frame_glyphs);
                }

                need_line_number = false;
            }

            // Render line-prefix or wrap-prefix at start of visual lines
            if need_prefix > 0 && row < max_rows {
                let prefix_type = if need_prefix == 2 { 1 } else { 0 };
                let mut tp_width: f32 = -1.0;

                // Check text property prefix first (overrides window default)
                neomacs_layout_check_line_prefix(
                    buffer,
                    window,
                    charpos,
                    prefix_type,
                    &mut tp_width,
                );

                if tp_width >= 0.0 {
                    // Text property prefix: render as space
                    let px_w = tp_width * char_w;
                    if px_w > 0.0 && x_offset + px_w <= avail_width {
                        let gx = content_x + x_offset;
                        let gy = row_y[row as usize];
                        frame_glyphs.add_stretch(gx, gy, px_w, char_h, default_bg, 0, false);
                        let prefix_cols = tp_width.ceil() as i32;
                        col += prefix_cols;
                        x_offset += px_w;
                    }
                } else {
                    // Fall back to window-level prefix string
                    let prefix_bytes = if need_prefix == 2 {
                        &params.wrap_prefix[..]
                    } else {
                        &params.line_prefix[..]
                    };

                    if !prefix_bytes.is_empty() {
                        let mut pi = 0usize;
                        while pi < prefix_bytes.len() {
                            let (pch, plen) = decode_utf8(&prefix_bytes[pi..]);
                            pi += plen;
                            if pch == '\n' || pch == '\r' {
                                continue;
                            }

                            let pchar_cols = if is_wide_char(pch) { 2 } else { 1 };
                            let adv = pchar_cols as f32 * char_w;
                            if x_offset + adv > avail_width {
                                break;
                            }

                            let gx = content_x + x_offset;
                            let gy = row_y[row as usize];
                            frame_glyphs.add_char(pch, gx, gy, adv, char_h, ascent, false);
                            col += pchar_cols;
                            x_offset += adv;
                        }
                    }
                }
                need_prefix = 0;
            }

            // Render margin content at the start of each visual line
            if need_margin_check
                && (params.left_margin_width > 0.0 || params.right_margin_width > 0.0)
            {
                // Skip margin check if covers_to tells us this position
                // is still within the same margin display property.
                if margin_covers_to > 0 && charpos < margin_covers_to {
                    need_margin_check = false;
                } else {
                    need_margin_check = false;
                    let mut left_margin_buf = [0u8; 256];
                    let mut right_margin_buf = [0u8; 256];
                    let mut left_len: c_int = 0;
                    let mut right_len: c_int = 0;
                    let mut left_fg: u32 = 0;
                    let mut left_bg: u32 = 0;
                    let mut right_fg: u32 = 0;
                    let mut right_bg: u32 = 0;
                    let mut left_image_gpu_id: c_int = 0;
                    let mut left_image_w: c_int = 0;
                    let mut left_image_h: c_int = 0;
                    let mut right_image_gpu_id: c_int = 0;
                    let mut right_image_w: c_int = 0;
                    let mut right_image_h: c_int = 0;
                    let mut covers_to: i64 = 0;
                    neomacs_layout_margin_strings_at(
                        buffer,
                        window,
                        frame,
                        charpos,
                        left_margin_buf.as_mut_ptr(),
                        256,
                        &mut left_len,
                        right_margin_buf.as_mut_ptr(),
                        256,
                        &mut right_len,
                        &mut left_fg,
                        &mut left_bg,
                        &mut right_fg,
                        &mut right_bg,
                        &mut left_image_gpu_id,
                        &mut left_image_w,
                        &mut left_image_h,
                        &mut right_image_gpu_id,
                        &mut right_image_w,
                        &mut right_image_h,
                        &mut covers_to,
                    );

                    if covers_to > 0 {
                        margin_covers_to = covers_to;
                    }

                    // Render left margin: image or text
                    // Default layout (fringes inside margins):
                    // | LEFT_MARGIN | LEFT_FRINGE | TEXT_AREA |
                    if left_image_gpu_id != 0 && params.left_margin_width > 0.0 {
                        let margin_x = text_x - left_fringe_width - params.left_margin_width;
                        let gy = row_y[row as usize];
                        frame_glyphs.add_image(
                            left_image_gpu_id as u32,
                            margin_x,
                            gy,
                            left_image_w as f32,
                            left_image_h as f32,
                        );
                    } else if left_len > 0 && params.left_margin_width > 0.0 {
                        let margin_x = text_x - left_fringe_width - params.left_margin_width;
                        let gy = row_y[row as usize];
                        let margin_cols = (params.left_margin_width / char_w).floor() as i32;

                        // Save and apply face colors if provided
                        let saved_fg = frame_glyphs.get_current_fg();
                        let saved_bg = frame_glyphs.get_current_bg();
                        if left_fg != 0 || left_bg != 0 {
                            let fg = if left_fg != 0 {
                                Color::from_pixel(left_fg)
                            } else {
                                saved_fg
                            };
                            let bg = if left_bg != 0 {
                                Some(Color::from_pixel(left_bg))
                            } else {
                                saved_bg
                            };
                            frame_glyphs.set_colors(fg, bg);
                        }

                        let s =
                            std::str::from_utf8_unchecked(&left_margin_buf[..left_len as usize]);
                        let mut mcol = 0i32;
                        for mch in s.chars() {
                            if mcol >= margin_cols {
                                break;
                            }
                            let gx = margin_x + mcol as f32 * char_w;
                            frame_glyphs.add_char(mch, gx, gy, char_w, char_h, ascent, false);
                            mcol += 1;
                        }

                        // Restore face colors
                        if left_fg != 0 || left_bg != 0 {
                            frame_glyphs.set_colors(saved_fg, saved_bg);
                        }
                    }

                    // Render right margin: image or text
                    // Default layout (fringes inside margins):
                    // | TEXT_AREA | RIGHT_FRINGE | RIGHT_MARGIN |
                    if right_image_gpu_id != 0 && params.right_margin_width > 0.0 {
                        let margin_x = text_x + text_width + right_fringe_width;
                        let gy = row_y[row as usize];
                        frame_glyphs.add_image(
                            right_image_gpu_id as u32,
                            margin_x,
                            gy,
                            right_image_w as f32,
                            right_image_h as f32,
                        );
                    } else if right_len > 0 && params.right_margin_width > 0.0 {
                        let margin_x = text_x + text_width + right_fringe_width;
                        let gy = row_y[row as usize];
                        let margin_cols = (params.right_margin_width / char_w).floor() as i32;

                        // Save and apply face colors if provided
                        let saved_fg = frame_glyphs.get_current_fg();
                        let saved_bg = frame_glyphs.get_current_bg();
                        if right_fg != 0 || right_bg != 0 {
                            let fg = if right_fg != 0 {
                                Color::from_pixel(right_fg)
                            } else {
                                saved_fg
                            };
                            let bg = if right_bg != 0 {
                                Some(Color::from_pixel(right_bg))
                            } else {
                                saved_bg
                            };
                            frame_glyphs.set_colors(fg, bg);
                        }

                        let s =
                            std::str::from_utf8_unchecked(&right_margin_buf[..right_len as usize]);
                        let mut mcol = 0i32;
                        for mch in s.chars() {
                            if mcol >= margin_cols {
                                break;
                            }
                            let gx = margin_x + mcol as f32 * char_w;
                            frame_glyphs.add_char(mch, gx, gy, char_w, char_h, ascent, false);
                            mcol += 1;
                        }

                        // Restore face colors
                        if right_fg != 0 || right_bg != 0 {
                            frame_glyphs.set_colors(saved_fg, saved_bg);
                        }
                    }
                }
            }

            // Handle hscroll: show $ indicator and skip columns
            if hscroll_remaining > 0 {
                // Skip characters consumed by hscroll
                let (ch, ch_len) = decode_utf8(&text[byte_idx..]);
                byte_idx += ch_len;
                charpos += 1;

                if ch == '\n' {
                    // Newline within hscroll region: new line
                    reorder_row_bidi(
                        frame_glyphs,
                        row_glyph_start,
                        frame_glyphs.glyphs.len(),
                        content_x,
                    );
                    col = 0;
                    x_offset = 0.0;
                    row += 1;
                    row_glyph_start = frame_glyphs.glyphs.len();
                    current_line += 1;
                    need_line_number = lnum_enabled;
                    need_margin_check = has_margins;
                    hscroll_remaining = hscroll; // reset for next line
                    wrap_has_break = false;
                } else {
                    let ch_cols = if ch == '\t' {
                        let tab_w = params.tab_width.max(1);
                        ((hscroll - hscroll_remaining) / tab_w + 1) * tab_w
                            - (hscroll - hscroll_remaining)
                    } else if is_wide_char(ch) {
                        2
                    } else {
                        1
                    };
                    hscroll_remaining -= ch_cols.min(hscroll_remaining);

                    // When hscroll is done, show $ at left edge
                    if hscroll_remaining <= 0 && show_left_trunc {
                        let gy = row_y[row as usize];
                        frame_glyphs.add_char('$', content_x, gy, char_w, char_h, ascent, false);
                        col = 1; // $ takes 1 column
                        x_offset = char_w;
                    }
                }
                window_end_charpos = charpos;
                continue;
            }

            // Check for invisible text at property change boundaries
            if charpos >= next_invis_check {
                let mut next_visible: i64 = 0;
                let invis =
                    neomacs_layout_check_invisible(buffer, window, charpos, &mut next_visible);

                if tracing::enabled!(tracing::Level::DEBUG)
                    && (charpos < 20 || (charpos % 500 == 0))
                {
                    let ch_preview = if byte_idx < text.len() {
                        let (ch, _) = decode_utf8(&text[byte_idx..]);
                        ch
                    } else {
                        '?'
                    };
                    tracing::debug!(
                        "  invis_check: charpos={} invis={} next_visible={} ch={:?} byte_idx={} row={}",
                        charpos,
                        invis,
                        next_visible,
                        ch_preview,
                        byte_idx,
                        row
                    );
                }

                if invis > 0 {
                    // Flush ligature run before invisible text skip
                    flush_run(&self.run_buf, frame_glyphs, ligatures);
                    self.run_buf.clear();

                    // Even though the buffer text is invisible, overlays
                    // at this position may have before-string/after-string
                    // content that should be rendered (e.g., Doom dashboard).
                    // Walk through the invisible region checking for overlay
                    // strings at each property boundary.
                    {
                        let mut ipos = charpos;
                        while ipos < next_visible && row < max_rows {
                            let mut ib_len: i32 = 0;
                            let mut ia_len: i32 = 0;
                            let mut ib_face = FaceDataFFI::default();
                            let mut ia_face = FaceDataFFI::default();
                            let mut ib_nruns: i32 = 0;
                            let mut ia_nruns: i32 = 0;
                            let mut i_lf_bmp: i32 = 0;
                            let mut i_lf_fg: u32 = 0;
                            let mut i_lf_bg: u32 = 0;
                            let mut i_rf_bmp: i32 = 0;
                            let mut i_rf_fg: u32 = 0;
                            let mut i_rf_bg: u32 = 0;
                            let mut ib_naligns: i32 = 0;
                            let mut ia_naligns: i32 = 0;
                            neomacs_layout_overlay_strings_at(
                                buffer,
                                window,
                                ipos,
                                overlay_before_buf.as_mut_ptr(),
                                overlay_before_buf.len() as i32,
                                &mut ib_len,
                                overlay_after_buf.as_mut_ptr(),
                                overlay_after_buf.len() as i32,
                                &mut ia_len,
                                &mut ib_face,
                                &mut ia_face,
                                &mut ib_nruns,
                                &mut ia_nruns,
                                &mut i_lf_bmp,
                                &mut i_lf_fg,
                                &mut i_lf_bg,
                                &mut i_rf_bmp,
                                &mut i_rf_fg,
                                &mut i_rf_bg,
                                &mut ib_naligns,
                                &mut ia_naligns,
                            );

                            // Store fringe bitmaps from overlay display properties
                            let r = row as usize;
                            if i_lf_bmp > 0 && r < row_left_fringe.len() {
                                row_left_fringe[r] = (i_lf_bmp, i_lf_fg, i_lf_bg);
                            }
                            if i_rf_bmp > 0 && r < row_right_fringe.len() {
                                row_right_fringe[r] = (i_rf_bmp, i_rf_fg, i_rf_bg);
                            }

                            // Render overlay before-string
                            if ib_len > 0 {
                                let ib_has_runs = ib_nruns > 0;
                                let ib_face_runs = if ib_has_runs {
                                    parse_overlay_face_runs(
                                        &overlay_before_buf,
                                        ib_len as usize,
                                        ib_nruns,
                                    )
                                } else {
                                    Vec::new()
                                };
                                let ib_align_entries = if ib_naligns > 0 {
                                    parse_overlay_align_entries(
                                        &overlay_before_buf,
                                        ib_len as usize,
                                        ib_nruns,
                                        ib_naligns,
                                    )
                                } else {
                                    Vec::new()
                                };
                                let mut ib_current_align = 0usize;

                                if !ib_has_runs {
                                    if ib_face.face_id != 0 {
                                        self.apply_face(&ib_face, frame, frame_glyphs);
                                    }
                                }

                                let bstr = &overlay_before_buf[..ib_len as usize];
                                let mut bi = 0usize;
                                let mut ib_current_run = 0usize;
                                while bi < bstr.len() && row < max_rows {
                                    if ib_current_align < ib_align_entries.len()
                                        && bi
                                            == ib_align_entries[ib_current_align].byte_offset
                                                as usize
                                    {
                                        let target_x =
                                            ib_align_entries[ib_current_align].align_to_px;
                                        if target_x > x_offset {
                                            let gx = content_x + x_offset;
                                            let gy = row_y[row as usize];
                                            let stretch_w = target_x - x_offset;
                                            let stretch_bg =
                                                overlay_run_bg_at(&ib_face_runs, bi, default_bg);
                                            frame_glyphs.add_stretch(
                                                gx, gy, stretch_w, char_h, stretch_bg, 0, false,
                                            );
                                            col = (ib_align_entries[ib_current_align].align_to_px
                                                / char_w)
                                                .ceil()
                                                as i32;
                                            x_offset = target_x;
                                        }
                                        ib_current_align += 1;
                                        let (_bch, blen) = decode_utf8(&bstr[bi..]);
                                        bi += blen;
                                        continue;
                                    }

                                    if ib_has_runs && ib_current_run < ib_face_runs.len() {
                                        if let Some((ext_bg, true)) =
                                            overlay_run_bg_extend_at(&ib_face_runs, bi)
                                        {
                                            row_extend_bg = Some((ext_bg, 0));
                                            row_extend_row = row as i32;
                                        }
                                        ib_current_run = apply_overlay_face_run(
                                            &ib_face_runs,
                                            bi,
                                            ib_current_run,
                                            frame_glyphs,
                                        );
                                    }

                                    let (bch, blen) = decode_utf8(&bstr[bi..]);
                                    bi += blen;
                                    if bch == '\n' {
                                        let remaining = avail_width - x_offset;
                                        if remaining > 0.0 {
                                            if let Some((ext_bg, _)) = row_extend_bg
                                                .filter(|_| row_extend_row == row as i32)
                                            {
                                                let gx = content_x + x_offset;
                                                let gy = row_y[row as usize];
                                                frame_glyphs.add_stretch(
                                                    gx,
                                                    gy,
                                                    remaining,
                                                    row_max_height,
                                                    ext_bg,
                                                    0,
                                                    false,
                                                );
                                            }
                                        }
                                        reorder_row_bidi(
                                            frame_glyphs,
                                            row_glyph_start,
                                            frame_glyphs.glyphs.len(),
                                            content_x,
                                        );
                                        col = 0;
                                        x_offset = 0.0;
                                        row += 1;
                                        row_glyph_start = frame_glyphs.glyphs.len();
                                        if row >= max_rows {
                                            break;
                                        }
                                        continue;
                                    }
                                    if bch != '\0' {
                                        let gx = content_x + x_offset;
                                        let gy = row_y[row as usize];
                                        frame_glyphs
                                            .add_char(bch, gx, gy, char_w, char_h, ascent, false);
                                        col += 1;
                                        x_offset += char_w;
                                        if x_offset >= avail_width {
                                            reorder_row_bidi(
                                                frame_glyphs,
                                                row_glyph_start,
                                                frame_glyphs.glyphs.len(),
                                                content_x,
                                            );
                                            col = 0;
                                            x_offset = 0.0;
                                            row += 1;
                                            row_glyph_start = frame_glyphs.glyphs.len();
                                        }
                                    }
                                }
                            }

                            // Render overlay after-string
                            if ia_len > 0 {
                                let ia_has_runs = ia_nruns > 0;
                                let ia_face_runs = if ia_has_runs {
                                    parse_overlay_face_runs(
                                        &overlay_after_buf,
                                        ia_len as usize,
                                        ia_nruns,
                                    )
                                } else {
                                    Vec::new()
                                };
                                let ia_align_entries = if ia_naligns > 0 {
                                    parse_overlay_align_entries(
                                        &overlay_after_buf,
                                        ia_len as usize,
                                        ia_nruns,
                                        ia_naligns,
                                    )
                                } else {
                                    Vec::new()
                                };
                                let mut ia_current_align = 0usize;

                                if !ia_has_runs {
                                    if ia_face.face_id != 0 {
                                        self.apply_face(&ia_face, frame, frame_glyphs);
                                    }
                                }

                                let astr = &overlay_after_buf[..ia_len as usize];
                                let mut ai = 0usize;
                                let mut ia_current_run = 0usize;
                                while ai < astr.len() && row < max_rows {
                                    if ia_current_align < ia_align_entries.len()
                                        && ai
                                            == ia_align_entries[ia_current_align].byte_offset
                                                as usize
                                    {
                                        let target_x =
                                            ia_align_entries[ia_current_align].align_to_px;
                                        if target_x > x_offset {
                                            let gx = content_x + x_offset;
                                            let gy = row_y[row as usize];
                                            let stretch_w = target_x - x_offset;
                                            let stretch_bg =
                                                overlay_run_bg_at(&ia_face_runs, ai, default_bg);
                                            frame_glyphs.add_stretch(
                                                gx, gy, stretch_w, char_h, stretch_bg, 0, false,
                                            );
                                            col = (ia_align_entries[ia_current_align].align_to_px
                                                / char_w)
                                                .ceil()
                                                as i32;
                                            x_offset = target_x;
                                        }
                                        ia_current_align += 1;
                                        let (_ach, alen) = decode_utf8(&astr[ai..]);
                                        ai += alen;
                                        continue;
                                    }

                                    if ia_has_runs && ia_current_run < ia_face_runs.len() {
                                        if let Some((ext_bg, true)) =
                                            overlay_run_bg_extend_at(&ia_face_runs, ai)
                                        {
                                            row_extend_bg = Some((ext_bg, 0));
                                            row_extend_row = row as i32;
                                        }
                                        ia_current_run = apply_overlay_face_run(
                                            &ia_face_runs,
                                            ai,
                                            ia_current_run,
                                            frame_glyphs,
                                        );
                                    }

                                    let (ach, alen) = decode_utf8(&astr[ai..]);
                                    ai += alen;
                                    if ach == '\n' {
                                        let remaining = avail_width - x_offset;
                                        if remaining > 0.0 {
                                            if let Some((ext_bg, _)) = row_extend_bg
                                                .filter(|_| row_extend_row == row as i32)
                                            {
                                                let gx = content_x + x_offset;
                                                let gy = row_y[row as usize];
                                                frame_glyphs.add_stretch(
                                                    gx,
                                                    gy,
                                                    remaining,
                                                    row_max_height,
                                                    ext_bg,
                                                    0,
                                                    false,
                                                );
                                            }
                                        }
                                        reorder_row_bidi(
                                            frame_glyphs,
                                            row_glyph_start,
                                            frame_glyphs.glyphs.len(),
                                            content_x,
                                        );
                                        col = 0;
                                        x_offset = 0.0;
                                        row += 1;
                                        row_glyph_start = frame_glyphs.glyphs.len();
                                        if row >= max_rows {
                                            break;
                                        }
                                        continue;
                                    }
                                    if ach != '\0' {
                                        let gx = content_x + x_offset;
                                        let gy = row_y[row as usize];
                                        frame_glyphs
                                            .add_char(ach, gx, gy, char_w, char_h, ascent, false);
                                        col += 1;
                                        x_offset += char_w;
                                        if x_offset >= avail_width {
                                            reorder_row_bidi(
                                                frame_glyphs,
                                                row_glyph_start,
                                                frame_glyphs.glyphs.len(),
                                                content_x,
                                            );
                                            col = 0;
                                            x_offset = 0.0;
                                            row += 1;
                                            row_glyph_start = frame_glyphs.glyphs.len();
                                        }
                                    }
                                }
                            }

                            // Advance to the next overlay boundary within the invisible range.
                            // Use Fnext_single_char_property_change on 'invisible' to find
                            // the next property boundary where a new overlay might start.
                            // If there are no more boundaries, jump to next_visible.
                            let next_boundary = {
                                let mut nb: i64 = 0;
                                // Re-check invisible at ipos+1 to find where property changes
                                neomacs_layout_check_invisible(buffer, window, ipos + 1, &mut nb);
                                nb
                            };
                            if next_boundary > ipos && next_boundary < next_visible {
                                ipos = next_boundary;
                            } else {
                                break;
                            }
                        }
                    }

                    // Skip invisible characters: advance byte_idx
                    // and charpos to next_visible
                    let chars_to_skip = next_visible - charpos;
                    for _ in 0..chars_to_skip {
                        if byte_idx >= bytes_read as usize {
                            break;
                        }
                        let (_, ch_len) = decode_utf8(&text[byte_idx..]);
                        byte_idx += ch_len;
                    }
                    // Show ellipsis for invis==2
                    if invis == 2 && x_offset + 3.0 * char_w <= avail_width && row < max_rows {
                        let gy = row_y[row as usize];
                        for _ in 0..3 {
                            let dx = content_x + x_offset;
                            frame_glyphs.add_char('.', dx, gy, char_w, char_h, ascent, false);
                            col += 1;
                            x_offset += char_w;
                        }
                    }
                    charpos = next_visible;
                    next_invis_check = next_visible;
                    // Force face re-check at new position
                    current_face_id = -1;
                    continue;
                } else {
                    // Visible: next_visible tells us when to re-check
                    next_invis_check = if next_visible > charpos {
                        next_visible
                    } else {
                        charpos + 1
                    };
                }
            }

            // Check for overlay before-string/after-string at this position.
            // Before-strings render at overlay start, after-strings at end.
            {
                overlay_before_len = 0;
                overlay_after_len = 0;
                let mut overlay_before_face = FaceDataFFI::default();
                overlay_after_face = FaceDataFFI::default();
                overlay_before_nruns = 0;
                overlay_after_nruns = 0;
                let mut ovl_left_fringe_bitmap: i32 = 0;
                let mut ovl_left_fringe_fg: u32 = 0;
                let mut ovl_left_fringe_bg: u32 = 0;
                let mut ovl_right_fringe_bitmap: i32 = 0;
                let mut ovl_right_fringe_fg: u32 = 0;
                let mut ovl_right_fringe_bg: u32 = 0;
                overlay_before_naligns = 0;
                overlay_after_naligns = 0;
                neomacs_layout_overlay_strings_at(
                    buffer,
                    window,
                    charpos,
                    overlay_before_buf.as_mut_ptr(),
                    overlay_before_buf.len() as i32,
                    &mut overlay_before_len,
                    overlay_after_buf.as_mut_ptr(),
                    overlay_after_buf.len() as i32,
                    &mut overlay_after_len,
                    &mut overlay_before_face,
                    &mut overlay_after_face,
                    &mut overlay_before_nruns,
                    &mut overlay_after_nruns,
                    &mut ovl_left_fringe_bitmap,
                    &mut ovl_left_fringe_fg,
                    &mut ovl_left_fringe_bg,
                    &mut ovl_right_fringe_bitmap,
                    &mut ovl_right_fringe_fg,
                    &mut ovl_right_fringe_bg,
                    &mut overlay_before_naligns,
                    &mut overlay_after_naligns,
                );

                // Store fringe bitmaps from overlay display properties
                let r = row as usize;
                if ovl_left_fringe_bitmap > 0 && r < row_left_fringe.len() {
                    row_left_fringe[r] = (
                        ovl_left_fringe_bitmap,
                        ovl_left_fringe_fg,
                        ovl_left_fringe_bg,
                    );
                }
                if ovl_right_fringe_bitmap > 0 && r < row_right_fringe.len() {
                    row_right_fringe[r] = (
                        ovl_right_fringe_bitmap,
                        ovl_right_fringe_fg,
                        ovl_right_fringe_bg,
                    );
                }

                // Flush ligature run before overlay strings (only if overlays exist)
                if overlay_before_len > 0 || overlay_after_len > 0 {
                    flush_run(&self.run_buf, frame_glyphs, ligatures);
                    self.run_buf.clear();
                }

                // Render before-string (if any) — insert before buffer text
                if overlay_before_len > 0 {
                    let before_has_runs = overlay_before_nruns > 0;
                    let before_face_runs = if before_has_runs {
                        parse_overlay_face_runs(
                            &overlay_before_buf,
                            overlay_before_len as usize,
                            overlay_before_nruns,
                        )
                    } else {
                        Vec::new()
                    };
                    let before_align_entries = if overlay_before_naligns > 0 {
                        parse_overlay_align_entries(
                            &overlay_before_buf,
                            overlay_before_len as usize,
                            overlay_before_nruns,
                            overlay_before_naligns,
                        )
                    } else {
                        Vec::new()
                    };
                    let mut bcurrent_align = 0usize;

                    // Use per-char face runs, overlay face, or resolve face for position
                    if !before_has_runs {
                        if overlay_before_face.face_id != 0 {
                            self.apply_face(&overlay_before_face, frame, frame_glyphs);
                        } else if charpos >= next_face_check || current_face_id < 0 {
                            let mut next_check: i64 = 0;
                            let fid = neomacs_layout_face_at_pos(
                                window,
                                charpos,
                                &mut self.face_data as *mut FaceDataFFI,
                                &mut next_check,
                            );
                            if fid >= 0 && fid != current_face_id {
                                current_face_id = fid;
                                face_fg = Color::from_pixel(self.face_data.fg);
                                face_bg = Color::from_pixel(self.face_data.bg);
                                self.apply_face(&self.face_data, frame, frame_glyphs);
                                // Track last face with :extend on this row
                                if self.face_data.extend != 0 {
                                    row_extend_bg = Some((face_bg, self.face_data.face_id));
                                    row_extend_row = row as i32;
                                }
                            }
                            next_face_check = if next_check > charpos {
                                next_check
                            } else {
                                charpos + 1
                            };
                        }
                    }

                    let bstr = &overlay_before_buf[..overlay_before_len as usize];
                    let mut bi = 0usize;
                    let mut bcurrent_run = 0usize;
                    while bi < bstr.len() && row < max_rows {
                        // Check for align-to entry at this byte offset
                        if bcurrent_align < before_align_entries.len()
                            && bi == before_align_entries[bcurrent_align].byte_offset as usize
                        {
                            let target_x = before_align_entries[bcurrent_align].align_to_px;
                            if target_x > x_offset {
                                let gx = content_x + x_offset;
                                let gy = row_y[row as usize];
                                let stretch_w = target_x - x_offset;
                                // Use overlay face run's bg for the stretch, not the buffer
                                // position's face_bg.  This matches official Emacs where
                                // overlay string stretches use face_for_overlay_string (base=
                                // DEFAULT_FACE_ID) merged with the string's text property face,
                                // NOT the buffer's overlay face (e.g. minibuffer-prompt).
                                let stretch_bg =
                                    overlay_run_bg_at(&before_face_runs, bi, default_bg);
                                frame_glyphs
                                    .add_stretch(gx, gy, stretch_w, char_h, stretch_bg, 0, false);
                                col = (before_align_entries[bcurrent_align].align_to_px / char_w)
                                    .ceil() as i32;
                                x_offset = target_x;
                            }
                            bcurrent_align += 1;
                            // Skip the character with the display property
                            let (_bch, blen) = decode_utf8(&bstr[bi..]);
                            bi += blen;
                            continue;
                        }

                        // Apply face run if needed; track :extend for end-of-line fill
                        // Uses shared row_extend_bg (unified with buffer text extend tracking)
                        if before_has_runs && bcurrent_run < before_face_runs.len() {
                            if let Some((ext_bg, true)) =
                                overlay_run_bg_extend_at(&before_face_runs, bi)
                            {
                                row_extend_bg = Some((ext_bg, 0));
                                row_extend_row = row as i32;
                            }
                            bcurrent_run = apply_overlay_face_run(
                                &before_face_runs,
                                bi,
                                bcurrent_run,
                                frame_glyphs,
                            );
                        }

                        let (bch, blen) = decode_utf8(&bstr[bi..]);
                        bi += blen;
                        if bch == '\n' {
                            // Fill rest of line if any face on this row had :extend
                            // (shared row_extend_bg covers both buffer text and overlay faces)
                            let remaining = avail_width - x_offset;
                            if remaining > 0.0 {
                                if let Some((ext_bg, _)) =
                                    row_extend_bg.filter(|_| row_extend_row == row as i32)
                                {
                                    let gx = content_x + x_offset;
                                    let gy = row_y[row as usize];
                                    frame_glyphs.add_stretch(
                                        gx,
                                        gy,
                                        remaining,
                                        row_max_height,
                                        ext_bg,
                                        0,
                                        false,
                                    );
                                }
                            }
                            reorder_row_bidi(
                                frame_glyphs,
                                row_glyph_start,
                                frame_glyphs.glyphs.len(),
                                content_x,
                            );
                            col = 0;
                            x_offset = 0.0;
                            row += 1;
                            row_glyph_start = frame_glyphs.glyphs.len();
                            if row >= max_rows {
                                break;
                            }
                            continue;
                        }
                        if bch == '\r' {
                            continue;
                        }

                        let bchar_cols = if is_wide_char(bch) { 2 } else { 1 };
                        let badv = bchar_cols as f32 * char_w;
                        if x_offset + badv > avail_width {
                            if params.truncate_lines {
                                // Skip to next newline, then advance to next row
                                reorder_row_bidi(
                                    frame_glyphs,
                                    row_glyph_start,
                                    frame_glyphs.glyphs.len(),
                                    content_x,
                                );
                                while bi < bstr.len() {
                                    let (sc, sl) = decode_utf8(&bstr[bi..]);
                                    bi += sl;
                                    if sc == '\n' {
                                        col = 0;
                                        x_offset = 0.0;
                                        row += 1;
                                        row_glyph_start = frame_glyphs.glyphs.len();
                                        break;
                                    }
                                }
                                if row >= max_rows {
                                    break;
                                }
                                continue;
                            }
                            reorder_row_bidi(
                                frame_glyphs,
                                row_glyph_start,
                                frame_glyphs.glyphs.len(),
                                content_x,
                            );
                            col = 0;
                            x_offset = 0.0;
                            row += 1;
                            row_glyph_start = frame_glyphs.glyphs.len();
                            if row >= max_rows {
                                break;
                            }
                        }
                        let gx = content_x + x_offset;
                        let gy = row_y[row as usize];
                        frame_glyphs.add_char(bch, gx, gy, badv, char_h, ascent, false);
                        col += bchar_cols;
                        x_offset += badv;
                    }

                    // Restore text face after overlay face was used
                    if (before_has_runs || overlay_before_face.face_id != 0) && current_face_id >= 0
                    {
                        self.apply_face(&self.face_data, frame, frame_glyphs);
                    }
                }

                // After-strings are rendered after the buffer text at the
                // position, so we defer rendering. We'll render them after
                // the character at this position has been processed.
                // (Stored in overlay_after_len for use after char rendering)
            }

            // Check for display text property at property boundaries
            if charpos >= next_display_check {
                neomacs_layout_check_display_prop(
                    buffer,
                    window,
                    charpos,
                    display_str_buf.as_mut_ptr(),
                    display_str_buf.len() as i32,
                    &mut display_prop,
                );

                if display_prop.prop_type != 0 {
                    tracing::debug!(
                        "  display_prop: charpos={} type={} covers_to={} str_len={} img_gpu_id={}",
                        charpos,
                        display_prop.prop_type,
                        display_prop.covers_to,
                        display_prop.str_len,
                        display_prop.image_gpu_id
                    );
                    // Flush ligature run before display property handling
                    flush_run(&self.run_buf, frame_glyphs, ligatures);
                    self.run_buf.clear();
                }

                if display_prop.prop_type == 1 {
                    // String replacement: render the display string instead
                    // of buffer text, skip original chars up to covers_to.

                    // First resolve face at this position
                    if charpos >= next_face_check || current_face_id < 0 {
                        let mut next_check: i64 = 0;
                        let fid = neomacs_layout_face_at_pos(
                            window,
                            charpos,
                            &mut self.face_data as *mut FaceDataFFI,
                            &mut next_check,
                        );
                        if fid >= 0 && fid != current_face_id {
                            face_fg = Color::from_pixel(self.face_data.fg);
                            face_bg = Color::from_pixel(self.face_data.bg);
                            self.apply_face(&self.face_data, frame, frame_glyphs);
                        }
                        next_face_check = if next_check > charpos {
                            next_check
                        } else {
                            charpos + 1
                        };
                    }

                    // Parse face runs for display string (if present)
                    struct DFaceRun {
                        byte_offset: u16,
                        fg: u32,
                        bg: u32,
                        _face_id: u32,
                    }
                    let mut dface_runs: Vec<DFaceRun> = Vec::new();
                    let has_face_runs = display_prop.display_nruns > 0;
                    if has_face_runs {
                        let runs_start = display_prop.str_len as usize;
                        for ri in 0..display_prop.display_nruns as usize {
                            let off = runs_start + ri * 14;
                            if off + 14 <= display_str_buf.len() {
                                let byte_offset = u16::from_ne_bytes([
                                    display_str_buf[off],
                                    display_str_buf[off + 1],
                                ]);
                                let fg = u32::from_ne_bytes([
                                    display_str_buf[off + 2],
                                    display_str_buf[off + 3],
                                    display_str_buf[off + 4],
                                    display_str_buf[off + 5],
                                ]);
                                let bg = u32::from_ne_bytes([
                                    display_str_buf[off + 6],
                                    display_str_buf[off + 7],
                                    display_str_buf[off + 8],
                                    display_str_buf[off + 9],
                                ]);
                                let face_id = u32::from_ne_bytes([
                                    display_str_buf[off + 10],
                                    display_str_buf[off + 11],
                                    display_str_buf[off + 12],
                                    display_str_buf[off + 13],
                                ]);
                                dface_runs.push(DFaceRun {
                                    byte_offset,
                                    fg,
                                    bg,
                                    _face_id: face_id,
                                });
                            }
                        }
                    } else {
                        // Single-face fallback (backward compat)
                        let has_display_face =
                            display_prop.display_fg != 0 || display_prop.display_bg != 0;
                        if has_display_face {
                            let dfg = Color::from_pixel(display_prop.display_fg);
                            let dbg = Color::from_pixel(display_prop.display_bg);
                            frame_glyphs.set_face(
                                0,
                                dfg,
                                Some(dbg),
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

                    // Render display string characters with face runs
                    let dstr = &display_str_buf[..display_prop.str_len as usize];
                    let mut di = 0usize;
                    let mut dcurrent_run = 0usize;
                    while di < dstr.len() && row < max_rows {
                        // Apply face run if needed
                        if has_face_runs && dcurrent_run < dface_runs.len() {
                            while dcurrent_run + 1 < dface_runs.len()
                                && di >= dface_runs[dcurrent_run + 1].byte_offset as usize
                            {
                                dcurrent_run += 1;
                            }
                            if di >= dface_runs[dcurrent_run].byte_offset as usize {
                                let run = &dface_runs[dcurrent_run];
                                if run.fg != 0 || run.bg != 0 {
                                    let rfg = Color::from_pixel(run.fg);
                                    let rbg = Color::from_pixel(run.bg);
                                    frame_glyphs.set_face(
                                        0,
                                        rfg,
                                        Some(rbg),
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
                                if dcurrent_run + 1 < dface_runs.len()
                                    && di + 1 >= dface_runs[dcurrent_run + 1].byte_offset as usize
                                {
                                    dcurrent_run += 1;
                                }
                            }
                        }

                        let (dch, dlen) = decode_utf8(&dstr[di..]);
                        di += dlen;

                        if dch == '\n' || dch == '\r' {
                            continue;
                        }

                        let dchar_cols = if is_wide_char(dch) { 2 } else { 1 };
                        let d_advance = dchar_cols as f32 * char_w;
                        if x_offset + d_advance > avail_width {
                            if params.truncate_lines {
                                break;
                            }
                            reorder_row_bidi(
                                frame_glyphs,
                                row_glyph_start,
                                frame_glyphs.glyphs.len(),
                                content_x,
                            );
                            col = 0;
                            x_offset = 0.0;
                            row += 1;
                            row_glyph_start = frame_glyphs.glyphs.len();
                            if row >= max_rows {
                                break;
                            }
                        }

                        let gx = content_x + x_offset;
                        let gy = row_y[row as usize];
                        frame_glyphs.add_char(dch, gx, gy, d_advance, char_h, ascent, false);
                        col += dchar_cols;
                        x_offset += d_advance;
                    }

                    // Restore text face after display string
                    if (has_face_runs
                        || display_prop.display_fg != 0
                        || display_prop.display_bg != 0)
                        && current_face_id >= 0
                    {
                        self.apply_face(&self.face_data, frame, frame_glyphs);
                    }

                    // Skip original buffer text covered by this display prop
                    let chars_to_skip = display_prop.covers_to - charpos;
                    for _ in 0..chars_to_skip {
                        if byte_idx >= bytes_read as usize {
                            break;
                        }
                        let (_, ch_len) = decode_utf8(&text[byte_idx..]);
                        byte_idx += ch_len;
                    }
                    charpos = display_prop.covers_to;
                    window_end_charpos = charpos;
                    next_display_check = display_prop.covers_to;
                    current_face_id = -1; // force re-check at new position
                    continue;
                } else if display_prop.prop_type == 2 {
                    // Space spec: render a stretch glyph

                    // Resolve face first
                    if charpos >= next_face_check || current_face_id < 0 {
                        let mut next_check: i64 = 0;
                        let fid = neomacs_layout_face_at_pos(
                            window,
                            charpos,
                            &mut self.face_data as *mut FaceDataFFI,
                            &mut next_check,
                        );
                        if fid >= 0 && fid != current_face_id {
                            face_fg = Color::from_pixel(self.face_data.fg);
                            face_bg = Color::from_pixel(self.face_data.bg);
                            self.apply_face(&self.face_data, frame, frame_glyphs);
                        }
                        next_face_check = if next_check > charpos {
                            next_check
                        } else {
                            charpos + 1
                        };
                    }

                    let space_cols = display_prop.space_width.ceil() as i32;
                    let space_pixel_w = display_prop.space_width * char_w;
                    let space_h = if display_prop.space_height > 0.0 {
                        display_prop.space_height
                    } else {
                        char_h
                    };

                    if x_offset + space_pixel_w <= avail_width && row < max_rows {
                        let gx = content_x + x_offset;
                        let gy = row_y[row as usize];
                        Self::add_stretch_for_face(
                            &self.face_data,
                            frame_glyphs,
                            gx,
                            gy,
                            space_pixel_w,
                            space_h,
                            face_bg,
                            self.face_data.face_id,
                            false,
                        );
                        col += space_cols;
                        x_offset += space_pixel_w;
                    }

                    // Skip original buffer text
                    let chars_to_skip = display_prop.covers_to - charpos;
                    for _ in 0..chars_to_skip {
                        if byte_idx >= bytes_read as usize {
                            break;
                        }
                        let (_, ch_len) = decode_utf8(&text[byte_idx..]);
                        byte_idx += ch_len;
                    }
                    charpos = display_prop.covers_to;
                    window_end_charpos = charpos;
                    next_display_check = display_prop.covers_to;
                    current_face_id = -1;
                    continue;
                } else if display_prop.prop_type == 3 {
                    // Align-to spec: render stretch from current col to target column

                    // Resolve face first
                    if charpos >= next_face_check || current_face_id < 0 {
                        let mut next_check: i64 = 0;
                        let fid = neomacs_layout_face_at_pos(
                            window,
                            charpos,
                            &mut self.face_data as *mut FaceDataFFI,
                            &mut next_check,
                        );
                        if fid >= 0 && fid != current_face_id {
                            face_fg = Color::from_pixel(self.face_data.fg);
                            face_bg = Color::from_pixel(self.face_data.bg);
                            self.apply_face(&self.face_data, frame, frame_glyphs);
                        }
                        next_face_check = if next_check > charpos {
                            next_check
                        } else {
                            charpos + 1
                        };
                    }

                    let target_x = display_prop.align_to;
                    if target_x > x_offset && row < max_rows {
                        let gx = content_x + x_offset;
                        let gy = row_y[row as usize];
                        let stretch_w = target_x - x_offset;
                        Self::add_stretch_for_face(
                            &self.face_data,
                            frame_glyphs,
                            gx,
                            gy,
                            stretch_w,
                            char_h,
                            face_bg,
                            self.face_data.face_id,
                            false,
                        );
                        col = (display_prop.align_to / char_w).ceil() as i32;
                        x_offset = target_x;
                    }

                    // Skip original buffer text
                    let chars_to_skip = display_prop.covers_to - charpos;
                    for _ in 0..chars_to_skip {
                        if byte_idx >= bytes_read as usize {
                            break;
                        }
                        let (_, ch_len) = decode_utf8(&text[byte_idx..]);
                        byte_idx += ch_len;
                    }
                    charpos = display_prop.covers_to;
                    window_end_charpos = charpos;
                    next_display_check = display_prop.covers_to;
                    current_face_id = -1;
                    continue;
                } else if display_prop.prop_type == 4 {
                    // Image display property: render image glyph
                    tracing::debug!(
                        "display prop type={} at charpos={} covers_to={}",
                        display_prop.prop_type,
                        charpos,
                        display_prop.covers_to
                    );
                    let img_w = display_prop.image_width as f32;
                    let img_h = display_prop.image_height as f32;
                    let hmargin = display_prop.image_hmargin as f32;
                    let vmargin = display_prop.image_vmargin as f32;
                    // Total dimensions including margins
                    let total_w = img_w + hmargin * 2.0;
                    let total_h = img_h + vmargin * 2.0;

                    if row < max_rows && display_prop.image_gpu_id != 0 {
                        let gx = content_x + x_offset + hmargin;
                        let gy_base = row_y[row as usize];

                        // For images that fit within one text line, use ascent-based
                        // alignment with the text baseline. For taller images, place
                        // at the row top and extend downward.
                        let gy = if total_h <= char_h {
                            let img_ascent = display_prop.image_ascent;
                            let ascent_px = if img_ascent == -1 {
                                // Centered: align middle of image with font baseline center
                                (total_h + ascent - (char_h - ascent) + 1.0) / 2.0
                            } else {
                                // Percentage: ascent% of total height
                                total_h * (img_ascent as f32 / 100.0)
                            };
                            gy_base + ascent - ascent_px + vmargin
                        } else {
                            // Large image: start at current row top
                            gy_base + vmargin
                        };

                        frame_glyphs.add_image(display_prop.image_gpu_id, gx, gy, img_w, img_h);
                        // Advance by total width (including margins)
                        let img_cols = (total_w / char_w).ceil() as i32;
                        col += img_cols;
                        x_offset += total_w;
                        // Track height for row advancement at newline/wrap
                        // (don't advance row now — allows multiple media on same line)
                        if total_h > row_max_height {
                            row_max_height = total_h;
                        }
                    }

                    // Skip original buffer text
                    let chars_to_skip = display_prop.covers_to - charpos;
                    for _ in 0..chars_to_skip {
                        if byte_idx >= bytes_read as usize {
                            break;
                        }
                        let (_, ch_len) = decode_utf8(&text[byte_idx..]);
                        byte_idx += ch_len;
                    }
                    charpos = display_prop.covers_to;
                    window_end_charpos = charpos;
                    next_display_check = display_prop.covers_to;
                    current_face_id = -1;
                    prev_was_non_ascii = false;
                    continue;
                } else if display_prop.prop_type == 9 {
                    // Video display property: render video glyph
                    let vid_w = display_prop.image_width as f32;
                    let vid_h = display_prop.image_height as f32;

                    if row < max_rows && display_prop.video_id != 0 {
                        let gx = content_x + x_offset;
                        let gy = row_y[row as usize];

                        frame_glyphs.add_video(
                            display_prop.video_id,
                            gx,
                            gy,
                            vid_w,
                            vid_h,
                            display_prop.video_loop_count,
                            display_prop.video_autoplay != 0,
                        );
                        let vid_cols = (vid_w / char_w).ceil() as i32;
                        col += vid_cols;
                        x_offset += vid_w;
                        // Track height for row advancement at newline/wrap
                        // (don't advance row now — allows multiple media on same line)
                        if vid_h > row_max_height {
                            row_max_height = vid_h;
                        }
                    }

                    // Skip original buffer text
                    let chars_to_skip = display_prop.covers_to - charpos;
                    for _ in 0..chars_to_skip {
                        if byte_idx >= bytes_read as usize {
                            break;
                        }
                        let (_, ch_len) = decode_utf8(&text[byte_idx..]);
                        byte_idx += ch_len;
                    }
                    charpos = display_prop.covers_to;
                    window_end_charpos = charpos;
                    next_display_check = display_prop.covers_to;
                    current_face_id = -1;
                    prev_was_non_ascii = false;
                    continue;
                } else if display_prop.prop_type == 10 {
                    // WebKit display property: render webkit glyph
                    let wk_w = display_prop.image_width as f32;
                    let wk_h = display_prop.image_height as f32;

                    if row < max_rows && display_prop.webkit_id != 0 {
                        let gx = content_x + x_offset;
                        let gy = row_y[row as usize];

                        frame_glyphs.add_webkit(display_prop.webkit_id, gx, gy, wk_w, wk_h);
                        let wk_cols = (wk_w / char_w).ceil() as i32;
                        col += wk_cols;
                        x_offset += wk_w;
                        // Track height for row advancement at newline/wrap
                        // (don't advance row now — allows multiple media on same line)
                        if wk_h > row_max_height {
                            row_max_height = wk_h;
                        }
                    }

                    // Skip original buffer text
                    let chars_to_skip = display_prop.covers_to - charpos;
                    for _ in 0..chars_to_skip {
                        if byte_idx >= bytes_read as usize {
                            break;
                        }
                        let (_, ch_len) = decode_utf8(&text[byte_idx..]);
                        byte_idx += ch_len;
                    }
                    charpos = display_prop.covers_to;
                    window_end_charpos = charpos;
                    next_display_check = display_prop.covers_to;
                    current_face_id = -1;
                    prev_was_non_ascii = false;
                    continue;
                } else if display_prop.prop_type == 5 || display_prop.prop_type == 8 {
                    // Raise and/or height: modify rendering of subsequent glyphs
                    if display_prop.raise_factor != 0.0 {
                        raise_y_offset = -(display_prop.raise_factor * char_h);
                        raise_end = display_prop.covers_to;
                    }
                    if display_prop.height_factor > 0.0 {
                        height_scale = display_prop.height_factor;
                        height_end = display_prop.covers_to;
                    }
                    next_display_check = display_prop.covers_to;
                    // Don't skip text - these modify rendering, not content
                } else if display_prop.prop_type == 6 || display_prop.prop_type == 7 {
                    // Left-fringe (6) or right-fringe (7) display property
                    let r = row as usize;
                    if display_prop.prop_type == 6 {
                        if r < row_left_fringe.len() {
                            row_left_fringe[r] = (
                                display_prop.fringe_bitmap_id,
                                display_prop.fringe_fg,
                                display_prop.fringe_bg,
                            );
                        }
                    } else {
                        if r < row_right_fringe.len() {
                            row_right_fringe[r] = (
                                display_prop.fringe_bitmap_id,
                                display_prop.fringe_fg,
                                display_prop.fringe_bg,
                            );
                        }
                    }
                    // Skip the covered text
                    let chars_to_skip = display_prop.covers_to - charpos;
                    for _ in 0..chars_to_skip {
                        if byte_idx >= bytes_read as usize {
                            break;
                        }
                        let (_, ch_len) = decode_utf8(&text[byte_idx..]);
                        byte_idx += ch_len;
                    }
                    charpos = display_prop.covers_to;
                    window_end_charpos = charpos;
                    next_display_check = display_prop.covers_to;
                    current_face_id = -1;
                    prev_was_non_ascii = false;
                    continue;
                } else {
                    // No display prop: covers_to tells us when to re-check
                    next_display_check = display_prop.covers_to;
                }

                // Reset raise offset when past the raise region
                if raise_end > 0 && charpos >= raise_end {
                    raise_y_offset = 0.0;
                    raise_end = 0;
                }
                // Reset height scale when past the height region
                if height_end > 0 && charpos >= height_end {
                    height_scale = 0.0;
                    height_end = 0;
                }
            }

            // Resolve face if needed:
            // - entering a new face region from face_at_buffer_position, or
            // - around non-ASCII transitions where FACE_FOR_CHAR can change
            //   the realized font even if text properties are unchanged.
            let force_char_face_check = if byte_idx < bytes_read as usize {
                let (peek_ch, _) = decode_utf8(&text[byte_idx..]);
                !peek_ch.is_ascii() || prev_was_non_ascii
            } else {
                prev_was_non_ascii
            };
            if force_char_face_check || charpos >= next_face_check || current_face_id < 0 {
                let mut next_check: i64 = 0;
                let fid = neomacs_layout_face_at_pos(
                    window,
                    charpos,
                    &mut self.face_data as *mut FaceDataFFI,
                    &mut next_check,
                );

                if fid >= 0 {
                    if fid != current_face_id {
                        // Flush ligature run before face change
                        flush_run(&self.run_buf, frame_glyphs, ligatures);
                        self.run_buf.clear();
                        // Close previous box face region if active.
                        // Box borders are now rendered by the renderer's box span
                        // detection (supports both sharp and SDF rounded corners).
                        if box_active {
                            box_active = false;
                        }

                        current_face_id = fid;
                        face_fg = Color::from_pixel(self.face_data.fg);
                        face_bg = Color::from_pixel(self.face_data.bg);
                        face_space_w = if self.face_data.font_space_width > 0.0 {
                            self.face_data.font_space_width
                        } else {
                            char_w
                        };
                        // Compute per-face line height and ascent.
                        // Scale proportionally from the window's default font metrics.
                        if self.face_data.font_ascent > 0.0 && self.face_data.font_size > 0 {
                            face_ascent = self.face_data.font_ascent;
                            // Line height = ascent + descent, scaled similarly
                            // Use ratio of face font_size to window font_pixel_size
                            let scale = self.face_data.font_size as f32 / params.font_pixel_size;
                            face_h = char_h * scale;
                        } else {
                            face_h = char_h;
                            face_ascent = ascent;
                        }
                        self.apply_face(&self.face_data, frame, frame_glyphs);

                        // Track last face with :extend on this row
                        if self.face_data.extend != 0 {
                            row_extend_bg = Some((face_bg, self.face_data.face_id));
                            row_extend_row = row as i32;
                        }

                        // Start new box face region if this face has a box
                        if self.face_data.box_type > 0 {
                            box_active = true;
                            _box_start_x = content_x + x_offset;
                            _box_row = row;
                        }
                    }
                    // next_check is 0 when face_at_buffer_position returns no limit
                    next_face_check = if next_check > charpos {
                        next_check
                    } else {
                        charpos + 1
                    };
                } else {
                    // Fallback to default face
                    next_face_check = charpos + 1;
                }
            }

            let cursor_byte_idx_at_char = byte_idx;
            let cursor_col_at_char = col;
            let point_at_this_char = !cursor_placed && charpos >= params.point;

            // Decode one UTF-8 character
            let (ch, ch_len) = decode_utf8(&text[byte_idx..]);
            byte_idx += ch_len;
            charpos += 1;
            prev_was_non_ascii = !ch.is_ascii();

            match ch {
                '\n' => {
                    if point_at_this_char {
                        place_cursor_here!(cursor_byte_idx_at_char, cursor_col_at_char);
                    }
                    // Flush ligature run before newline
                    flush_run(&self.run_buf, frame_glyphs, ligatures);
                    self.run_buf.clear();

                    // Bidi reorder: reorder glyph X positions for this completed row
                    reorder_row_bidi(
                        frame_glyphs,
                        row_glyph_start,
                        frame_glyphs.glyphs.len(),
                        content_x,
                    );

                    // Highlight trailing whitespace (overlay stretch on top)
                    if let Some(tw_bg) = trailing_ws_bg {
                        if trailing_ws_start_col >= 0 && trailing_ws_row == row {
                            let tw_x = content_x + trailing_ws_start_x;
                            let tw_w = x_offset - trailing_ws_start_x;
                            let gy = row_y[row as usize];
                            if tw_w > 0.0 {
                                frame_glyphs.add_stretch(tw_x, gy, tw_w, char_h, tw_bg, 0, false);
                            }
                        }
                    }
                    trailing_ws_start_col = -1;

                    // Fill rest of line with stretch.
                    // Use face bg if :extend is set; fall back to row_extend_bg
                    // (last face with :extend on this row) for cases where the
                    // newline itself lacks :extend but earlier text had it
                    // (e.g., completion overlays that don't cover the newline).
                    let remaining = avail_width - x_offset;
                    if remaining > 0.0 {
                        let gx = content_x + x_offset;
                        let gy = row_y[row as usize];
                        let (fill_bg, fill_face) = if self.face_data.extend != 0 {
                            (face_bg, self.face_data.face_id)
                        } else if let Some((ext_bg, ext_face)) =
                            row_extend_bg.filter(|_| row_extend_row == row as i32)
                        {
                            (ext_bg, ext_face)
                        } else {
                            (default_bg, 0)
                        };
                        if fill_face != 0 {
                            frame_glyphs.add_stretch(
                                gx,
                                gy,
                                remaining,
                                row_max_height,
                                fill_bg,
                                fill_face,
                                false,
                            );
                        } else {
                            frame_glyphs.add_stretch(
                                gx,
                                gy,
                                remaining,
                                row_max_height,
                                fill_bg,
                                0,
                                false,
                            );
                        }
                    }

                    // Box face tracking: box stays active across line breaks.
                    // Borders are rendered by the renderer's box span detection.
                    if box_active {
                        _box_start_x = content_x;
                    }

                    // Record hit-test row (newline ends the row)
                    if (row as usize) < row_y.len() {
                        hit_rows.push(HitRow {
                            y_start: row_y[row as usize],
                            y_end: row_y[row as usize] + row_max_height,
                            charpos_start: hit_row_charpos_start,
                            charpos_end: charpos,
                        });
                        hit_row_charpos_start = charpos;
                    }

                    col = 0;
                    x_offset = 0.0;
                    row += 1;
                    row_glyph_start = frame_glyphs.glyphs.len();

                    // Apply extra height from variable-height faces on this row
                    if row_max_height > char_h {
                        let extra = row_max_height - char_h;
                        row_extra_y += extra;
                        for ri in (row as usize)..row_y.len() {
                            row_y[ri] = text_y + ri as f32 * char_h + row_extra_y;
                        }
                    }
                    // Reset per-row tracking for the new row
                    row_max_height = char_h;
                    row_max_ascent = ascent;

                    // Check line-height / line-spacing text properties on the newline
                    {
                        let mut extra_h: f32 = 0.0;
                        let nl_pos = charpos - 1; // the newline we just consumed
                        neomacs_layout_check_line_spacing(
                            buffer,
                            window,
                            nl_pos,
                            char_h,
                            &mut extra_h,
                        );
                        if extra_h > 0.0 {
                            row_extra_y += extra_h;
                            // Update all remaining row_y entries
                            for ri in (row as usize)..row_y.len() {
                                row_y[ri] = text_y + ri as f32 * char_h + row_extra_y;
                            }
                        }
                    }

                    if box_active {
                        _box_row = row;
                    }
                    current_line += 1;
                    need_line_number = lnum_enabled;
                    need_margin_check = has_margins;
                    wrap_has_break = false;
                    hscroll_remaining = hscroll;
                    if !params.line_prefix.is_empty() {
                        need_prefix = 1;
                    }

                    // Selective display: skip lines indented beyond threshold
                    if params.selective_display > 0 {
                        let mut shown_ellipsis = false;
                        while byte_idx < bytes_read as usize && row < max_rows {
                            // Peek at indentation of next line
                            let mut indent = 0i32;
                            let mut peek_idx = byte_idx;
                            while peek_idx < bytes_read as usize {
                                let (pch, plen) = decode_utf8(&text[peek_idx..]);
                                if pch == ' ' {
                                    indent += 1;
                                } else if pch == '\t' {
                                    indent = ((indent / params.tab_width) + 1) * params.tab_width;
                                } else {
                                    break;
                                }
                                peek_idx += plen;
                            }

                            if indent > params.selective_display {
                                // Show ... ellipsis once for the hidden block
                                if !shown_ellipsis && row > 0 {
                                    let gy = row_y[(row - 1) as usize];
                                    for dot_i in 0..3i32.min(cols) {
                                        frame_glyphs.add_char(
                                            '.',
                                            content_x + dot_i as f32 * char_w,
                                            gy,
                                            char_w,
                                            char_h,
                                            ascent,
                                            false,
                                        );
                                    }
                                    shown_ellipsis = true;
                                }
                                // Skip this hidden line
                                while byte_idx < bytes_read as usize {
                                    let (sch, slen) = decode_utf8(&text[byte_idx..]);
                                    byte_idx += slen;
                                    charpos += 1;
                                    if sch == '\n' {
                                        current_line += 1;
                                        break;
                                    }
                                }
                            } else {
                                break; // Next line is visible
                            }
                        }
                    }
                }
                '\t' => {
                    if point_at_this_char {
                        place_cursor_here!(cursor_byte_idx_at_char, cursor_col_at_char);
                    }
                    // Flush ligature run before tab
                    flush_run(&self.run_buf, frame_glyphs, ligatures);
                    self.run_buf.clear();

                    // Tab: advance to next tab stop (column-based, pixel width uses space_w)
                    let next_tab = if !params.tab_stop_list.is_empty() {
                        params
                            .tab_stop_list
                            .iter()
                            .find(|&&stop| (stop as usize) > col as usize)
                            .map(|&stop| stop)
                            .unwrap_or_else(|| {
                                let last = *params.tab_stop_list.last().unwrap();
                                let tab_w = params.tab_width.max(1);
                                if col >= last {
                                    last + ((col - last) / tab_w + 1) * tab_w
                                } else {
                                    last
                                }
                            })
                    } else {
                        let tab_w = params.tab_width.max(1);
                        ((col / tab_w) + 1) * tab_w
                    };
                    // Ensure tab advances at least one column
                    let next_tab = next_tab.max(col + 1);
                    let spaces = (next_tab - col).min(cols - col);
                    let tab_pixel_w = spaces as f32 * face_space_w;

                    // Render tab as stretch glyph (use face bg)
                    let gx = content_x + x_offset;
                    let gy = row_y[row as usize];
                    Self::add_stretch_for_face(
                        &self.face_data,
                        frame_glyphs,
                        gx,
                        gy,
                        tab_pixel_w,
                        char_h,
                        face_bg,
                        self.face_data.face_id,
                        false,
                    );

                    col += spaces;
                    x_offset += tab_pixel_w;
                    // Tab is a breakpoint for word-wrap
                    if params.word_wrap {
                        _wrap_break_col = col;
                        wrap_break_x = x_offset;
                        wrap_break_byte_idx = byte_idx;
                        wrap_break_charpos = charpos;
                        wrap_break_glyph_count = frame_glyphs.glyphs.len();
                        wrap_has_break = true;
                    }
                    if x_offset >= avail_width {
                        // Bidi reorder before advancing to next row
                        reorder_row_bidi(
                            frame_glyphs,
                            row_glyph_start,
                            frame_glyphs.glyphs.len(),
                            content_x,
                        );
                        if params.truncate_lines {
                            if (row as usize) < row_truncated.len() {
                                row_truncated[row as usize] = true;
                            }
                            while byte_idx < bytes_read as usize {
                                let (c, l) = decode_utf8(&text[byte_idx..]);
                                byte_idx += l;
                                charpos += 1;
                                if c == '\n' {
                                    col = 0;
                                    x_offset = 0.0;
                                    row += 1;
                                    row_glyph_start = frame_glyphs.glyphs.len();
                                    current_line += 1;
                                    need_line_number = lnum_enabled;
                                    need_margin_check = has_margins;
                                    wrap_has_break = false;
                                    break;
                                }
                            }
                        } else {
                            if (row as usize) < row_continued.len() {
                                row_continued[row as usize] = true;
                            }
                            col = 0;
                            x_offset = 0.0;
                            row += 1;
                            row_glyph_start = frame_glyphs.glyphs.len();
                            if (row as usize) < row_continuation.len() {
                                row_continuation[row as usize] = true;
                            }
                            wrap_has_break = false;
                            if !params.wrap_prefix.is_empty() {
                                need_prefix = 2;
                            }
                        }
                    }
                }
                '\r' => {
                    if point_at_this_char {
                        place_cursor_here!(cursor_byte_idx_at_char, cursor_col_at_char);
                    }
                    // Flush ligature run before carriage return
                    flush_run(&self.run_buf, frame_glyphs, ligatures);
                    self.run_buf.clear();

                    if params.selective_display > 0 {
                        // In selective-display mode, \r hides until next \n
                        // Show ... ellipsis
                        let gy = row_y[row as usize];
                        if x_offset + 3.0 * char_w <= avail_width {
                            for dot_i in 0..3 {
                                frame_glyphs.add_char(
                                    '.',
                                    content_x + x_offset + dot_i as f32 * char_w,
                                    gy,
                                    char_w,
                                    char_h,
                                    ascent,
                                    false,
                                );
                            }
                        }
                        // Bidi reorder before advancing to next row
                        reorder_row_bidi(
                            frame_glyphs,
                            row_glyph_start,
                            frame_glyphs.glyphs.len(),
                            content_x,
                        );
                        // Skip to next \n
                        while byte_idx < bytes_read as usize {
                            let (sch, slen) = decode_utf8(&text[byte_idx..]);
                            byte_idx += slen;
                            charpos += 1;
                            if sch == '\n' {
                                col = 0;
                                x_offset = 0.0;
                                row += 1;
                                row_glyph_start = frame_glyphs.glyphs.len();
                                current_line += 1;
                                need_line_number = lnum_enabled;
                                need_margin_check = has_margins;
                                wrap_has_break = false;
                                hscroll_remaining = hscroll;
                                break;
                            }
                        }
                    }
                    // Otherwise: carriage return is just skipped
                }
                _ if ch < ' ' || ch == '\x7F' => {
                    if point_at_this_char {
                        place_cursor_here!(cursor_byte_idx_at_char, cursor_col_at_char);
                    }
                    // Flush ligature run before control char
                    flush_run(&self.run_buf, frame_glyphs, ligatures);
                    self.run_buf.clear();

                    // Control character: display as ^X (2 columns)
                    // DEL (0x7F) displays as ^?
                    // Use escape-glyph face for control char display
                    let escape_fg = Color::from_pixel(params.escape_glyph_fg);
                    frame_glyphs.set_face(
                        0,
                        escape_fg,
                        Some(face_bg),
                        400,
                        false,
                        0,
                        None,
                        0,
                        None,
                        0,
                        None,
                    );

                    let gx = content_x + x_offset;
                    let gy = row_y[row as usize];

                    let ctrl_ch = if ch == '\x7F' {
                        '?'
                    } else {
                        char::from((ch as u8) + b'@')
                    };
                    if x_offset + 2.0 * char_w <= avail_width {
                        frame_glyphs.add_char('^', gx, gy, char_w, char_h, ascent, false);
                        frame_glyphs.add_char(
                            ctrl_ch,
                            gx + char_w,
                            gy,
                            char_w,
                            char_h,
                            ascent,
                            false,
                        );
                        col += 2;
                        x_offset += 2.0 * char_w;
                    } else {
                        // Bidi reorder before advancing to next row (control char overflow)
                        reorder_row_bidi(
                            frame_glyphs,
                            row_glyph_start,
                            frame_glyphs.glyphs.len(),
                            content_x,
                        );
                        if params.truncate_lines {
                            while byte_idx < bytes_read as usize {
                                let (c, l) = decode_utf8(&text[byte_idx..]);
                                byte_idx += l;
                                charpos += 1;
                                if c == '\n' {
                                    col = 0;
                                    x_offset = 0.0;
                                    row += 1;
                                    row_glyph_start = frame_glyphs.glyphs.len();
                                    current_line += 1;
                                    need_line_number = lnum_enabled;
                                    need_margin_check = has_margins;
                                    wrap_has_break = false;
                                    break;
                                }
                            }
                        } else {
                            col = 0;
                            x_offset = 0.0;
                            row += 1;
                            row_glyph_start = frame_glyphs.glyphs.len();
                            wrap_has_break = false;
                        }
                    }
                    // Restore text face after escape-glyph
                    if current_face_id >= 0 {
                        self.apply_face(&self.face_data, frame, frame_glyphs);
                    }
                }
                _ => {
                    // Non-breaking space and soft hyphen highlighting
                    if params.nobreak_char_display > 0 && (ch == '\u{00A0}' || ch == '\u{00AD}') {
                        // Flush ligature run before nobreak char special handling
                        flush_run(&self.run_buf, frame_glyphs, ligatures);
                        self.run_buf.clear();
                        let nb_fg = Color::from_pixel(params.nobreak_char_fg);
                        frame_glyphs.set_face(
                            0,
                            nb_fg,
                            Some(face_bg),
                            400,
                            false,
                            0,
                            None,
                            0,
                            None,
                            0,
                            None,
                        );
                        let gx = content_x + x_offset;
                        let gy = row_y[row as usize];
                        let display_ch = if ch == '\u{00A0}' { ' ' } else { '-' };
                        if x_offset + char_w <= avail_width {
                            frame_glyphs
                                .add_char(display_ch, gx, gy, char_w, char_h, ascent, false);
                            col += 1;
                            x_offset += char_w;
                        }
                        // Restore text face
                        if current_face_id >= 0 {
                            self.apply_face(&self.face_data, frame, frame_glyphs);
                        }
                        window_end_charpos = charpos;
                        continue;
                    }

                    // Grapheme cluster detection: collect combining marks,
                    // ZWJ sequences, variation selectors, skin tone modifiers,
                    // and regional indicator pairs with the base character.
                    let (cluster_text, cluster_extra_bytes, cluster_extra_chars) =
                        collect_grapheme_cluster(ch, &text[byte_idx..bytes_read as usize]);

                    if let Some(ref cluster) = cluster_text {
                        // Flush ligature run before grapheme cluster (emoji/ZWJ)
                        flush_run(&self.run_buf, frame_glyphs, ligatures);
                        self.run_buf.clear();
                        // Multi-codepoint grapheme cluster (emoji ZWJ, combining marks, etc.)
                        // Advance past the extra characters consumed
                        byte_idx += cluster_extra_bytes;
                        charpos += cluster_extra_chars as i64;

                        // Determine width: composed emoji are 2 columns wide
                        let char_cols = if is_wide_char(ch) { 2 } else { 1 };
                        let glyph_w = char_cols as f32 * char_w;

                        if x_offset + glyph_w > avail_width {
                            reorder_row_bidi(
                                frame_glyphs,
                                row_glyph_start,
                                frame_glyphs.glyphs.len(),
                                content_x,
                            );
                            if params.truncate_lines {
                                // Skip to end of line
                                while byte_idx < bytes_read as usize {
                                    let (c, l) = decode_utf8(&text[byte_idx..]);
                                    byte_idx += l;
                                    charpos += 1;
                                    if c == '\n' {
                                        col = 0;
                                        x_offset = 0.0;
                                        row += 1;
                                        row_glyph_start = frame_glyphs.glyphs.len();
                                        current_line += 1;
                                        need_line_number = lnum_enabled;
                                        need_margin_check = has_margins;
                                        wrap_has_break = false;
                                        hscroll_remaining = hscroll;
                                        break;
                                    }
                                }
                                window_end_charpos = charpos;
                                continue;
                            } else {
                                // Wrap to next line
                                col = 0;
                                x_offset = 0.0;
                                row += 1;
                                row_glyph_start = frame_glyphs.glyphs.len();
                                wrap_has_break = false;
                                if row >= max_rows {
                                    break;
                                }
                            }
                        }

                        let gx = content_x + x_offset;
                        let gy = row_y[row as usize] + raise_y_offset;

                        if height_scale > 0.0 && height_scale != 1.0 {
                            let orig_size = frame_glyphs.font_size();
                            frame_glyphs.set_font_size(orig_size * height_scale);
                            frame_glyphs.add_composed_char(
                                cluster, ch, gx, gy, glyph_w, char_h, ascent, false,
                            );
                            frame_glyphs.set_font_size(orig_size);
                        } else {
                            frame_glyphs.add_composed_char(
                                cluster, ch, gx, gy, glyph_w, char_h, ascent, false,
                            );
                        }
                        col += char_cols;
                        x_offset += glyph_w;
                        window_end_charpos = charpos;
                        continue;
                    }

                    // Standalone combining mark without a base: render as zero-width
                    // at the previous position (fallback for bare combining marks)
                    if is_cluster_extender(ch)
                        && ch != '\u{200D}'
                        && ch != '\u{200C}'
                        && ch != '\u{200B}'
                        && ch != '\u{200E}'
                        && ch != '\u{200F}'
                        && ch != '\u{FEFF}'
                    {
                        // Flush ligature run before combining mark
                        flush_run(&self.run_buf, frame_glyphs, ligatures);
                        self.run_buf.clear();
                        if x_offset > 0.0 {
                            // Place combining mark at the position of the previous character
                            let gx = content_x + x_offset - char_w;
                            let gy = row_y[row as usize];
                            frame_glyphs.add_char(ch, gx, gy, 0.0, char_h, ascent, false);
                        }
                        window_end_charpos = charpos;
                        continue;
                    }

                    // Glyphless character check for C1 control and other
                    // non-printable chars
                    if is_potentially_glyphless(ch) {
                        // Flush ligature run before glyphless handling
                        flush_run(&self.run_buf, frame_glyphs, ligatures);
                        self.run_buf.clear();
                        let mut method: c_int = 0;
                        let mut str_buf = [0u8; 64];
                        let mut str_len: c_int = 0;
                        neomacs_layout_check_glyphless(
                            frame,
                            ch as c_int,
                            &mut method,
                            str_buf.as_mut_ptr(),
                            64,
                            &mut str_len,
                        );
                        if method != 0 {
                            let glyph_fg = Color::from_pixel(params.glyphless_char_fg);
                            frame_glyphs.set_face(
                                0,
                                glyph_fg,
                                Some(face_bg),
                                400,
                                false,
                                0,
                                None,
                                0,
                                None,
                                0,
                                None,
                            );
                            let gx = content_x + x_offset;
                            let gy = row_y[row as usize];
                            match method {
                                1 => {
                                    // thin-space: 1-pixel-wide stretch
                                    frame_glyphs
                                        .add_stretch(gx, gy, 1.0, char_h, face_bg, 0, false);
                                    x_offset += 1.0;
                                }
                                2 => {
                                    // empty-box: render as hollow box char
                                    if x_offset + char_w <= avail_width {
                                        frame_glyphs.add_char(
                                            '\u{25A1}', gx, gy, char_w, char_h, ascent, false,
                                        );
                                        col += 1;
                                        x_offset += char_w;
                                    }
                                }
                                3 => {
                                    // hex-code: render as [U+XXXX]
                                    let hex = if (ch as u32) < 0x10000 {
                                        format!("U+{:04X}", ch as u32)
                                    } else {
                                        format!("U+{:06X}", ch as u32)
                                    };
                                    let needed = hex.len() as i32;
                                    let needed_px = needed as f32 * char_w;
                                    if x_offset + needed_px <= avail_width {
                                        for (i, hch) in hex.chars().enumerate() {
                                            frame_glyphs.add_char(
                                                hch,
                                                gx + i as f32 * char_w,
                                                gy,
                                                char_w,
                                                char_h,
                                                ascent,
                                                false,
                                            );
                                        }
                                        col += needed;
                                        x_offset += needed_px;
                                    } else {
                                        x_offset = avail_width; // truncate
                                    }
                                }
                                4 => {
                                    // acronym: render the string
                                    if str_len > 0 {
                                        let s = std::str::from_utf8_unchecked(
                                            &str_buf[..str_len as usize],
                                        );
                                        let needed = s.len() as i32;
                                        let needed_px = needed as f32 * char_w;
                                        if x_offset + needed_px <= avail_width {
                                            for (i, ach) in s.chars().enumerate() {
                                                frame_glyphs.add_char(
                                                    ach,
                                                    gx + i as f32 * char_w,
                                                    gy,
                                                    char_w,
                                                    char_h,
                                                    ascent,
                                                    false,
                                                );
                                            }
                                            col += needed;
                                            x_offset += needed_px;
                                        } else {
                                            x_offset = avail_width;
                                        }
                                    }
                                }
                                5 => {
                                    // zero-width: skip entirely
                                }
                                _ => {}
                            }
                            // Restore face
                            if current_face_id >= 0 {
                                self.apply_face(&self.face_data, frame, frame_glyphs);
                            }
                            if point_at_this_char && !cursor_placed {
                                place_cursor_here!(cursor_byte_idx_at_char, cursor_col_at_char);
                            }
                            window_end_charpos = charpos;
                            continue;
                        }
                    }

                    // Track word-wrap breakpoints: after space/tab
                    if params.word_wrap && (ch == ' ' || ch == '\t') {
                        // Record break AFTER this whitespace
                        // (will be updated after rendering below)
                    }

                    // Normal character — compute advance width
                    let char_cols = if is_wide_char(ch) { 2 } else { 1 };
                    let face_id = self.face_data.face_id;
                    let font_size = self.face_data.font_size;
                    let face_char_w = self.face_data.font_char_width;
                    // Resolve effective family once per face change (not per char)
                    if face_id != self.resolved_family_face_id {
                        let font_family = if !self.face_data.font_family.is_null() {
                            CStr::from_ptr(self.face_data.font_family)
                                .to_str()
                                .unwrap_or("")
                        } else {
                            ""
                        };
                        let font_file_path_str = if !self.face_data.font_file_path.is_null() {
                            CStr::from_ptr(self.face_data.font_file_path)
                                .to_str()
                                .ok()
                                .filter(|s| !s.is_empty())
                        } else {
                            None
                        };
                        self.current_resolved_family = if let Some(ref mut svc) = self.font_metrics
                        {
                            svc.resolve_family(font_family, font_file_path_str)
                        } else {
                            font_family.to_string()
                        };
                        self.resolved_family_face_id = face_id;
                    }
                    let font_weight = self.face_data.font_weight as u16;
                    let font_italic = self.face_data.italic != 0;
                    let advance = char_advance(
                        &mut self.ascii_width_cache,
                        &mut self.font_metrics,
                        ch,
                        char_cols,
                        char_w,
                        face_id,
                        font_size,
                        face_char_w,
                        window,
                        &self.current_resolved_family,
                        font_weight,
                        font_italic,
                    );

                    if x_offset + advance > avail_width {
                        // Flush ligature run before line wrap/truncation
                        flush_run(&self.run_buf, frame_glyphs, ligatures);
                        self.run_buf.clear();

                        // Line full
                        if params.truncate_lines {
                            // Bidi reorder this completed row before truncation
                            reorder_row_bidi(
                                frame_glyphs,
                                row_glyph_start,
                                frame_glyphs.glyphs.len(),
                                content_x,
                            );
                            // Show $ truncation indicator at right edge
                            let trunc_x = content_x + avail_width - char_w;
                            let gy = row_y[row as usize];
                            frame_glyphs.add_char('$', trunc_x, gy, char_w, char_h, ascent, false);
                            if (row as usize) < row_truncated.len() {
                                row_truncated[row as usize] = true;
                            }

                            while byte_idx < bytes_read as usize {
                                let (c, l) = decode_utf8(&text[byte_idx..]);
                                byte_idx += l;
                                charpos += 1;
                                if c == '\n' {
                                    col = 0;
                                    x_offset = 0.0;
                                    row += 1;
                                    row_glyph_start = frame_glyphs.glyphs.len();
                                    // Apply variable-height row adjustment
                                    if row_max_height > char_h {
                                        row_extra_y += row_max_height - char_h;
                                        for ri in (row as usize)..row_y.len() {
                                            row_y[ri] = text_y + ri as f32 * char_h + row_extra_y;
                                        }
                                    }
                                    row_max_height = char_h;
                                    row_max_ascent = ascent;
                                    current_line += 1;
                                    need_line_number = lnum_enabled;
                                    need_margin_check = has_margins;
                                    wrap_has_break = false;
                                    hscroll_remaining = hscroll;
                                    break;
                                }
                            }
                            continue;
                        } else if params.word_wrap && wrap_has_break && wrap_break_x > 0.0 {
                            // Word-wrap: rewind to last breakpoint
                            frame_glyphs.glyphs.truncate(wrap_break_glyph_count);
                            // Fill from break to end of line with bg
                            let fill_w = avail_width - wrap_break_x;
                            if fill_w > 0.0 {
                                let gx = content_x + wrap_break_x;
                                let gy = row_y[row as usize];
                                Self::add_stretch_for_face(
                                    &self.face_data,
                                    frame_glyphs,
                                    gx,
                                    gy,
                                    fill_w,
                                    row_max_height,
                                    face_bg,
                                    self.face_data.face_id,
                                    false,
                                );
                            }
                            // Bidi reorder after word-wrap truncation (re-reorder the truncated glyphs)
                            reorder_row_bidi(
                                frame_glyphs,
                                row_glyph_start,
                                frame_glyphs.glyphs.len(),
                                content_x,
                            );
                            if (row as usize) < row_continued.len() {
                                row_continued[row as usize] = true;
                            }
                            // Rewind position to the break
                            byte_idx = wrap_break_byte_idx;
                            charpos = wrap_break_charpos;
                            // Record hit-test row (word-wrap break)
                            if (row as usize) < row_y.len() {
                                hit_rows.push(HitRow {
                                    y_start: row_y[row as usize],
                                    y_end: row_y[row as usize] + row_max_height,
                                    charpos_start: hit_row_charpos_start,
                                    charpos_end: charpos,
                                });
                                hit_row_charpos_start = charpos;
                            }
                            // Force face re-check since we rewound
                            current_face_id = -1;
                            col = 0;
                            x_offset = 0.0;
                            row += 1;
                            row_glyph_start = frame_glyphs.glyphs.len();
                            // Apply variable-height row adjustment
                            if row_max_height > char_h {
                                row_extra_y += row_max_height - char_h;
                                for ri in (row as usize)..row_y.len() {
                                    row_y[ri] = text_y + ri as f32 * char_h + row_extra_y;
                                }
                            }
                            row_max_height = char_h;
                            row_max_ascent = ascent;
                            if (row as usize) < row_continuation.len() {
                                row_continuation[row as usize] = true;
                            }
                            wrap_has_break = false;
                            if !params.wrap_prefix.is_empty() {
                                need_prefix = 2;
                            }
                            if row >= max_rows {
                                break;
                            }
                            continue;
                        } else {
                            // Bidi reorder this completed row before char-wrap
                            reorder_row_bidi(
                                frame_glyphs,
                                row_glyph_start,
                                frame_glyphs.glyphs.len(),
                                content_x,
                            );
                            // Character wrap: fill remaining space
                            let remaining = avail_width - x_offset;
                            if remaining > 0.0 {
                                let gx = content_x + x_offset;
                                let gy = row_y[row as usize];
                                Self::add_stretch_for_face(
                                    &self.face_data,
                                    frame_glyphs,
                                    gx,
                                    gy,
                                    remaining,
                                    row_max_height,
                                    face_bg,
                                    self.face_data.face_id,
                                    false,
                                );
                            }
                            if (row as usize) < row_continued.len() {
                                row_continued[row as usize] = true;
                            }
                            // Record hit-test row (char-wrap break)
                            if (row as usize) < row_y.len() {
                                hit_rows.push(HitRow {
                                    y_start: row_y[row as usize],
                                    y_end: row_y[row as usize] + row_max_height,
                                    charpos_start: hit_row_charpos_start,
                                    charpos_end: charpos,
                                });
                                hit_row_charpos_start = charpos;
                            }
                            col = 0;
                            x_offset = 0.0;
                            row += 1;
                            row_glyph_start = frame_glyphs.glyphs.len();
                            // Apply variable-height row adjustment
                            if row_max_height > char_h {
                                row_extra_y += row_max_height - char_h;
                                for ri in (row as usize)..row_y.len() {
                                    row_y[ri] = text_y + ri as f32 * char_h + row_extra_y;
                                }
                            }
                            row_max_height = char_h;
                            row_max_ascent = ascent;
                            if (row as usize) < row_continuation.len() {
                                row_continuation[row as usize] = true;
                            }
                            wrap_has_break = false;
                            if !params.wrap_prefix.is_empty() {
                                need_prefix = 2;
                            }
                            if row >= max_rows {
                                break;
                            }
                        }
                    }

                    // Cursor must use final visual position of this character.
                    // Place it after wrap decisions, before drawing the glyph.
                    if point_at_this_char && !cursor_placed {
                        place_cursor_here!(cursor_byte_idx_at_char, cursor_col_at_char);
                    }

                    // Track per-row max height for variable-height faces
                    if face_h > row_max_height {
                        row_max_height = face_h;
                    }
                    if face_ascent > row_max_ascent {
                        row_max_ascent = face_ascent;
                    }

                    // Spaces break ligature runs (they never ligate) and serve
                    // as word-wrap breakpoints. Flush and emit individually.
                    if ch == ' ' {
                        flush_run(&self.run_buf, frame_glyphs, ligatures);
                        self.run_buf.clear();

                        let gx = content_x + x_offset;
                        let gy = row_y[row as usize] + raise_y_offset;
                        if height_scale > 0.0 && height_scale != 1.0 {
                            let orig_size = frame_glyphs.font_size();
                            frame_glyphs.set_font_size(orig_size * height_scale);
                            frame_glyphs.add_char(ch, gx, gy, advance, face_h, face_ascent, false);
                            frame_glyphs.set_font_size(orig_size);
                        } else {
                            frame_glyphs.add_char(ch, gx, gy, advance, face_h, face_ascent, false);
                        }
                    } else if ligatures {
                        // Accumulate into ligature run
                        let gy = row_y[row as usize] + raise_y_offset;
                        if self.run_buf.is_empty() {
                            let gx = content_x + x_offset;
                            self.run_buf.start(
                                gx,
                                gy,
                                face_h,
                                face_ascent,
                                self.face_data.face_id,
                                false,
                                height_scale,
                            );
                        }
                        self.run_buf.push(ch, advance);

                        // Flush at max run length to limit texture sizes
                        if self.run_buf.len() >= MAX_LIGATURE_RUN_LEN {
                            flush_run(&self.run_buf, frame_glyphs, ligatures);
                            self.run_buf.clear();
                        }
                    } else {
                        // Ligatures disabled: emit directly
                        let gx = content_x + x_offset;
                        let gy = row_y[row as usize] + raise_y_offset;
                        if height_scale > 0.0 && height_scale != 1.0 {
                            let orig_size = frame_glyphs.font_size();
                            frame_glyphs.set_font_size(orig_size * height_scale);
                            frame_glyphs.add_char(ch, gx, gy, advance, face_h, face_ascent, false);
                            frame_glyphs.set_font_size(orig_size);
                        } else {
                            frame_glyphs.add_char(ch, gx, gy, advance, face_h, face_ascent, false);
                        }
                    }
                    col += char_cols;
                    x_offset += advance;

                    // Track trailing whitespace
                    if trailing_ws_bg.is_some() {
                        if ch == ' ' || ch == '\t' {
                            if trailing_ws_start_col < 0 {
                                trailing_ws_start_col = col - char_cols;
                                trailing_ws_start_x = x_offset - advance;
                                trailing_ws_row = row;
                            }
                        } else {
                            trailing_ws_start_col = -1;
                        }
                    }

                    // Record break AFTER whitespace characters
                    if params.word_wrap && (ch == ' ' || ch == '\t') {
                        // Flush ligature run at word-wrap boundary so truncate()
                        // never cuts inside a composed glyph
                        flush_run(&self.run_buf, frame_glyphs, ligatures);
                        self.run_buf.clear();

                        _wrap_break_col = col;
                        wrap_break_x = x_offset;
                        wrap_break_byte_idx = byte_idx;
                        wrap_break_charpos = charpos;
                        wrap_break_glyph_count = frame_glyphs.glyphs.len();
                        wrap_has_break = true;
                    }
                }
            }

            // Flush ligature run only if we have overlay after-strings to render
            if overlay_after_len > 0 {
                flush_run(&self.run_buf, frame_glyphs, ligatures);
                self.run_buf.clear();
            }

            // Place cursor before rendering overlay after-string.
            // When an overlay ends at point (e.g., fido-vertical-mode completions),
            // the after-string visually follows the cursor. Without this check,
            // the cursor would be placed after the entire after-string content.
            if !cursor_placed && charpos >= params.point && overlay_after_len > 0 {
                cursor_col = col;
                cursor_x = x_offset;
                cursor_row = row;
                let cursor_px = content_x + x_offset;
                let cursor_y = row_y[row as usize];

                // Use face-specific dimensions so cursor matches variable-height faces
                let cursor_face_w = if self.face_data.font_char_width > 0.0 {
                    self.face_data.font_char_width
                } else {
                    char_w
                };

                let cursor_style = cursor_style_for_window(params);

                if let Some(style) = cursor_style {
                    let cursor_w =
                        cursor_width_for_style(style, text, byte_idx, col, params, cursor_face_w);
                    frame_glyphs.add_cursor(
                        params.window_id as i32,
                        cursor_px,
                        cursor_y,
                        cursor_w,
                        face_h,
                        style,
                        Color::from_pixel(params.cursor_color),
                    );

                    if matches!(style, CursorStyle::FilledBox) {
                        frame_glyphs.set_cursor_inverse(
                            cursor_px,
                            cursor_y,
                            cursor_w,
                            face_h,
                            Color::from_pixel(params.cursor_color),
                            default_bg,
                        );
                    }
                }

                cursor_placed = true;
            }

            // Render overlay after-string (if any) — collected earlier
            if overlay_after_len > 0 && row < max_rows {
                let after_has_runs = overlay_after_nruns > 0;
                let after_face_runs = if after_has_runs {
                    parse_overlay_face_runs(
                        &overlay_after_buf,
                        overlay_after_len as usize,
                        overlay_after_nruns,
                    )
                } else {
                    Vec::new()
                };
                let after_align_entries = if overlay_after_naligns > 0 {
                    parse_overlay_align_entries(
                        &overlay_after_buf,
                        overlay_after_len as usize,
                        overlay_after_nruns,
                        overlay_after_naligns,
                    )
                } else {
                    Vec::new()
                };
                let mut acurrent_align = 0usize;

                // Apply overlay face for after-string if no per-char runs
                if !after_has_runs && overlay_after_face.face_id != 0 {
                    self.apply_face(&overlay_after_face, frame, frame_glyphs);
                }

                let astr = &overlay_after_buf[..overlay_after_len as usize];
                let mut ai = 0usize;
                let mut acurrent_run = 0usize;
                while ai < astr.len() && row < max_rows {
                    // Check for align-to entry at this byte offset
                    if acurrent_align < after_align_entries.len()
                        && ai == after_align_entries[acurrent_align].byte_offset as usize
                    {
                        let target_x = after_align_entries[acurrent_align].align_to_px;
                        if target_x > x_offset {
                            let gx = content_x + x_offset;
                            let gy = row_y[row as usize];
                            let stretch_w = target_x - x_offset;
                            // Use overlay face run's bg, not buffer position's face_bg.
                            // See before-string comment for rationale.
                            let stretch_bg = overlay_run_bg_at(&after_face_runs, ai, default_bg);
                            frame_glyphs
                                .add_stretch(gx, gy, stretch_w, char_h, stretch_bg, 0, false);
                            col = (after_align_entries[acurrent_align].align_to_px / char_w).ceil()
                                as i32;
                            x_offset = target_x;
                        }
                        acurrent_align += 1;
                        let (_ach, alen) = decode_utf8(&astr[ai..]);
                        ai += alen;
                        continue;
                    }

                    // Apply face run if needed; track :extend for end-of-line fill
                    // Uses shared row_extend_bg (unified with buffer text extend tracking)
                    if after_has_runs && acurrent_run < after_face_runs.len() {
                        if let Some((ext_bg, true)) = overlay_run_bg_extend_at(&after_face_runs, ai)
                        {
                            row_extend_bg = Some((ext_bg, 0));
                            row_extend_row = row as i32;
                        }
                        acurrent_run = apply_overlay_face_run(
                            &after_face_runs,
                            ai,
                            acurrent_run,
                            frame_glyphs,
                        );
                    }

                    let (ach, alen) = decode_utf8(&astr[ai..]);
                    ai += alen;
                    if ach == '\n' {
                        // Fill rest of line if any face on this row had :extend
                        // (shared row_extend_bg covers both buffer text and overlay faces)
                        let remaining = avail_width - x_offset;
                        if remaining > 0.0 {
                            if let Some((ext_bg, _)) =
                                row_extend_bg.filter(|_| row_extend_row == row as i32)
                            {
                                let gx = content_x + x_offset;
                                let gy = row_y[row as usize];
                                frame_glyphs.add_stretch(
                                    gx,
                                    gy,
                                    remaining,
                                    row_max_height,
                                    ext_bg,
                                    0,
                                    false,
                                );
                            }
                        }
                        reorder_row_bidi(
                            frame_glyphs,
                            row_glyph_start,
                            frame_glyphs.glyphs.len(),
                            content_x,
                        );
                        col = 0;
                        x_offset = 0.0;
                        row += 1;
                        row_glyph_start = frame_glyphs.glyphs.len();
                        if row >= max_rows {
                            break;
                        }
                        continue;
                    }
                    if ach == '\r' {
                        continue;
                    }

                    let achar_cols = if is_wide_char(ach) { 2 } else { 1 };
                    let a_advance = achar_cols as f32 * char_w;
                    if x_offset + a_advance > avail_width {
                        if params.truncate_lines {
                            reorder_row_bidi(
                                frame_glyphs,
                                row_glyph_start,
                                frame_glyphs.glyphs.len(),
                                content_x,
                            );
                            while ai < astr.len() {
                                let (sc, sl) = decode_utf8(&astr[ai..]);
                                ai += sl;
                                if sc == '\n' {
                                    col = 0;
                                    x_offset = 0.0;
                                    row += 1;
                                    row_glyph_start = frame_glyphs.glyphs.len();
                                    break;
                                }
                            }
                            if row >= max_rows {
                                break;
                            }
                            continue;
                        }
                        reorder_row_bidi(
                            frame_glyphs,
                            row_glyph_start,
                            frame_glyphs.glyphs.len(),
                            content_x,
                        );
                        col = 0;
                        x_offset = 0.0;
                        row += 1;
                        row_glyph_start = frame_glyphs.glyphs.len();
                        if row >= max_rows {
                            break;
                        }
                    }
                    let gx = content_x + x_offset;
                    let gy = row_y[row as usize];
                    frame_glyphs.add_char(ach, gx, gy, a_advance, char_h, ascent, false);
                    col += achar_cols;
                    x_offset += a_advance;
                }

                // Restore text face after overlay after-string
                if (after_has_runs || overlay_after_face.face_id != 0) && current_face_id >= 0 {
                    self.apply_face(&self.face_data, frame, frame_glyphs);
                }
            }

            window_end_charpos = charpos;
        }

        // Flush any remaining ligature run at end of buffer
        flush_run(&self.run_buf, frame_glyphs, ligatures);
        self.run_buf.clear();

        tracing::debug!(
            "  layout_window done: charpos={} byte_idx={} row={} glyphs={} end_charpos={}",
            charpos,
            byte_idx,
            row,
            frame_glyphs.glyphs.len(),
            window_end_charpos
        );

        // Place cursor before end-of-buffer overlay strings.
        // When point is at end-of-buffer and overlays have after-strings there
        // (e.g., fido-vertical-mode completions), the cursor must be placed
        // BEFORE the overlay content is rendered.
        if !cursor_placed && params.point >= window_start && charpos >= params.point {
            let clamped_row = row.min(max_rows - 1);
            let cursor_y = row_y[clamped_row as usize];

            // Safety: don't place cursor past text area
            if cursor_y < text_y_limit {
                cursor_col = col;
                cursor_x = x_offset;
                cursor_row = clamped_row;
                let cursor_px = content_x + x_offset;

                let cursor_face_w = if self.face_data.font_char_width > 0.0 {
                    self.face_data.font_char_width
                } else {
                    char_w
                };

                let cursor_style = cursor_style_for_window(params);

                if let Some(style) = cursor_style {
                    let cursor_w =
                        cursor_width_for_style(style, text, byte_idx, col, params, cursor_face_w);
                    frame_glyphs.add_cursor(
                        params.window_id as i32,
                        cursor_px,
                        cursor_y,
                        cursor_w,
                        face_h,
                        style,
                        Color::from_pixel(params.cursor_color),
                    );

                    if matches!(style, CursorStyle::FilledBox) {
                        frame_glyphs.set_cursor_inverse(
                            cursor_px,
                            cursor_y,
                            cursor_w,
                            face_h,
                            Color::from_pixel(params.cursor_color),
                            default_bg,
                        );
                    }
                }

                cursor_placed = true;
            }
            // If cursor_y >= text_y_limit, skip placement — forward scroll will fix next frame
        }

        // Check for overlay strings at end-of-buffer (e.g., fido-vertical-mode
        // completions placed as after-strings at point-max).
        if row < max_rows {
            overlay_after_len = 0;
            overlay_after_face = FaceDataFFI::default();
            overlay_before_nruns = 0;
            overlay_after_nruns = 0;
            let mut eob_before_len: i32 = 0;
            let mut eob_before_face = FaceDataFFI::default();
            let mut eob_left_fringe_bitmap: i32 = 0;
            let mut eob_left_fringe_fg: u32 = 0;
            let mut eob_left_fringe_bg: u32 = 0;
            let mut eob_right_fringe_bitmap: i32 = 0;
            let mut eob_right_fringe_fg: u32 = 0;
            let mut eob_right_fringe_bg: u32 = 0;
            let mut eob_before_naligns: i32 = 0;
            let mut eob_after_naligns: i32 = 0;
            neomacs_layout_overlay_strings_at(
                buffer,
                window,
                charpos,
                overlay_before_buf.as_mut_ptr(),
                overlay_before_buf.len() as i32,
                &mut eob_before_len,
                overlay_after_buf.as_mut_ptr(),
                overlay_after_buf.len() as i32,
                &mut overlay_after_len,
                &mut eob_before_face,
                &mut overlay_after_face,
                &mut overlay_before_nruns,
                &mut overlay_after_nruns,
                &mut eob_left_fringe_bitmap,
                &mut eob_left_fringe_fg,
                &mut eob_left_fringe_bg,
                &mut eob_right_fringe_bitmap,
                &mut eob_right_fringe_fg,
                &mut eob_right_fringe_bg,
                &mut eob_before_naligns,
                &mut eob_after_naligns,
            );

            // Store fringe bitmaps from overlay display properties at EOB
            let r = row as usize;
            if eob_left_fringe_bitmap > 0 && r < row_left_fringe.len() {
                row_left_fringe[r] = (
                    eob_left_fringe_bitmap,
                    eob_left_fringe_fg,
                    eob_left_fringe_bg,
                );
            }
            if eob_right_fringe_bitmap > 0 && r < row_right_fringe.len() {
                row_right_fringe[r] = (
                    eob_right_fringe_bitmap,
                    eob_right_fringe_fg,
                    eob_right_fringe_bg,
                );
            }

            // Render before-string at end-of-buffer
            if eob_before_len > 0 {
                let eob_before_has_runs = overlay_before_nruns > 0;
                let eob_before_face_runs = if eob_before_has_runs {
                    parse_overlay_face_runs(
                        &overlay_before_buf,
                        eob_before_len as usize,
                        overlay_before_nruns,
                    )
                } else {
                    Vec::new()
                };
                let eob_before_align_entries = if eob_before_naligns > 0 {
                    parse_overlay_align_entries(
                        &overlay_before_buf,
                        eob_before_len as usize,
                        overlay_before_nruns,
                        eob_before_naligns,
                    )
                } else {
                    Vec::new()
                };
                let mut eob_bcurrent_align = 0usize;

                if !eob_before_has_runs && eob_before_face.face_id != 0 {
                    self.apply_face(&eob_before_face, frame, frame_glyphs);
                }
                let bstr = &overlay_before_buf[..eob_before_len as usize];
                let mut bi = 0usize;
                let mut bcurrent_run = 0usize;
                while bi < bstr.len() && row < max_rows {
                    // Check for align-to entry at this byte offset
                    if eob_bcurrent_align < eob_before_align_entries.len()
                        && bi == eob_before_align_entries[eob_bcurrent_align].byte_offset as usize
                    {
                        let target_x = eob_before_align_entries[eob_bcurrent_align].align_to_px;
                        if target_x > x_offset {
                            let gx = content_x + x_offset;
                            let gy = row_y[row as usize];
                            let stretch_w = target_x - x_offset;
                            let stretch_bg =
                                overlay_run_bg_at(&eob_before_face_runs, bi, default_bg);
                            frame_glyphs
                                .add_stretch(gx, gy, stretch_w, char_h, stretch_bg, 0, false);
                            col = (eob_before_align_entries[eob_bcurrent_align].align_to_px
                                / char_w)
                                .ceil() as i32;
                            x_offset = target_x;
                        }
                        eob_bcurrent_align += 1;
                        let (_bch, blen) = decode_utf8(&bstr[bi..]);
                        bi += blen;
                        continue;
                    }

                    if eob_before_has_runs && bcurrent_run < eob_before_face_runs.len() {
                        if let Some((ext_bg, true)) =
                            overlay_run_bg_extend_at(&eob_before_face_runs, bi)
                        {
                            row_extend_bg = Some((ext_bg, 0));
                            row_extend_row = row as i32;
                        }
                        bcurrent_run = apply_overlay_face_run(
                            &eob_before_face_runs,
                            bi,
                            bcurrent_run,
                            frame_glyphs,
                        );
                    }

                    let (bch, blen) = decode_utf8(&bstr[bi..]);
                    bi += blen;
                    if bch == '\n' {
                        // Fill rest of line if any face on this row had :extend
                        // (shared row_extend_bg covers both buffer text and overlay faces)
                        let remaining = avail_width - x_offset;
                        if remaining > 0.0 {
                            if let Some((ext_bg, _)) =
                                row_extend_bg.filter(|_| row_extend_row == row as i32)
                            {
                                let gx = content_x + x_offset;
                                let gy = row_y[row as usize];
                                frame_glyphs.add_stretch(
                                    gx,
                                    gy,
                                    remaining,
                                    row_max_height,
                                    ext_bg,
                                    0,
                                    false,
                                );
                            }
                        }
                        reorder_row_bidi(
                            frame_glyphs,
                            row_glyph_start,
                            frame_glyphs.glyphs.len(),
                            content_x,
                        );
                        col = 0;
                        x_offset = 0.0;
                        row += 1;
                        row_glyph_start = frame_glyphs.glyphs.len();
                        if row >= max_rows {
                            break;
                        }
                        continue;
                    }
                    if bch == '\r' {
                        continue;
                    }
                    let bchar_cols = if is_wide_char(bch) { 2 } else { 1 };
                    let b_advance = bchar_cols as f32 * char_w;
                    if x_offset + b_advance > avail_width {
                        if params.truncate_lines {
                            reorder_row_bidi(
                                frame_glyphs,
                                row_glyph_start,
                                frame_glyphs.glyphs.len(),
                                content_x,
                            );
                            while bi < bstr.len() {
                                let (sc, sl) = decode_utf8(&bstr[bi..]);
                                bi += sl;
                                if sc == '\n' {
                                    col = 0;
                                    x_offset = 0.0;
                                    row += 1;
                                    row_glyph_start = frame_glyphs.glyphs.len();
                                    break;
                                }
                            }
                            if row >= max_rows {
                                break;
                            }
                            continue;
                        }
                        reorder_row_bidi(
                            frame_glyphs,
                            row_glyph_start,
                            frame_glyphs.glyphs.len(),
                            content_x,
                        );
                        col = 0;
                        x_offset = 0.0;
                        row += 1;
                        row_glyph_start = frame_glyphs.glyphs.len();
                        if row >= max_rows {
                            break;
                        }
                    }
                    let gx = content_x + x_offset;
                    let gy = row_y[row as usize];
                    frame_glyphs.add_char(bch, gx, gy, b_advance, char_h, ascent, false);
                    col += bchar_cols;
                    x_offset += b_advance;
                }
                if (eob_before_has_runs || eob_before_face.face_id != 0) && current_face_id >= 0 {
                    self.apply_face(&self.face_data, frame, frame_glyphs);
                }
            }

            // Render after-string at end-of-buffer
            if overlay_after_len > 0 {
                let eob_after_has_runs = overlay_after_nruns > 0;
                let eob_after_face_runs = if eob_after_has_runs {
                    parse_overlay_face_runs(
                        &overlay_after_buf,
                        overlay_after_len as usize,
                        overlay_after_nruns,
                    )
                } else {
                    Vec::new()
                };
                let eob_after_align_entries = if eob_after_naligns > 0 {
                    parse_overlay_align_entries(
                        &overlay_after_buf,
                        overlay_after_len as usize,
                        overlay_after_nruns,
                        eob_after_naligns,
                    )
                } else {
                    Vec::new()
                };
                let mut eob_acurrent_align = 0usize;

                if !eob_after_has_runs && overlay_after_face.face_id != 0 {
                    self.apply_face(&overlay_after_face, frame, frame_glyphs);
                }
                let astr = &overlay_after_buf[..overlay_after_len as usize];
                let mut ai = 0usize;
                let mut acurrent_run = 0usize;
                while ai < astr.len() && row < max_rows {
                    // Check for align-to entry at this byte offset
                    if eob_acurrent_align < eob_after_align_entries.len()
                        && ai == eob_after_align_entries[eob_acurrent_align].byte_offset as usize
                    {
                        let target_x = eob_after_align_entries[eob_acurrent_align].align_to_px;
                        if target_x > x_offset {
                            let gx = content_x + x_offset;
                            let gy = row_y[row as usize];
                            let stretch_w = target_x - x_offset;
                            let stretch_bg =
                                overlay_run_bg_at(&eob_after_face_runs, ai, default_bg);
                            frame_glyphs
                                .add_stretch(gx, gy, stretch_w, char_h, stretch_bg, 0, false);
                            col = (eob_after_align_entries[eob_acurrent_align].align_to_px / char_w)
                                .ceil() as i32;
                            x_offset = target_x;
                        }
                        eob_acurrent_align += 1;
                        let (_ach, alen) = decode_utf8(&astr[ai..]);
                        ai += alen;
                        continue;
                    }

                    if eob_after_has_runs && acurrent_run < eob_after_face_runs.len() {
                        if let Some((ext_bg, true)) =
                            overlay_run_bg_extend_at(&eob_after_face_runs, ai)
                        {
                            row_extend_bg = Some((ext_bg, 0));
                            row_extend_row = row as i32;
                        }
                        acurrent_run = apply_overlay_face_run(
                            &eob_after_face_runs,
                            ai,
                            acurrent_run,
                            frame_glyphs,
                        );
                    }

                    let (ach, alen) = decode_utf8(&astr[ai..]);
                    ai += alen;
                    if ach == '\n' {
                        // Fill rest of line if any face on this row had :extend
                        // (shared row_extend_bg covers both buffer text and overlay faces)
                        let remaining = avail_width - x_offset;
                        if remaining > 0.0 {
                            if let Some((ext_bg, _)) =
                                row_extend_bg.filter(|_| row_extend_row == row as i32)
                            {
                                let gx = content_x + x_offset;
                                let gy = row_y[row as usize];
                                frame_glyphs.add_stretch(
                                    gx,
                                    gy,
                                    remaining,
                                    row_max_height,
                                    ext_bg,
                                    0,
                                    false,
                                );
                            }
                        }
                        reorder_row_bidi(
                            frame_glyphs,
                            row_glyph_start,
                            frame_glyphs.glyphs.len(),
                            content_x,
                        );
                        col = 0;
                        x_offset = 0.0;
                        row += 1;
                        row_glyph_start = frame_glyphs.glyphs.len();
                        if row >= max_rows {
                            break;
                        }
                        continue;
                    }
                    if ach == '\r' {
                        continue;
                    }
                    let achar_cols = if is_wide_char(ach) { 2 } else { 1 };
                    let a_advance = achar_cols as f32 * char_w;
                    if x_offset + a_advance > avail_width {
                        if params.truncate_lines {
                            reorder_row_bidi(
                                frame_glyphs,
                                row_glyph_start,
                                frame_glyphs.glyphs.len(),
                                content_x,
                            );
                            while ai < astr.len() {
                                let (sc, sl) = decode_utf8(&astr[ai..]);
                                ai += sl;
                                if sc == '\n' {
                                    col = 0;
                                    x_offset = 0.0;
                                    row += 1;
                                    row_glyph_start = frame_glyphs.glyphs.len();
                                    break;
                                }
                            }
                            if row >= max_rows {
                                break;
                            }
                            continue;
                        }
                        reorder_row_bidi(
                            frame_glyphs,
                            row_glyph_start,
                            frame_glyphs.glyphs.len(),
                            content_x,
                        );
                        col = 0;
                        x_offset = 0.0;
                        row += 1;
                        row_glyph_start = frame_glyphs.glyphs.len();
                        if row >= max_rows {
                            break;
                        }
                    }
                    let gx = content_x + x_offset;
                    let gy = row_y[row as usize];
                    frame_glyphs.add_char(ach, gx, gy, a_advance, char_h, ascent, false);
                    col += achar_cols;
                    x_offset += a_advance;
                }
                if (eob_after_has_runs || overlay_after_face.face_id != 0) && current_face_id >= 0 {
                    self.apply_face(&self.face_data, frame, frame_glyphs);
                }
            }
        }

        // Flush any remaining ligature run and bidi reorder the last row
        flush_run(&self.run_buf, frame_glyphs, ligatures);
        self.run_buf.clear();
        reorder_row_bidi(
            frame_glyphs,
            row_glyph_start,
            frame_glyphs.glyphs.len(),
            content_x,
        );

        // Fill rest of last line with :extend background if applicable
        // (handles end-of-buffer without trailing newline)
        if row < max_rows && x_offset > 0.0 {
            let remaining = avail_width - x_offset;
            if remaining > 0.0 {
                let gx = content_x + x_offset;
                let gy = row_y[row as usize];
                let (fill_bg, fill_face) = if self.face_data.extend != 0 {
                    (face_bg, self.face_data.face_id)
                } else if let Some((ext_bg, ext_face)) =
                    row_extend_bg.filter(|_| row_extend_row == row as i32)
                {
                    (ext_bg, ext_face)
                } else {
                    (default_bg, 0)
                };
                frame_glyphs.add_stretch(
                    gx,
                    gy,
                    remaining,
                    row_max_height,
                    fill_bg,
                    fill_face,
                    false,
                );
            }
        }

        // If cursor wasn't placed (point is past visible content), place at end
        if !cursor_placed && params.point >= window_start {
            let clamped_row = row.min(max_rows - 1);
            let cursor_y = row_y[clamped_row as usize];

            // Safety: don't place cursor past text area
            if cursor_y < text_y_limit {
                cursor_col = col;
                cursor_row = clamped_row;
                cursor_x = x_offset;
                let cursor_px = content_x + x_offset;

                let cursor_face_w = if self.face_data.font_char_width > 0.0 {
                    self.face_data.font_char_width
                } else {
                    char_w
                };

                let cursor_style = cursor_style_for_window(params);

                if let Some(style) = cursor_style {
                    let cursor_w =
                        cursor_width_for_style(style, text, byte_idx, col, params, cursor_face_w);
                    frame_glyphs.add_cursor(
                        params.window_id as i32,
                        cursor_px,
                        cursor_y,
                        cursor_w,
                        face_h,
                        style,
                        Color::from_pixel(params.cursor_color),
                    );

                    if matches!(style, CursorStyle::FilledBox) {
                        frame_glyphs.set_cursor_inverse(
                            cursor_px,
                            cursor_y,
                            cursor_w,
                            face_h,
                            Color::from_pixel(params.cursor_color),
                            default_bg,
                        );
                    }
                }
            }
            // If cursor_y >= text_y_limit, skip — forward scroll will fix next frame
        }

        // Fill remaining rows with default background
        let filled_rows = row + 1;
        if filled_rows < max_rows {
            let gy = row_y[filled_rows as usize];
            let remaining_h = (text_y + text_height) - row_y[filled_rows as usize];
            if remaining_h > 0.0 {
                frame_glyphs.add_stretch(text_x, gy, text_width, remaining_h, default_bg, 0, false);
            }
        }

        // Render fringe indicators
        let actual_rows = (row + 1).min(max_rows);
        if right_fringe_width > 0.0 || left_fringe_width > 0.0 {
            // Use default face for fringe rendering
            frame_glyphs.set_face(
                0,
                default_fg,
                Some(default_bg),
                400,
                false,
                0,
                None,
                0,
                None,
                0,
                None,
            );

            for r in 0..actual_rows as usize {
                let gy = row_y[r];
                if gy + char_h > text_y_limit {
                    break;
                }

                // Right fringe: continuation indicator for wrapped lines
                if right_fringe_width > 0.0 && row_continued.get(r).copied().unwrap_or(false) {
                    // Bitmap 7: left-curly-arrow (continuation)
                    render_fringe_bitmap(
                        7,
                        right_fringe_x,
                        gy,
                        right_fringe_width,
                        char_h,
                        default_fg,
                        frame_glyphs,
                    );
                }

                // Right fringe: truncation indicator
                if right_fringe_width > 0.0 && row_truncated.get(r).copied().unwrap_or(false) {
                    // Bitmap 4: right-arrow (truncation)
                    render_fringe_bitmap(
                        4,
                        right_fringe_x,
                        gy,
                        right_fringe_width,
                        char_h,
                        default_fg,
                        frame_glyphs,
                    );
                }

                // Left fringe: continuation indicator for continued lines
                if left_fringe_width > 0.0 && row_continuation.get(r).copied().unwrap_or(false) {
                    // Bitmap 8: right-curly-arrow (continuation from prev)
                    render_fringe_bitmap(
                        8,
                        left_fringe_x,
                        gy,
                        left_fringe_width,
                        char_h,
                        default_fg,
                        frame_glyphs,
                    );
                }

                // User-specified left-fringe display property bitmap
                if left_fringe_width > 0.0 {
                    if let Some(&(bid, fg, _bg)) = row_left_fringe.get(r) {
                        if bid > 0 {
                            let ffg = if fg != 0 {
                                Color::from_pixel(fg)
                            } else {
                                default_fg
                            };
                            render_fringe_bitmap(
                                bid,
                                left_fringe_x,
                                gy,
                                left_fringe_width,
                                char_h,
                                ffg,
                                frame_glyphs,
                            );
                        }
                    }
                }

                // User-specified right-fringe display property bitmap
                if right_fringe_width > 0.0 {
                    if let Some(&(bid, fg, _bg)) = row_right_fringe.get(r) {
                        if bid > 0 {
                            let ffg = if fg != 0 {
                                Color::from_pixel(fg)
                            } else {
                                default_fg
                            };
                            render_fringe_bitmap(
                                bid,
                                right_fringe_x,
                                gy,
                                right_fringe_width,
                                char_h,
                                ffg,
                                frame_glyphs,
                            );
                        }
                    }
                }
            }

            // EOB empty line indicators (bitmap 24 = empty_line)
            if params.indicate_empty_lines > 0 {
                let eob_start = actual_rows;
                for r in eob_start as usize..max_rows as usize {
                    let gy = row_y[r];
                    if gy + char_h > text_y_limit {
                        break;
                    }
                    if params.indicate_empty_lines == 2 {
                        // Right fringe
                        if right_fringe_width > 0.0 {
                            render_fringe_bitmap(
                                24,
                                right_fringe_x,
                                gy,
                                right_fringe_width,
                                char_h,
                                default_fg,
                                frame_glyphs,
                            );
                        }
                    } else {
                        // Left fringe (default)
                        if left_fringe_width > 0.0 {
                            render_fringe_bitmap(
                                24,
                                left_fringe_x,
                                gy,
                                left_fringe_width,
                                char_h,
                                default_fg,
                                frame_glyphs,
                            );
                        }
                    }
                }
            }
        }

        // Render fill-column indicator
        if params.fill_column_indicator > 0 {
            let fci_col = params.fill_column_indicator;
            let fci_char = params.fill_column_indicator_char;
            let fci_fg = Color::from_pixel(params.fill_column_indicator_fg);

            frame_glyphs.set_face(
                0,
                fci_fg,
                Some(default_bg),
                400,
                false,
                0,
                None,
                0,
                None,
                0,
                None,
            );

            // Draw indicator character at the fill column on each row
            if fci_col < cols {
                for r in 0..max_rows as usize {
                    let gx = content_x + fci_col as f32 * char_w;
                    let gy = row_y[r];
                    if gy + char_h > text_y_limit {
                        break;
                    }
                    frame_glyphs.add_char(fci_char, gx, gy, char_w, char_h, ascent, false);
                }
            }
        }

        // Render tab-line if this window has one
        if params.tab_line_height > 0.0 {
            self.render_status_line(
                params.bounds.x,
                params.bounds.y,
                params.bounds.width,
                params.tab_line_height,
                params.window_id,
                params.char_width,
                params.font_ascent,
                wp,
                frame,
                frame_glyphs,
                StatusLineKind::TabLine,
            );
        }

        // Render header-line if this window has one
        if params.header_line_height > 0.0 {
            self.render_status_line(
                params.bounds.x,
                params.bounds.y + params.tab_line_height,
                params.bounds.width,
                params.header_line_height,
                params.window_id,
                params.char_width,
                params.font_ascent,
                wp,
                frame,
                frame_glyphs,
                StatusLineKind::HeaderLine,
            );
        }

        // Render mode-line if this window has one
        if params.mode_line_height > 0.0 {
            self.render_status_line(
                params.bounds.x,
                params.bounds.y + params.bounds.height - params.mode_line_height,
                params.bounds.width,
                params.mode_line_height,
                params.window_id,
                params.char_width,
                params.font_ascent,
                wp,
                frame,
                frame_glyphs,
                StatusLineKind::ModeLine,
            );
        }

        // Record last hit-test row (end of visible text)
        if row < max_rows && (row as usize) < row_y.len() && charpos > hit_row_charpos_start {
            hit_rows.push(HitRow {
                y_start: row_y[row as usize],
                y_end: row_y[row as usize] + row_max_height,
                charpos_start: hit_row_charpos_start,
                charpos_end: charpos,
            });
        }

        // Store hit-test data for this window
        self.hit_data.push(WindowHitData {
            window_id: params.window_id,
            content_x,
            char_w,
            rows: hit_rows,
        });

        // Write layout results back to Emacs
        neomacs_layout_set_window_end(wp.window_ptr, window_end_charpos, row.min(max_rows - 1));

        // Set cursor position for Emacs (needed for recenter, scroll, etc.)
        // Ensure cursor_row is valid and within text area
        if cursor_row < max_rows && row_y[cursor_row as usize] < text_y_limit {
            neomacs_layout_set_cursor(
                wp.window_ptr,
                (content_x + cursor_x) as i32,
                (row_y[cursor_row as usize]) as i32,
                cursor_col,
                cursor_row,
            );
        } else {
            // Set cursor at row 0 as fallback — scroll will fix next frame
            neomacs_layout_set_cursor(wp.window_ptr, content_x as i32, text_y as i32, 0, 0);
        }
    }
}

/// Get the advance width for a character in a specific face.
///
/// Standalone function to avoid borrow conflicts with `LayoutEngine::text_buf`.
///
/// Supports two measurement backends:
/// - **C FFI** (default): Uses `neomacs_layout_fill_ascii_widths()` / `neomacs_layout_char_width()`
///   which read from Emacs C font metrics (fontconfig/freetype).
/// - **Cosmic-text**: Uses `FontMetricsService` for measurement, matching the render thread's
///   font resolution exactly. Eliminates width mismatches between layout and rendering.
///
/// The backend is selected by `font_metrics_svc` being Some (cosmic) or None (C FFI).
unsafe fn char_advance(
    ascii_width_cache: &mut std::collections::HashMap<(u32, i32), [f32; 128]>,
    font_metrics_svc: &mut Option<FontMetricsService>,
    ch: char,
    char_cols: i32,
    char_w: f32,
    face_id: u32,
    font_size: i32,
    face_char_w: f32,
    _window: EmacsWindow,
    font_family: &str,
    font_weight: u16,
    font_italic: bool,
) -> f32 {
    // Use the face-specific character width when available (handles
    // faces with :height attribute that use a differently-sized font).
    let face_w = if face_char_w > 0.0 {
        face_char_w
    } else {
        char_w
    };
    let min_grid_advance = char_cols as f32 * face_w;

    let svc = font_metrics_svc.get_or_insert_with(FontMetricsService::new);
    let font_size_f = if font_size > 0 {
        font_size as f32
    } else {
        face_w.max(1.0)
    };
    let cp = ch as u32;
    if cp < 128 {
        let cache_key = (face_id, font_size);
        if !ascii_width_cache.contains_key(&cache_key) {
            let mut widths =
                svc.fill_ascii_widths(font_family, font_weight, font_italic, font_size_f);
            for w in &mut widths {
                if *w <= 0.0 {
                    *w = face_w.max(min_grid_advance);
                }
            }
            ascii_width_cache.insert(cache_key, widths);
        }
        return ascii_width_cache[&cache_key][cp as usize];
    }

    let measured = svc.char_width(ch, font_family, font_weight, font_italic, font_size_f);
    if measured > 0.0 {
        measured
    } else {
        min_grid_advance
    }
}

/// Render a fringe bitmap at the given position using Border rects.
/// Queries the actual bitmap data from Emacs via FFI and draws
/// each set bit as a filled pixel rectangle.
unsafe fn render_fringe_bitmap(
    bitmap_id: i32,
    fringe_x: f32,
    row_y: f32,
    fringe_width: f32,
    row_height: f32,
    fg: Color,
    frame_glyphs: &mut FrameGlyphBuffer,
) {
    let mut bits = [0u16; 64]; // max 64 rows
    let mut bm_width: c_int = 0;
    let mut bm_height: c_int = 0;
    let mut bm_align: c_int = 0;

    let rows = neomacs_layout_get_fringe_bitmap(
        bitmap_id,
        bits.as_mut_ptr(),
        64,
        &mut bm_width,
        &mut bm_height,
        &mut bm_align,
    );

    if rows <= 0 || bm_width <= 0 {
        return;
    }

    let bm_w = bm_width as f32;
    let bm_h = rows as f32;

    // Emacs parity: fringe bitmap pixels are intrinsic (1 bitmap pixel = 1 screen px),
    // not scaled to row height or fringe width.
    let pixel_w = 1.0;
    let pixel_h = 1.0;
    let scaled_w = bm_w;
    let scaled_h = bm_h;

    // Center horizontally in fringe
    let x_start = fringe_x + (fringe_width - scaled_w) / 2.0;
    let x_end = fringe_x + fringe_width;

    // Vertical alignment within the row
    let y_start = match bm_align {
        1 => row_y,                                 // top
        2 => row_y + row_height - scaled_h,         // bottom
        _ => row_y + (row_height - scaled_h) / 2.0, // center (default)
    };

    // Render each row of the bitmap
    for r in 0..rows as usize {
        let row_bits = bits[r];
        if row_bits == 0 {
            continue;
        }

        let py = y_start + r as f32 * pixel_h;
        if py + pixel_h <= row_y || py >= row_y + row_height {
            continue; // skip rows outside visible area
        }

        // Scan for horizontal runs of consecutive set bits
        let mut bit = bm_width - 1; // MSB = leftmost pixel
        while bit >= 0 {
            if row_bits & (1 << bit) != 0 {
                // Start of a run
                let run_start = bit;
                while bit > 0 && row_bits & (1 << (bit - 1)) != 0 {
                    bit -= 1;
                }
                let run_end = bit;
                let run_len = (run_start - run_end + 1) as f32;
                let px = x_start + (bm_width - 1 - run_start) as f32 * pixel_w;
                let run_w = run_len * pixel_w;
                let clip_l = px.max(fringe_x);
                let clip_r = (px + run_w).min(x_end);
                let clip_w = clip_r - clip_l;
                if clip_w > 0.0 {
                    frame_glyphs.add_border(clip_l, py, clip_w, pixel_h, fg);
                }
            }
            bit -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neomacs_display_protocol::frame_glyphs::FrameGlyph;

    fn test_window_params() -> WindowParams {
        WindowParams {
            window_id: 1,
            buffer_id: 1,
            bounds: Rect::new(0.0, 0.0, 800.0, 600.0),
            text_bounds: Rect::new(0.0, 0.0, 800.0, 560.0),
            selected: true,
            is_minibuffer: false,
            window_start: 1,
            window_end: 0,
            point: 1,
            buffer_size: 1,
            buffer_begv: 1,
            hscroll: 0,
            vscroll: 0,
            truncate_lines: false,
            word_wrap: false,
            tab_width: 8,
            tab_stop_list: vec![],
            default_fg: 0xFFFFFF,
            default_bg: 0x000000,
            char_width: 8.0,
            char_height: 16.0,
            font_pixel_size: 14.0,
            font_ascent: 12.0,
            mode_line_height: 0.0,
            header_line_height: 0.0,
            tab_line_height: 0.0,
            cursor_type: 0,
            cursor_bar_width: 2,
            cursor_color: 0xFFFFFF,
            left_fringe_width: 0.0,
            right_fringe_width: 0.0,
            indicate_empty_lines: 0,
            show_trailing_whitespace: false,
            trailing_ws_bg: 0,
            fill_column_indicator: 0,
            fill_column_indicator_char: '|',
            fill_column_indicator_fg: 0,
            extra_line_spacing: 0.0,
            cursor_in_non_selected: false,
            selective_display: 0,
            escape_glyph_fg: 0,
            nobreak_char_display: 0,
            nobreak_char_fg: 0,
            glyphless_char_fg: 0,
            wrap_prefix: vec![],
            line_prefix: vec![],
            left_margin_width: 0.0,
            right_margin_width: 0.0,
        }
    }

    #[test]
    fn test_ligature_run_buffer_new() {
        let buf = LigatureRunBuffer::new();

        // All fields should be zeroed/empty
        assert_eq!(buf.chars.len(), 0);
        assert_eq!(buf.advances.len(), 0);
        assert_eq!(buf.start_x, 0.0);
        assert_eq!(buf.start_y, 0.0);
        assert_eq!(buf.face_h, 0.0);
        assert_eq!(buf.face_ascent, 0.0);
        assert_eq!(buf.face_id, 0);
        assert_eq!(buf.total_advance, 0.0);
        assert_eq!(buf.is_overlay, false);
        assert_eq!(buf.height_scale, 0.0);

        // Vectors should be pre-allocated
        assert!(buf.chars.capacity() >= MAX_LIGATURE_RUN_LEN);
        assert!(buf.advances.capacity() >= MAX_LIGATURE_RUN_LEN);
    }

    #[test]
    fn test_ligature_run_buffer_is_empty_len() {
        let mut buf = LigatureRunBuffer::new();

        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);

        buf.push('a', 8.0);

        assert!(!buf.is_empty());
        assert_eq!(buf.len(), 1);

        buf.push('b', 8.0);

        assert!(!buf.is_empty());
        assert_eq!(buf.len(), 2);
    }

    #[test]
    fn test_ligature_run_buffer_push() {
        let mut buf = LigatureRunBuffer::new();

        buf.push('h', 8.0);
        assert_eq!(buf.chars, vec!['h']);
        assert_eq!(buf.advances, vec![8.0]);
        assert_eq!(buf.total_advance, 8.0);

        buf.push('e', 8.0);
        assert_eq!(buf.chars, vec!['h', 'e']);
        assert_eq!(buf.advances, vec![8.0, 8.0]);
        assert_eq!(buf.total_advance, 16.0);

        buf.push('l', 7.5);
        assert_eq!(buf.chars, vec!['h', 'e', 'l']);
        assert_eq!(buf.advances, vec![8.0, 8.0, 7.5]);
        assert_eq!(buf.total_advance, 23.5);
    }

    #[test]
    fn test_ligature_run_buffer_clear() {
        let mut buf = LigatureRunBuffer::new();

        buf.push('a', 8.0);
        buf.push('b', 8.0);
        buf.start_x = 100.0;
        buf.start_y = 200.0;
        buf.face_h = 16.0;
        buf.face_ascent = 12.0;
        buf.face_id = 42;
        buf.is_overlay = true;
        buf.height_scale = 1.5;

        buf.clear();

        // Vectors and total_advance cleared
        assert_eq!(buf.chars.len(), 0);
        assert_eq!(buf.advances.len(), 0);
        assert_eq!(buf.total_advance, 0.0);

        // Position/face fields NOT cleared
        assert_eq!(buf.start_x, 100.0);
        assert_eq!(buf.start_y, 200.0);
        assert_eq!(buf.face_h, 16.0);
        assert_eq!(buf.face_ascent, 12.0);
        assert_eq!(buf.face_id, 42);
        assert_eq!(buf.is_overlay, true);
        assert_eq!(buf.height_scale, 1.5);
    }

    #[test]
    fn test_ligature_run_buffer_start() {
        let mut buf = LigatureRunBuffer::new();

        buf.push('x', 10.0);
        buf.start_x = 999.0;

        buf.start(50.0, 60.0, 20.0, 15.0, 5, true, 1.2);

        // Clears chars/advances/total_advance
        assert_eq!(buf.chars.len(), 0);
        assert_eq!(buf.advances.len(), 0);
        assert_eq!(buf.total_advance, 0.0);

        // Sets all position/face params
        assert_eq!(buf.start_x, 50.0);
        assert_eq!(buf.start_y, 60.0);
        assert_eq!(buf.face_h, 20.0);
        assert_eq!(buf.face_ascent, 15.0);
        assert_eq!(buf.face_id, 5);
        assert_eq!(buf.is_overlay, true);
        assert_eq!(buf.height_scale, 1.2);
    }

    #[test]
    fn test_max_ligature_run_len_constant() {
        assert_eq!(MAX_LIGATURE_RUN_LEN, 64);
    }

    #[test]
    fn test_flush_run_empty() {
        let run = LigatureRunBuffer::new();
        let mut frame_glyphs = FrameGlyphBuffer::new();

        flush_run(&run, &mut frame_glyphs, true);

        // No glyphs added
        assert_eq!(frame_glyphs.glyphs.len(), 0);
    }

    #[test]
    fn test_flush_run_single_char_ligatures_true() {
        let mut run = LigatureRunBuffer::new();
        run.start(10.0, 20.0, 16.0, 12.0, 1, false, 0.0);
        run.push('a', 8.0);

        let mut frame_glyphs = FrameGlyphBuffer::new();
        flush_run(&run, &mut frame_glyphs, true);

        // Single char emits as individual char, not composed
        assert_eq!(frame_glyphs.glyphs.len(), 1);
        match &frame_glyphs.glyphs[0] {
            FrameGlyph::Char {
                char: ch,
                composed,
                x,
                y,
                width,
                height,
                ascent,
                ..
            } => {
                assert_eq!(*ch, 'a');
                assert_eq!(*composed, None);
                assert_eq!(*x, 10.0);
                assert_eq!(*y, 20.0);
                assert_eq!(*width, 8.0);
                assert_eq!(*height, 16.0);
                assert_eq!(*ascent, 12.0);
            }
            _ => panic!("Expected Char glyph"),
        }
    }

    #[test]
    fn test_flush_run_single_char_ligatures_false() {
        let mut run = LigatureRunBuffer::new();
        run.start(100.0, 200.0, 18.0, 14.0, 2, true, 0.0);
        run.push('x', 9.0);

        let mut frame_glyphs = FrameGlyphBuffer::new();
        flush_run(&run, &mut frame_glyphs, false);

        // Single char emits as individual char
        assert_eq!(frame_glyphs.glyphs.len(), 1);
        match &frame_glyphs.glyphs[0] {
            FrameGlyph::Char {
                char: ch,
                composed,
                x,
                y,
                width,
                ..
            } => {
                assert_eq!(*ch, 'x');
                assert_eq!(*composed, None);
                assert_eq!(*x, 100.0);
                assert_eq!(*y, 200.0);
                assert_eq!(*width, 9.0);
            }
            _ => panic!("Expected Char glyph"),
        }
    }

    #[test]
    fn test_flush_run_multiple_chars_ligatures_false() {
        let mut run = LigatureRunBuffer::new();
        run.start(50.0, 60.0, 16.0, 12.0, 1, false, 0.0);
        run.push('f', 6.0);
        run.push('i', 4.0);
        run.push('j', 4.0);

        let mut frame_glyphs = FrameGlyphBuffer::new();
        flush_run(&run, &mut frame_glyphs, false);

        // Emits individual chars with correct x positions
        assert_eq!(frame_glyphs.glyphs.len(), 3);

        match &frame_glyphs.glyphs[0] {
            FrameGlyph::Char {
                char: ch, x, width, ..
            } => {
                assert_eq!(*ch, 'f');
                assert_eq!(*x, 50.0);
                assert_eq!(*width, 6.0);
            }
            _ => panic!("Expected Char glyph"),
        }

        match &frame_glyphs.glyphs[1] {
            FrameGlyph::Char {
                char: ch, x, width, ..
            } => {
                assert_eq!(*ch, 'i');
                assert_eq!(*x, 56.0); // 50.0 + 6.0
                assert_eq!(*width, 4.0);
            }
            _ => panic!("Expected Char glyph"),
        }

        match &frame_glyphs.glyphs[2] {
            FrameGlyph::Char {
                char: ch, x, width, ..
            } => {
                assert_eq!(*ch, 'j');
                assert_eq!(*x, 60.0); // 56.0 + 4.0
                assert_eq!(*width, 4.0);
            }
            _ => panic!("Expected Char glyph"),
        }
    }

    #[test]
    fn test_flush_run_multiple_chars_ligatures_true() {
        // Use ligature-eligible chars (pure symbol run)
        let mut run = LigatureRunBuffer::new();
        run.start(10.0, 20.0, 16.0, 12.0, 1, false, 0.0);
        run.push('-', 6.0);
        run.push('>', 4.0);

        let mut frame_glyphs = FrameGlyphBuffer::new();
        flush_run(&run, &mut frame_glyphs, true);

        // Emits as composed glyph
        assert_eq!(frame_glyphs.glyphs.len(), 1);

        match &frame_glyphs.glyphs[0] {
            FrameGlyph::Char {
                char: ch,
                composed,
                x,
                y,
                width,
                height,
                ascent,
                ..
            } => {
                assert_eq!(*ch, '-'); // base char
                assert_eq!(composed.as_ref().map(|s: &Box<str>| s.as_ref()), Some("->"));
                assert_eq!(*x, 10.0);
                assert_eq!(*y, 20.0);
                assert_eq!(*width, 10.0); // total_advance = 6.0 + 4.0
                assert_eq!(*height, 16.0);
                assert_eq!(*ascent, 12.0);
            }
            _ => panic!("Expected Char glyph"),
        }
    }

    #[test]
    fn test_flush_run_mixed_alpha_symbol_not_composed() {
        // Mixed alphanumeric+symbol runs should NOT compose (e.g., "arrow:")
        let mut run = LigatureRunBuffer::new();
        run.start(0.0, 0.0, 16.0, 12.0, 1, false, 0.0);
        run.push('f', 6.0);
        run.push('i', 4.0);

        let mut frame_glyphs = FrameGlyphBuffer::new();
        flush_run(&run, &mut frame_glyphs, true);

        // Should emit as individual chars, not composed
        assert_eq!(frame_glyphs.glyphs.len(), 2);
    }

    #[test]
    fn test_flush_run_height_scale_individual() {
        let mut run = LigatureRunBuffer::new();
        run.start(0.0, 0.0, 16.0, 12.0, 1, false, 1.5);
        run.push('a', 8.0);

        let mut frame_glyphs = FrameGlyphBuffer::new();
        frame_glyphs.set_font_size(14.0);

        flush_run(&run, &mut frame_glyphs, false);

        // Font size should be restored after flush
        assert_eq!(frame_glyphs.font_size(), 14.0);

        // Glyph should exist
        assert_eq!(frame_glyphs.glyphs.len(), 1);
    }

    #[test]
    fn test_flush_run_height_scale_composed() {
        // Use ligature-eligible chars for composed path
        let mut run = LigatureRunBuffer::new();
        run.start(0.0, 0.0, 16.0, 12.0, 1, false, 2.0);
        run.push('=', 6.0);
        run.push('>', 4.0);

        let mut frame_glyphs = FrameGlyphBuffer::new();
        frame_glyphs.set_font_size(14.0);

        flush_run(&run, &mut frame_glyphs, true);

        // Font size should be restored after flush
        assert_eq!(frame_glyphs.font_size(), 14.0);

        // Composed glyph should exist
        assert_eq!(frame_glyphs.glyphs.len(), 1);
    }

    #[test]
    fn test_is_ligature_char() {
        // Ligature-eligible characters
        for ch in [
            '-', '>', '<', '=', '!', '|', '&', '*', '+', '.', '/', ':', ';', '?', '@', '\\', '^',
            '~', '#', '$', '%',
        ] {
            assert!(is_ligature_char(ch), "'{}' should be a ligature char", ch);
        }
        // Non-ligature characters
        for ch in [
            'a', 'Z', '0', '9', ' ', '\n', '\t', '(', ')', '[', ']', '{', '}', ',', '\'', '"',
        ] {
            assert!(
                !is_ligature_char(ch),
                "'{}' should NOT be a ligature char",
                ch
            );
        }
    }

    #[test]
    fn test_run_is_pure_ligature() {
        // Pure symbol run
        let mut run = LigatureRunBuffer::new();
        run.start(0.0, 0.0, 16.0, 12.0, 1, false, 0.0);
        run.push('-', 8.0);
        run.push('>', 8.0);
        assert!(run_is_pure_ligature(&run));

        // Mixed run (alpha + symbol)
        let mut run2 = LigatureRunBuffer::new();
        run2.start(0.0, 0.0, 16.0, 12.0, 1, false, 0.0);
        run2.push('a', 8.0);
        run2.push(':', 8.0);
        assert!(!run_is_pure_ligature(&run2));

        // Pure alpha run
        let mut run3 = LigatureRunBuffer::new();
        run3.start(0.0, 0.0, 16.0, 12.0, 1, false, 0.0);
        run3.push('h', 8.0);
        run3.push('i', 8.0);
        assert!(!run_is_pure_ligature(&run3));
    }

    #[test]
    fn test_cursor_point_columns_wide_char() {
        let params = test_window_params();
        let text = "你".as_bytes();
        assert_eq!(cursor_point_columns(text, 0, 0, &params), 2);
    }

    #[test]
    fn test_cursor_point_columns_tab_uses_tab_stop_list() {
        let mut params = test_window_params();
        params.tab_width = 8;
        params.tab_stop_list = vec![4, 10];
        let text = b"\t";

        assert_eq!(cursor_point_columns(text, 0, 3, &params), 1);
        assert_eq!(cursor_point_columns(text, 0, 4, &params), 6);
    }

    #[test]
    fn test_cursor_width_for_style_bar_uses_bar_width() {
        let params = test_window_params();
        let text = "你".as_bytes();

        let width = cursor_width_for_style(CursorStyle::Bar(2.5), text, 0, 0, &params, 7.0);
        assert_eq!(width, 2.5);
    }

    #[test]
    fn test_cursor_width_for_style_hbar_uses_glyph_columns() {
        let params = test_window_params();
        let text = "你".as_bytes();

        let width = cursor_width_for_style(CursorStyle::Hbar(2.0), text, 0, 0, &params, 7.0);
        assert_eq!(width, 14.0);
    }
}
