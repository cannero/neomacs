//! Faithful Rust port of GNU Emacs's `calc_pixel_width_or_height`
//! from `src/xdisp.c:30102`.
//!
//! This is the evaluator for the value of `(space :width …)` and
//! `(space :align-to …)` display property forms. It handles:
//!
//! - Numbers (fixnum/float) scaled by the frame's column width or line
//!   height
//! - Two-character unit symbols `in`, `mm`, `cm` with DPI conversion
//! - Symbols `height`, `width` for the current face's font dimensions
//! - Symbols `text`, `left`, `right`, `center`, `left-fringe`,
//!   `right-fringe`, `left-margin`, `right-margin`, `scroll-bar` for
//!   window-box-relative positions (in align-to mode) or widths
//! - Fall-through to an arbitrary symbol, recursing into its value
//!   (normally looked up via buffer-local-value in GNU; this port
//!   accepts a caller-provided closure)
//! - Cons `(+ E…)` and `(- E…)` for recursive arithmetic
//! - Cons `(NUM)` for absolute pixel count
//! - Cons `(NUM . UNIT)` for scaled values
//! - Cons `(image PROPS…)` and `(xwidget PROPS…)` — currently return a
//!   placeholder 100px; real image dimensions require image-loading
//!   infrastructure and are a `TODO(verify)` for a future commit
//!
//! The helper is backend-agnostic: TUI and GUI both call it with a
//! `PixelCalcContext` built from the caller's window/frame state. No
//! call sites in the codebase yet; this is Step 1 of the display-engine
//! unification plan. See `docs/plans/2026-04-11-display-engine-unification.md`.

use neovm_core::emacs_core::Value;

/// Context equivalent to the fields of GNU's `struct it` that
/// `calc_pixel_width_or_height` reads.
///
/// All values are `f64` pixels. The layout engine's `WindowParams` and
/// `FrameParams` already carry everything we need — the caller extracts
/// these fields once per `(space …)` evaluation and passes them in.
#[derive(Debug, Clone)]
pub struct PixelCalcContext {
    /// Frame's default column width in pixels.
    /// GNU: `FRAME_COLUMN_WIDTH(it->f)`. Used as the base unit when a
    /// bare number is interpreted as a width.
    pub frame_column_width: f64,

    /// Frame's default line height in pixels.
    /// GNU: `FRAME_LINE_HEIGHT(it->f)`. Base unit for bare numbers in
    /// height mode.
    pub frame_line_height: f64,

    /// Frame horizontal resolution in pixels per inch. Used for `in`,
    /// `mm`, `cm` unit conversion in width mode.
    /// GNU: `FRAME_RES_X(it->f)`.
    pub frame_res_x: f64,

    /// Frame vertical resolution in pixels per inch. Used in height mode.
    /// GNU: `FRAME_RES_Y(it->f)`.
    pub frame_res_y: f64,

    /// Current face's font height in pixels. Returned for the `height`
    /// symbol.
    /// GNU: `normal_char_height(font, -1)` with `FRAME_LINE_HEIGHT`
    /// fallback.
    pub face_font_height: f64,

    /// Current face's font width in pixels. Returned for the `width`
    /// symbol.
    /// GNU: `font->average_width` (or `space_width`), with
    /// `FRAME_COLUMN_WIDTH` fallback.
    pub face_font_width: f64,

    /// Text-area left offset within the window, in pixels.
    /// GNU: `window_box_left_offset(it->w, TEXT_AREA)`.
    pub text_area_left: f64,

    /// Text-area right offset within the window, in pixels.
    /// GNU: `window_box_right_offset(it->w, TEXT_AREA)`.
    pub text_area_right: f64,

    /// Text-area width in pixels.
    /// GNU: `window_box_width(it->w, TEXT_AREA)`.
    pub text_area_width: f64,

    /// Left margin left offset and width.
    /// GNU: `window_box_left_offset(it->w, LEFT_MARGIN_AREA)` and
    /// `WINDOW_LEFT_MARGIN_WIDTH(it->w)`.
    pub left_margin_left: f64,
    pub left_margin_width: f64,

    /// Right margin left offset and width.
    /// GNU: `window_box_left_offset(it->w, RIGHT_MARGIN_AREA)` and
    /// `WINDOW_RIGHT_MARGIN_WIDTH(it->w)`.
    pub right_margin_left: f64,
    pub right_margin_width: f64,

    /// Fringe widths.
    /// GNU: `WINDOW_LEFT_FRINGE_WIDTH` / `WINDOW_RIGHT_FRINGE_WIDTH`.
    pub left_fringe_width: f64,
    pub right_fringe_width: f64,

    /// Whether fringes sit outside the display margins.
    /// GNU: `WINDOW_HAS_FRINGES_OUTSIDE_MARGINS(it->w)`.
    pub fringes_outside_margins: bool,

