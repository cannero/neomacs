//! Evaluator-free frame content types — the handoff from the evaluator
//! (or mock-display) to the layout engine.
//!
//! FrameContent carries WHAT to display: faces, window structure, styled
//! text.  The layout engine adds HOW: font metrics, pixel positions, bidi
//! reordering, and produces FrameDisplayState.

use crate::face::Face;
use crate::types::{Color, Rect};

/// A single glyph with its face assignment and display property.
#[derive(Debug, Clone)]
pub struct StyledGlyph {
    pub ch: char,
    pub face_id: u32,
    pub display: Option<DisplayProperty>,
}

/// Display properties resolved by the evaluator into Rust enums.
#[derive(Debug, Clone)]
pub enum DisplayProperty {
    /// Character is invisible (invisible text property).
    Invisible,
    /// Replace with a different string and face.
    Replace(String, u32),
    /// A composed sequence (combining marks, ZWJ sequences, etc.).
    Composition(Vec<StyledGlyph>),
}

/// One row of buffer text: a sequence of styled glyphs.
#[derive(Debug, Clone)]
pub struct StyledLine {
    pub glyphs: Vec<StyledGlyph>,
}

impl StyledLine {
    pub fn from_str(text: &str, face_id: u32) -> Self {
        Self {
            glyphs: text
                .chars()
                .map(|ch| StyledGlyph {
                    ch,
                    face_id,
                    display: None,
                })
                .collect(),
        }
    }
}

/// Content for one window in a frame.
#[derive(Debug, Clone)]
pub struct WindowContent {
    pub window_id: u64,
    pub lines: Vec<StyledLine>,
    /// Pre-formatted mode-line text (evaluator-produced).
    pub mode_line_text: String,
    /// Pixel bounds relative to frame, computed by the evaluator from
    /// frame parameters and split configuration.
    pub pixel_bounds: Rect,
    pub selected: bool,
    /// Whether the buffer has been scrolled horizontally.
    pub truncated_lines: bool,
}

/// Content for a floating child frame (posframe, completion popup, etc.).
#[derive(Debug, Clone)]
pub struct ChildFrameContent {
    pub frame_id: u64,
    pub window: WindowContent,
    /// Position within the parent frame's pixel area.
    pub parent_x: f32,
    pub parent_y: f32,
    /// Stacking order relative to other child frames.
    pub z_order: i32,
}

/// The evaluator handoff: everything needed to lay out and render a frame.
///
/// This carries no Lisp values, no evaluator handles, no unresolved symbols.
/// Both the real neomacs evaluator bridge and mock-display produce this same
/// type, and the shared layout engine consumes it.
#[derive(Debug, Clone)]
pub struct FrameContent {
    pub frame_id: u64,
    /// Faces keyed by numeric ID.  Face 0 is the default face.
    pub faces: Vec<Face>,
    pub windows: Vec<WindowContent>,
    pub child_frames: Vec<ChildFrameContent>,
    /// Full frame pixel dimensions (from frame parameters).
    pub frame_pixel_width: f32,
    pub frame_pixel_height: f32,
    pub background: Color,
    /// Per-level menu bar items, if any.  Pre-formatted strings keyed by
    /// level.  Level 0 is the top-level menu bar.
    pub menu_bar: Option<Vec<String>>,
}