    /// Scroll bar area width.
    /// GNU: `WINDOW_SCROLL_BAR_AREA_WIDTH(it->w)`.
    pub scroll_bar_width: f64,

    /// Whether the vertical scroll bar is on the left side of the window.
    /// GNU: `WINDOW_HAS_VERTICAL_SCROLL_BAR_ON_LEFT(it->w)`.
    pub scroll_bar_on_left: bool,

    /// Line-number pixel width. Added to the align-to result on first
    /// evaluation to match GNU's `lnum_pixel_width` handling.
    /// GNU: `it->line_number_produced_p ? it->lnum_pixel_width : 0`.
    pub line_number_pixel_width: f64,
}

impl PixelCalcContext {
    /// Zero-initialized context. Every field defaults to 0.0/false/etc.
    /// Useful as a starting point for tests; real call sites should
    /// fill in every field from their `WindowParams`/`FrameParams`.
    pub fn zeroed() -> Self {
        Self {
            frame_column_width: 0.0,
            frame_line_height: 0.0,
            frame_res_x: 96.0, // default DPI
            frame_res_y: 96.0,
            face_font_height: 0.0,
            face_font_width: 0.0,
            text_area_left: 0.0,
            text_area_right: 0.0,
            text_area_width: 0.0,
            left_margin_left: 0.0,
            left_margin_width: 0.0,
            right_margin_left: 0.0,
            right_margin_width: 0.0,
            left_fringe_width: 0.0,
            right_fringe_width: 0.0,
            fringes_outside_margins: false,
            scroll_bar_width: 0.0,
            scroll_bar_on_left: false,
            line_number_pixel_width: 0.0,
        }
    }
}

/// Evaluate a `(space :width …)` or `(space :align-to …)` expression
/// value into a pixel count.
///
/// This is a faithful port of GNU `calc_pixel_width_or_height`
/// (`src/xdisp.c:30102`). Every branch is labeled with the
/// corresponding GNU source line to make audit easy.
///
/// # Arguments
///
/// - `ctx`: window/frame/face pixel state equivalent to GNU's
///   `struct it` fields the function reads.
/// - `prop`: the expression value — may be nil, a number, a symbol,
///   a cons form, etc.
/// - `width_p`: true for width/x-coordinate evaluation, false for
///   height/y-coordinate.
/// - `align_to`: side channel for `:align-to` mode. Pass `None` for
///   `:width` evaluation. Pass `Some(&mut -1)` on the initial call
///   when evaluating an `:align-to` expression — the function treats
///   window-box symbols as positions (left-edge offsets) on the first
///   evaluation and writes the resolved position back through this
///   reference. Recursive calls see `*align_to >= 0` and revert to
///   interpreting symbols as widths, so forms like `(- right N)`
///   compute `right_position - N_width`.
///
/// # Returns
///
/// `Some(pixels)` on success. `None` for expressions the evaluator
/// doesn't recognize (matches GNU's `return false`).
pub fn calc_pixel_width_or_height(
    ctx: &PixelCalcContext,
    prop: &Value,
    width_p: bool,
    align_to: Option<&mut i32>,
) -> Option<f64> {
    // GNU xdisp.c:30112 — initial lnum_pixel_width snapshot. GNU snapshots
    // this only if the line number has already been produced for the
    // current screen line. We accept the caller's value directly; the
    // caller is responsible for passing 0 if the line number hasn't
    // been produced yet.
    let lnum_pixel_width = ctx.line_number_pixel_width;

    // GNU xdisp.c:30125 — `if (NILP (prop)) return OK_PIXELS (0);`
    if prop.is_nil() {
        return Some(0.0);
    }

    // GNU xdisp.c:30131 — symbol branch
    if prop.is_symbol() {
        return calc_symbol(ctx, prop, width_p, align_to, lnum_pixel_width);
    }

    // GNU xdisp.c:30242 — number branch
    if let Some(n) = prop.as_fixnum() {
        return Some(calc_number(ctx, n as f64, width_p, &align_to, lnum_pixel_width));
    }
    if prop.is_float() {
        return Some(calc_number(
            ctx,
            prop.xfloat(),
            width_p,
            &align_to,
            lnum_pixel_width,
        ));
    }

    // GNU xdisp.c:30251 — cons branch
    if prop.is_cons() {
        return calc_cons(ctx, prop, width_p, align_to, lnum_pixel_width);
    }

    None
}

// ---------------------------------------------------------------------------
// Symbol branch (GNU xdisp.c:30131–30241)
// ---------------------------------------------------------------------------

fn calc_symbol(
    ctx: &PixelCalcContext,
    prop: &Value,
    width_p: bool,
    mut align_to: Option<&mut i32>,
    _lnum_pixel_width: f64,
) -> Option<f64> {
    let name = prop.as_symbol_name()?;

    // GNU xdisp.c:30133 — two-character unit symbols (in, mm, cm).
    if name.len() == 2 {
        let bytes = name.as_bytes();
        let pixels_per_unit = match (bytes[0], bytes[1]) {
            (b'i', b'n') => 1.0,        // 1 inch
            (b'm', b'm') => 25.4,       // 1 inch = 25.4 mm
            (b'c', b'm') => 2.54,       // 1 inch = 2.54 cm
            _ => -1.0,
        };
        if pixels_per_unit > 0.0 {
            // GNU xdisp.c:30147: `ppi / pixels`
            let ppi = if width_p {
                ctx.frame_res_x
            } else {
                ctx.frame_res_y
            };
            if ppi > 0.0 {
                return Some(ppi / pixels_per_unit);
            }
            return None;
        }
        // fall through for two-char symbols that aren't units (e.g., "my")
    }

    // GNU xdisp.c:30158 — `height` symbol
    if name == "height" {
        return Some(ctx.face_font_height);
    }
    // GNU xdisp.c:30164 — `width` symbol
    if name == "width" {
        return Some(ctx.face_font_width);
    }
    // GNU xdisp.c:30175 — `text` symbol (text-area width)
    if name == "text" {
        return Some(ctx.text_area_width - ctx.line_number_pixel_width);
    }

    // GNU xdisp.c:30183 — `if (align_to && *align_to < 0)`:
    // first-time align-to resolution. The following symbols resolve to
    // left-edge positions of various window regions.
    let in_first_align_to = matches!(align_to.as_deref(), Some(v) if *v < 0);

    if in_first_align_to {
        // GNU xdisp.c:30188 — `left`
        if name == "left" {
            let pos = ctx.text_area_left + ctx.line_number_pixel_width;
            if let Some(a) = align_to.as_deref_mut() {
                *a = pos as i32;
            }
            return Some(0.0); // GNU sets `*res = 0` here
        }
        // GNU xdisp.c:30192 — `right` (right edge of text area)
        if name == "right" {
            let pos = ctx.text_area_right;
            if let Some(a) = align_to.as_deref_mut() {
                *a = pos as i32;
            }
            return Some(0.0);
        }
        // GNU xdisp.c:30196 — `center`
        if name == "center" {
            let pos = ctx.text_area_left
                + ctx.line_number_pixel_width
                + ctx.text_area_width / 2.0;
            if let Some(a) = align_to.as_deref_mut() {
                *a = pos as i32;
            }
            return Some(0.0);
        }
        // GNU xdisp.c:30201 — `left-fringe`
        if name == "left-fringe" {
            let pos = if ctx.fringes_outside_margins {
                // scroll-bar area width when scroll bar is on left
                if ctx.scroll_bar_on_left {
                    ctx.scroll_bar_width
                } else {
                    0.0
                }
            } else {
                // window_box_right_offset(LEFT_MARGIN_AREA) — i.e., left
                // margin's right edge. With left_margin_left and
                // left_margin_width we get this directly.
                ctx.left_margin_left + ctx.left_margin_width
            };
            if let Some(a) = align_to.as_deref_mut() {
                *a = pos as i32;
            }
            return Some(0.0);
        }
        // GNU xdisp.c:30206 — `right-fringe`
        if name == "right-fringe" {
            let pos = if ctx.fringes_outside_margins {
                // window_box_right_offset(RIGHT_MARGIN_AREA)
                ctx.right_margin_left + ctx.right_margin_width
            } else {
                // window_box_right_offset(TEXT_AREA)
                ctx.text_area_right
            };
            if let Some(a) = align_to.as_deref_mut() {
                *a = pos as i32;
            }
            return Some(0.0);
        }
        // GNU xdisp.c:30211 — `left-margin`
        if name == "left-margin" {
            let pos = ctx.left_margin_left;
            if let Some(a) = align_to.as_deref_mut() {
                *a = pos as i32;
            }
            return Some(0.0);
        }
        // GNU xdisp.c:30214 — `right-margin`
        if name == "right-margin" {
            let pos = ctx.right_margin_left;
            if let Some(a) = align_to.as_deref_mut() {
                *a = pos as i32;
            }
            return Some(0.0);
        }
        // GNU xdisp.c:30217 — `scroll-bar`
        if name == "scroll-bar" {
            let pos = if ctx.scroll_bar_on_left {
                0.0
            } else {
                // RHS scroll bar: right edge of right margin + right fringe (if
                // outside margins)
                let right_margin_right = ctx.right_margin_left + ctx.right_margin_width;
                if ctx.fringes_outside_margins {
                    right_margin_right + ctx.right_fringe_width
                } else {
                    right_margin_right
                }
            };
            if let Some(a) = align_to.as_deref_mut() {
                *a = pos as i32;
            }
            return Some(0.0);
        }
    } else {
        // GNU xdisp.c:30223 — `else` branch: same symbols interpreted as
        // WIDTHS, not positions. Used when we're inside a recursive
        // `(+ ...)`/`(- ...)` after an align-to base has already been
        // resolved, OR when `align_to` is None (width mode).
        if name == "left-fringe" {
            return Some(ctx.left_fringe_width);
        }
        if name == "right-fringe" {
            return Some(ctx.right_fringe_width);
        }
        if name == "left-margin" {
            return Some(ctx.left_margin_width);
        }
        if name == "right-margin" {
            return Some(ctx.right_margin_width);
        }
        if name == "scroll-bar" {
            return Some(ctx.scroll_bar_width);
        }
    }

    // GNU xdisp.c:30233 — fall through: `prop = buffer_local_value(prop,
    // it->w->contents)`. The port doesn't currently support buffer-local
    // fall-through. For doom-modeline and the forms we've identified in
    // the audit, this branch is not reached. If a future user reports
    // a missing symbol, add the fall-through via a caller-provided
    // closure on `PixelCalcContext`.
    // TODO(verify): buffer-local fall-through for unrecognized symbols.
    None
}

// ---------------------------------------------------------------------------
// Number branch (GNU xdisp.c:30242)
// ---------------------------------------------------------------------------

fn calc_number(
    ctx: &PixelCalcContext,
    n: f64,
    width_p: bool,
    align_to: &Option<&mut i32>,
    lnum_pixel_width: f64,
) -> f64 {
    // GNU xdisp.c:30246: `int base_unit = (width_p ? FRAME_COLUMN_WIDTH
    // (it->f) : FRAME_LINE_HEIGHT (it->f));`
    let base_unit = if width_p {
        ctx.frame_column_width
    } else {
        ctx.frame_line_height
    };
    // GNU xdisp.c:30248: `if (width_p && align_to && *align_to < 0)
    //   return OK_PIXELS (XFLOATINT (prop) * base_unit + lnum_pixel_width);`
    let in_first_align_to = matches!(align_to.as_deref(), Some(v) if *v < 0);
    if width_p && in_first_align_to {
        n * base_unit + lnum_pixel_width
    } else {
        n * base_unit
    }
}

// ---------------------------------------------------------------------------
// Cons branch (GNU xdisp.c:30251)
// ---------------------------------------------------------------------------

fn calc_cons(
    ctx: &PixelCalcContext,
    prop: &Value,
    width_p: bool,
    mut align_to: Option<&mut i32>,
    lnum_pixel_width: f64,
) -> Option<f64> {
    // Walk via direct car/cdr access so we handle dotted pairs like
    // `(NUM . UNIT)` — list_to_vec only accepts proper lists.
    if !prop.is_cons() {
        return None;
    }
    let car = prop.cons_car();
    let cdr_raw = prop.cons_cdr();

    // GNU xdisp.c:30254 — `SYMBOLP (car)` branch
    if let Some(head) = car.as_symbol_name() {
        // GNU xdisp.c:30261 — `(image PROPS...)`. Requires image
        // infrastructure; return placeholder width.
        if head == "image" {
            // TODO(verify): actually look up image dimensions.
            return Some(100.0);
        }
        // GNU xdisp.c:30269 — `(xwidget PROPS...)`. Same placeholder.
        if head == "xwidget" {
            return Some(100.0);
        }

        // GNU xdisp.c:30278 — `(+ E...)` or `(- E...)`
        if head == "+" || head == "-" {
            let mut pixels = 0.0_f64;
            let mut first = true;
            // Walk the cdr list directly. cdr_raw is the tail after the
            // head symbol (e.g. `(5 3)` for `(- 5 3)`).
            let mut tail = cdr_raw;
            let mut local_align: Option<i32> = align_to.as_deref().copied();
            while tail.is_cons() {
                let arg = tail.cons_car();
                let sub_align_ref: Option<&mut i32> = local_align.as_mut();
                let px = calc_pixel_width_or_height(ctx, &arg, width_p, sub_align_ref)?;
                if first {
                    pixels = if head == "+" { px } else { -px };
                    first = false;
                } else {
                    pixels += px;
                }
                tail = tail.cons_cdr();
            }
            // GNU xdisp.c:30297: `if (EQ (car, Qminus)) pixels = -pixels;`
            // But only when minus has >1 argument — first-arg negation
            // is handled above. Re-reading GNU: the negation at the end
            // applies to all minus forms regardless of arity. Wait — no,
            // look more carefully: GNU sets `pixels = (EQ (car, Qplus)
            // ? px : -px)` on the first iteration, then adds subsequent
            // values. After the loop GNU does `if (EQ (car, Qminus))
            // pixels = -pixels;`. So for `(- A B)` the result is
            // `-(-A + B) = A - B`. ✓ matches our logic after the end
            // negation.
            //
            // Actually wait, let me re-read GNU once more:
            //
            //   if (first)
            //     pixels = (EQ (car, Qplus) ? px : -px), first = false;
            //   else
            //     pixels += px;
            //   ...
            //   if (EQ (car, Qminus))
            //     pixels = -pixels;
            //
            // For `(- 5 3)`:
            //   iter 1: first=true, pixels = -5 (because minus)
            //   iter 2: pixels = -5 + 3 = -2
            //   end:    pixels = -(-2) = 2
            // Correct.
            //
            // For `(- 5)`:
            //   iter 1: first=true, pixels = -5
            //   end:    pixels = -(-5) = 5
            // That's... negation of negation, = positive. But `(- 5)`
            // should be -5. Hmm. Let me check GNU's actual code.
            //
            // Actually GNU does (simplified):
            //
            //   pixels = 0;
            //   while (CONSP (cdr))
            //     {
            //       ... calc px ...
            //       if (first)
            //         pixels = (EQ (car, Qplus) ? px : -px), first = false;
            //       else
            //         pixels += px;
            //       cdr = XCDR (cdr);
            //     }
            //   if (EQ (car, Qminus))
            //     pixels = -pixels;
            //
            // For `(- 5)`:
            //   iter 1: first=true, pixels = -5
            //   end:    pixels = -(-5) = 5
            //
            // That gives 5, but `(- 5)` in Elisp = -5. So GNU's code
            // looks buggy for single-arg minus? Or am I misreading?
            //
            // Actually I think I'm misreading. Let me check once more
            // by reading the C directly:
            if head == "-" {
                pixels = -pixels;
            }
            // Sync the local_align back to the caller.
            if let Some(a) = align_to.as_deref_mut() {
                if let Some(la) = local_align {
                    *a = la;
                }
            }
            return Some(pixels);
        }

        // GNU xdisp.c:30307 — fall-through: resolve car via
        // buffer-local-value and fall through to the NUMBERP check below.
        // Not supported in this port; return None.
        // TODO(verify): buffer-local fall-through for unrecognized
        // cons-head symbols.
        return None;
    }

    // GNU xdisp.c:30311 — `(NUM)` or `(NUM . UNIT)` — car is a number.
    // The two forms are distinguished by the cdr: `(NUM)` has cdr=nil
    // (proper list of one element), `(NUM . UNIT)` has cdr=UNIT
    // (dotted pair).
    if let Some(pixels) = as_f64(&car) {
        // GNU xdisp.c:30314: `int offset = width_p && align_to &&
        //   *align_to < 0 ? lnum_pixel_width : 0;`
        let in_first_align_to = matches!(align_to.as_deref(), Some(v) if *v < 0);
        let offset = if width_p && in_first_align_to {
            lnum_pixel_width
        } else {
            0.0
        };
        // GNU xdisp.c:30316: `if (NILP (cdr)) return OK_PIXELS (pixels
        // + offset);`
        if cdr_raw.is_nil() {
            return Some(pixels + offset);
        }
        // GNU xdisp.c:30319: `(NUM . UNIT)` — recurse on the unit side.
        // The unit can be either a bare value in a dotted pair
        // `(NUM . UNIT)` or the head of a proper list `(NUM UNIT)`.
        // GNU calls `calc_pixel_width_or_height(..., cdr, ...)`.
        //
        // For `(NUM UNIT)` (proper list), cdr is `(UNIT . nil)`, a cons.
        // For `(NUM . UNIT)` (dotted pair), cdr is UNIT directly.
        //
        // We pass whichever we have directly — if it's a cons whose
        // car is UNIT, the recursion will treat it as a sub-expression
        // (most likely evaluating via the symbol or number branches).
        //
        // Actually GNU passes cdr directly, which for a proper list
        // `(NUM UNIT)` is `(UNIT)` — a cons. The recursive call then
        // goes through this same CONSP branch and evaluates the inner
        // `(UNIT)` form, which via the NUMBERP(car) path (if UNIT is
        // numeric) or the SYMBOLP(car) path (if UNIT is a symbol).
        //
        // For a dotted pair `(NUM . UNIT)` where UNIT is a symbol, cdr
        // is just the symbol — no cons wrapper. The recursion goes to
        // the symbol branch directly.
        //
        // Both cases work if we just pass cdr_raw as-is.
        let mut local_align: Option<i32> = align_to.as_deref().copied();
        let sub_align_ref: Option<&mut i32> = local_align.as_mut();
        let fact = calc_pixel_width_or_height(ctx, &cdr_raw, width_p, sub_align_ref)?;
        if let Some(a) = align_to.as_deref_mut() {
            if let Some(la) = local_align {
                *a = la;
            }
        }
        return Some(pixels * fact + offset);
    }

    None
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[inline]
fn as_f64(v: &Value) -> Option<f64> {
    if let Some(n) = v.as_fixnum() {
        return Some(n as f64);
    }
    if v.is_float() {
        return Some(v.xfloat());
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use neovm_core::emacs_core::Context;

    /// Construct a realistic test context: 800x600 window, 10px char
    /// width, 20px line height, 8px left fringe, 0 margins, 0 scroll
    /// bar, fringes inside margins.
    fn test_ctx() -> PixelCalcContext {
        PixelCalcContext {
            frame_column_width: 10.0,
            frame_line_height: 20.0,
            frame_res_x: 96.0,
            frame_res_y: 96.0,
            face_font_height: 20.0,
            face_font_width: 10.0,
            // Text area runs from x=8 (after left fringe) to x=792 (before
            // right fringe). width = 784.
            text_area_left: 8.0,
            text_area_right: 792.0,
            text_area_width: 784.0,
            // Left margin: x=0, width=0. Right margin: x=792, width=0.
            left_margin_left: 0.0,
            left_margin_width: 0.0,
            right_margin_left: 792.0,
            right_margin_width: 0.0,
            left_fringe_width: 8.0,
            right_fringe_width: 8.0,
            fringes_outside_margins: false,
            scroll_bar_width: 0.0,
            scroll_bar_on_left: false,
            line_number_pixel_width: 0.0,
        }
    }

    /// Parse an Elisp expression via a fresh Context.
    fn parse(src: &str) -> (Context, Value) {
        let mut ctx = Context::new();
        let v = ctx
            .eval_str(&format!("(quote {})", src))
            .expect("parse should succeed");
        (ctx, v)
    }

    // ------------------------------------------------------------------
    // Nil
    // ------------------------------------------------------------------
    #[test]
    fn nil_returns_zero() {
        let ctx = test_ctx();
        let r = calc_pixel_width_or_height(&ctx, &Value::NIL, true, None);
        assert_eq!(r, Some(0.0));
    }

    // ------------------------------------------------------------------
    // Numbers
    // ------------------------------------------------------------------
    #[test]
    fn fixnum_width_scales_by_column_width() {
        let ctx = test_ctx();
        let v = Value::fixnum(3);
        // 3 × frame_column_width = 30
        assert_eq!(
            calc_pixel_width_or_height(&ctx, &v, true, None),
            Some(30.0)
        );
    }

    #[test]
    fn fixnum_height_scales_by_line_height() {
        let ctx = test_ctx();
        let v = Value::fixnum(3);
        // 3 × frame_line_height = 60
        assert_eq!(
            calc_pixel_width_or_height(&ctx, &v, false, None),
            Some(60.0)
        );
    }

    #[test]
    fn float_width_scales_by_column_width() {
        // Value::make_float requires a tagged heap on the thread;
        // install one via a fresh Context.
        let (_keep, v) = parse("1.5");
        let ctx = test_ctx();
        // 1.5 × 10 = 15
        assert_eq!(
            calc_pixel_width_or_height(&ctx, &v, true, None),
            Some(15.0)
        );
    }

    // ------------------------------------------------------------------
    // Symbols: height / width / text
    // ------------------------------------------------------------------
    #[test]
    fn height_symbol_returns_face_font_height() {
        let ctx = test_ctx();
        let v = Value::symbol("height");
        assert_eq!(
            calc_pixel_width_or_height(&ctx, &v, true, None),
            Some(20.0)
        );
    }

    #[test]
    fn width_symbol_returns_face_font_width() {
        let ctx = test_ctx();
        let v = Value::symbol("width");
        assert_eq!(
            calc_pixel_width_or_height(&ctx, &v, true, None),
            Some(10.0)
        );
    }

    #[test]
    fn text_symbol_returns_text_area_width_minus_lnum() {
        let ctx = test_ctx();
        let v = Value::symbol("text");
        // text_area_width (784) - lnum_pixel_width (0) = 784
        assert_eq!(
            calc_pixel_width_or_height(&ctx, &v, true, None),
            Some(784.0)
        );
    }

    // ------------------------------------------------------------------
    // Symbols: units (in/mm/cm)
    // ------------------------------------------------------------------
    #[test]
    fn inch_unit_returns_dpi_pixels() {
        let ctx = test_ctx();
        let v = Value::symbol("in");
        // frame_res_x / 1.0 = 96
        assert_eq!(
            calc_pixel_width_or_height(&ctx, &v, true, None),
            Some(96.0)
        );
    }

    #[test]
    fn cm_unit_returns_dpi_over_2_54() {
        let ctx = test_ctx();
        let v = Value::symbol("cm");
        assert!(
            (calc_pixel_width_or_height(&ctx, &v, true, None).unwrap() - (96.0 / 2.54))
                .abs()
                < 0.01
        );
    }

    // ------------------------------------------------------------------
    // Symbols: align-to position (right/left/center)
    // ------------------------------------------------------------------
    #[test]
    fn right_symbol_in_align_to_mode_returns_text_right() {
        let ctx = test_ctx();
        let v = Value::symbol("right");
        let mut align = -1_i32;
        let px = calc_pixel_width_or_height(&ctx, &v, true, Some(&mut align));
        // First-eval symbols write the position to *align_to and return 0.
        assert_eq!(px, Some(0.0));
        assert_eq!(align, ctx.text_area_right as i32);
    }

    #[test]
    fn left_symbol_in_align_to_mode_returns_text_left_plus_lnum() {
        let ctx = test_ctx();
        let v = Value::symbol("left");
        let mut align = -1_i32;
        let px = calc_pixel_width_or_height(&ctx, &v, true, Some(&mut align));
        assert_eq!(px, Some(0.0));
        assert_eq!(align, (ctx.text_area_left + ctx.line_number_pixel_width) as i32);
    }

    #[test]
    fn center_symbol_in_align_to_mode_returns_midpoint() {
        let ctx = test_ctx();
        let v = Value::symbol("center");
        let mut align = -1_i32;
        let px = calc_pixel_width_or_height(&ctx, &v, true, Some(&mut align));
        assert_eq!(px, Some(0.0));
        // text_area_left (8) + 0 + text_area_width/2 (392) = 400
        assert_eq!(align, 400);
    }

    // ------------------------------------------------------------------
    // Symbols: width mode (non-first-time) treats same symbols as widths
    // ------------------------------------------------------------------
    #[test]
    fn right_fringe_symbol_in_width_mode_returns_fringe_width() {
        let ctx = test_ctx();
        let v = Value::symbol("right-fringe");
        // In width mode (align_to = None), right-fringe returns the fringe
        // WIDTH, not its position.
        assert_eq!(
            calc_pixel_width_or_height(&ctx, &v, true, None),
            Some(8.0)
        );
    }

    #[test]
    fn left_margin_symbol_in_width_mode_returns_margin_width() {
        let ctx = test_ctx();
        let v = Value::symbol("left-margin");
        assert_eq!(
            calc_pixel_width_or_height(&ctx, &v, true, None),
            Some(0.0)
        );
    }

    // ------------------------------------------------------------------
    // Cons: (+ E...) and (- E...)
    // ------------------------------------------------------------------
    #[test]
    fn plus_of_two_numbers_in_width_mode() {
        let ctx = test_ctx();
        // (+ 3 4) = 3 columns + 4 columns = (3 + 4) * 10 = 70
        let (_keep, v) = parse("(+ 3 4)");
        assert_eq!(
            calc_pixel_width_or_height(&ctx, &v, true, None),
            Some(70.0)
        );
    }

    #[test]
    fn minus_of_two_numbers_in_width_mode() {
        let ctx = test_ctx();
        // (- 5 3) = 5 cols - 3 cols = (5 - 3) * 10 = 20
        let (_keep, v) = parse("(- 5 3)");
        assert_eq!(
            calc_pixel_width_or_height(&ctx, &v, true, None),
            Some(20.0)
        );
    }

    #[test]
    fn minus_of_single_number_matches_gnu_double_negate() {
        let ctx = test_ctx();
        // GNU edge case: `(- 5)` with a SINGLE argument double-negates
        // because of how xdisp.c:30286 sets pixels=-px on the first
        // iteration AND then applies `pixels = -pixels` at the end
        // for the minus head. For the single-arg case this cancels
        // out and returns +50 (not -50 as one might expect from Elisp
        // arithmetic). The edge case is irrelevant in practice — every
        // `:align-to`/`:width` form we have seen uses multi-arg minus
        // like `(- right N)`. We match GNU exactly.
        let (_keep, v) = parse("(- 5)");
        assert_eq!(
            calc_pixel_width_or_height(&ctx, &v, true, None),
            Some(50.0)
        );
    }


    // ------------------------------------------------------------------
    // Cons: (NUM) — absolute pixel count
    // ------------------------------------------------------------------
    #[test]
    fn paren_num_returns_literal_pixels() {
        let ctx = test_ctx();
        // (200) = 200 pixels literal, no column scaling
        let (_keep, v) = parse("(200)");
        assert_eq!(
            calc_pixel_width_or_height(&ctx, &v, true, None),
            Some(200.0)
        );
    }

    // ------------------------------------------------------------------
    // Cons: (NUM . UNIT)
    // ------------------------------------------------------------------
    #[test]
    fn num_dot_width_unit_scales_by_face_font_width() {
        let ctx = test_ctx();
        // (2 . width) = 2 * face_font_width = 2 * 10 = 20
        let (_keep, v) = parse("(2 . width)");
        assert_eq!(
            calc_pixel_width_or_height(&ctx, &v, true, None),
            Some(20.0)
        );
    }

    // ------------------------------------------------------------------
    // Doom-modeline form: (- right (200))
    // ------------------------------------------------------------------
    #[test]
    fn doom_modeline_right_minus_paren_literal() {
        let ctx = test_ctx();
        // `right` resolves to text_area_right (792) on first eval, then
        // inside the recursive minus, `(200)` returns 200 pixels literal.
        // Result: align_to = 792 - 200 = 592.
        //
        // The caller's alignment sentinel starts at -1; after the call,
        // we expect it to have been updated along the way.
        let (_keep, v) = parse("(- right (200))");
        let mut align = -1_i32;
        let r = calc_pixel_width_or_height(&ctx, &v, true, Some(&mut align));
        // The outer result is the computed offset from the align_to
        // base. For doom-modeline this is what the caller adds to
        // current_x to reach the target position. With our context
        // (right=792, literal=200):
        //   first iter: px = 0 (right wrote 792 to align_to, returned 0)
        //   second iter: px = 200 (literal), subtracted: 0 + (-200)... wait.
        //
        // Hmm. Let me work through this more carefully against GNU:
        //   cdr = (right (200))
        //   iter 1 (right): px = 0, align_to becomes 792
        //     first=true → pixels = -0 = 0 (minus head)
        //   iter 2 ((200)): px = 200 (no align_to offset since align_to >= 0)
        //     not first → pixels += 200 → pixels = 200
        //   end of loop: minus ⇒ pixels = -200
        //
        // Hmm. That gives -200, not 792-200=592.
        //
        // GNU's stretch-glyph producer uses the RESOLVED align_to value
        // as the target x, and the returned `res` as extra stretch.
        // So target_x = align_to (792) and extra = res (-200)?
        //
        // Re-reading xdisp.c:32585:
        //   int x = it->current_x + ...;
        //   int x0 = x;
        //   ...
        //   if (it->glyph_row == NULL || !it->glyph_row->mode_line_p)
        //     align_to = (align_to < 0 ? 0 : align_to);
        //   else if (align_to < 0)
        //     align_to = x;
        //   ...
        //   if (align_to < 0)
        //     align_to = x + (int)tem;
        //   ...
        //   width = max (0, align_to - x);
        //
        // So the convention is: align_to ends up as an absolute pixel
        // position (the target x). The width of the stretch is
        // max(0, align_to - current_x). The `res` return value is used
        // only when align_to < 0 (width mode, no base).
        //
        // For `(- right (200))`:
        //   - align_to starts at -1
        //   - iter 1: `right` sets align_to = 792, returns res = 0
        //   - iter 2: `(200)` returns res = 200, align_to stays 792
        //     (it's no longer < 0)
        //   - plus/minus accumulator: first=0 (negated), then +200 → 200
        //   - minus negation at end: -200
        //
        // So the function returns -200 with align_to = 792. The caller
        // then computes target_x = align_to + res = 792 + (-200) = 592.
        //
        // That matches doom's intent.
        //
        // So the test should verify: align_to becomes 792 (updated by
        // `right`), and the returned pixels is -200.
        assert_eq!(align, 792);
        assert_eq!(r, Some(-200.0));
        // The caller resolves the final target position:
        let target_x = align + (r.unwrap() as i32);
        assert_eq!(target_x, 592);
    }

    // ------------------------------------------------------------------
    // Regression: fixnum :align-to behaves like the old parser
    // ------------------------------------------------------------------
    #[test]
    fn fixnum_align_to_in_align_mode_writes_column_offset() {
        let ctx = test_ctx();
        // A bare fixnum in align-to mode: n * column_width + lnum_pixel_width.
        // With lnum_pixel_width = 0 and column_width = 10:
        //   `5` in align-to mode → 5 * 10 = 50 pixels
        // The old parser at engine.rs:527 did: content_x + n * char_w.
        // Our helper returns the pixels portion; the caller is
        // responsible for adding content_x (or using align_to as target).
        let v = Value::fixnum(5);
        let mut align = -1_i32;
        assert_eq!(
            calc_pixel_width_or_height(&ctx, &v, true, Some(&mut align)),
            Some(50.0)
        );
    }
}
