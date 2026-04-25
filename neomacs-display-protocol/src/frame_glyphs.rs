//! Frame glyph buffer for matrix-based full-frame rendering.
//!
//! Each frame, the C-side matrix walker extracts ALL visible glyphs from
//! Emacs's current_matrix and rebuilds this buffer from scratch. No
//! incremental overlap tracking is needed.

use crate::face::{BoxType, Face, FaceAttributes, UnderlineStyle};
use crate::scroll_animation::{ScrollEasing, ScrollEffect};
use crate::types::{Color, Rect};
use crate::ui_types::TabBarItem;
use std::collections::HashMap;

/// GNU Emacs `enum text_cursor_kinds` (`src/dispextern.h:204-212`).
///
/// The discriminants match GNU exactly so anyone copying constants
/// from `xdisp.c` does not get a silently re-numbered enum:
///
/// ```text
/// DEFAULT_CURSOR    = -2
/// NO_CURSOR         = -1
/// FILLED_BOX_CURSOR =  0
/// HOLLOW_BOX_CURSOR =  1
/// BAR_CURSOR        =  2
/// HBAR_CURSOR       =  3
/// ```
///
/// Cursor audit Finding 1 in `drafts/cursor-audit.md`: an earlier
/// internal `u8` encoding swapped slots 1/3 and used `4` as an
/// out-of-band sentinel for `NO_CURSOR`. That divergence is
/// removed; this enum is now the single canonical representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i8)]
pub enum CursorKind {
    Default = -2,
    NoCursor = -1,
    FilledBox = 0,
    HollowBox = 1,
    Bar = 2,
    Hbar = 3,
}

impl CursorKind {
    /// Decode from GNU's signed `enum text_cursor_kinds` integer
    /// representation. Returns `None` for any value outside the
    /// six legal discriminants.
    pub fn from_gnu_code(code: i8) -> Option<Self> {
        match code {
            -2 => Some(Self::Default),
            -1 => Some(Self::NoCursor),
            0 => Some(Self::FilledBox),
            1 => Some(Self::HollowBox),
            2 => Some(Self::Bar),
            3 => Some(Self::Hbar),
            _ => None,
        }
    }

    /// GNU enum integer code (matches `enum text_cursor_kinds`).
    pub fn gnu_code(self) -> i8 {
        self as i8
    }
}

/// Cursor visual style, carrying bar/hbar dimensions.
///
/// Filled and hollow cursors use the owning slot rectangle as-is. Bar/Hbar
/// variants carry the thin dimension (width or height) for rendering within
/// that slot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CursorStyle {
    /// Filled box cursor (covers entire character cell)
    FilledBox,
    /// Vertical bar cursor with specified width in pixels
    Bar(f32),
    /// Horizontal bar (underline) cursor with specified height in pixels
    Hbar(f32),
    /// Hollow box cursor (unfocused window border)
    Hollow,
}

impl CursorStyle {
    /// Convert a `CursorKind` plus bar width into a renderable
    /// `CursorStyle`. `Default` collapses to `FilledBox` (its
    /// resolved value when no buffer/window override is in effect)
    /// and `NoCursor` returns `None`.
    pub fn from_kind(kind: CursorKind, bar_width: i32) -> Option<CursorStyle> {
        let dim = bar_width.max(1) as f32;
        match kind {
            CursorKind::FilledBox | CursorKind::Default => Some(CursorStyle::FilledBox),
            CursorKind::HollowBox => Some(CursorStyle::Hollow),
            CursorKind::Bar => Some(CursorStyle::Bar(dim)),
            CursorKind::Hbar => Some(CursorStyle::Hbar(dim)),
            CursorKind::NoCursor => None,
        }
    }

    /// Legacy entry point that accepts the old `u8` encoding so any
    /// out-of-tree caller still compiles. The body now decodes via
    /// `CursorKind::from_gnu_code` and routes through `from_kind`,
    /// so passing the old `0=box, 1=bar, 2=hbar, 3=hollow` byte
    /// arrangement will silently produce the wrong shape — callers
    /// must migrate to `CursorKind`.
    #[deprecated(note = "use CursorStyle::from_kind with CursorKind for GNU-parity encoding")]
    pub fn from_type(cursor_type: u8, bar_width: i32) -> Option<CursorStyle> {
        let kind = CursorKind::from_gnu_code(cursor_type as i8)?;
        Self::from_kind(kind, bar_width)
    }

    /// Returns true if this is a hollow (unfocused) cursor
    pub fn is_hollow(&self) -> bool {
        matches!(self, CursorStyle::Hollow)
    }
}

/// Semantic role of a glyph row emitted by layout.
///
/// This is authoritative layout metadata used by renderer ordering/clipping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GlyphRowRole {
    /// Regular buffer text rows.
    #[default]
    Text,
    /// Tab-line row.
    TabLine,
    /// Header-line row.
    HeaderLine,
    /// Mode-line row.
    ModeLine,
    /// Minibuffer/echo text row.
    Minibuffer,
    /// Frame-level tab-bar row.
    TabBar,
}

impl GlyphRowRole {
    /// True for UI chrome rows that should render above regular text rows.
    pub fn is_chrome(self) -> bool {
        matches!(
            self,
            Self::TabLine | Self::HeaderLine | Self::ModeLine | Self::TabBar
        )
    }
}

/// Stable identity for one materialized display slot within a frame.
///
/// This is the shared contract between layout and rendering for
/// "the thing under point": the cursor points at a slot id, and the
/// renderer can target that exact slot instead of re-discovering it
/// from geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DisplaySlotId {
    /// Window that owns the slot.
    pub window_id: i64,
    /// Visual row within the owning window.
    pub row: u32,
    /// Visual column within that row.
    pub col: u16,
}

impl DisplaySlotId {
    pub const ZERO: Self = Self {
        window_id: 0,
        row: 0,
        col: 0,
    };

    /// Best-effort slot identity for direct pixel-emission paths.
    ///
    /// Matrix-backed layout should populate slot ids from explicit row/column
    /// indices. This helper exists for manual glyph construction in tests and
    /// direct frame-space emission paths that have not been matrix-ified yet.
    pub fn from_pixels(window_id: i64, x: f32, y: f32, char_width: f32, char_height: f32) -> Self {
        let row = if char_height > 0.0 {
            (y / char_height).round().max(0.0) as u32
        } else {
            0
        };
        let col = if char_width > 0.0 {
            (x / char_width).round().max(0.0) as u16
        } else {
            0
        };
        Self {
            window_id,
            row,
            col,
        }
    }
}

impl Default for DisplaySlotId {
    fn default() -> Self {
        Self::ZERO
    }
}

/// A single glyph to render
#[derive(Debug, Clone)]
pub enum FrameGlyph {
    /// Character glyph with text
    Char {
        /// Window identifier this glyph belongs to.
        window_id: i64,
        /// Layout row role for ordering.
        row_role: GlyphRowRole,
        /// Authoritative clip rect in frame coordinates.
        clip_rect: Option<Rect>,
        /// Stable identity of the covered display slot.
        slot_id: DisplaySlotId,
        /// Bidirectional resolved level for this displayed glyph.
        ///
        /// 0 is the default LTR level; odd values indicate RTL runs.
        bidi_level: u8,
        /// Character to render (base character for single-codepoint glyphs)
        char: char,
        /// Composed text for multi-codepoint grapheme clusters (emoji ZWJ, combining marks).
        /// When Some, the renderer uses this instead of `char` for glyph lookup.
        composed: Option<Box<str>>,
        /// Frame-absolute X position
        x: f32,
        /// Frame-absolute Y position
        y: f32,
        /// Frame-absolute baseline Y position (authoritative from layout)
        baseline: f32,
        /// Glyph width
        width: f32,
        /// Row height
        height: f32,
        /// Font ascent
        ascent: f32,
        /// Foreground color
        fg: Color,
        /// Background color (if not transparent)
        bg: Option<Color>,
        /// Face ID for font lookup
        face_id: u32,
        /// Font weight (CSS scale: 100=thin, 400=normal, 700=bold, 900=black)
        font_weight: u16,
        /// Italic flag
        italic: bool,
        /// Font size in pixels
        font_size: f32,
        /// Underline style (0=none, 1=single, 2=wave, 3=double, 4=dotted, 5=dashed)
        underline: u8,
        /// Underline color
        underline_color: Option<Color>,
        /// Strike-through (0=none, 1=enabled)
        strike_through: u8,
        /// Strike-through color
        strike_through_color: Option<Color>,
        /// Overline (0=none, 1=enabled)
        overline: u8,
        /// Overline color
        overline_color: Option<Color>,
        /// Overstrike: draw glyph twice (at x and x+1) to simulate bold.
        /// Set when Emacs can't find a bold variant for the font.
        overstrike: bool,
    },

    /// Stretch (whitespace) glyph
    Stretch {
        /// Window identifier this glyph belongs to.
        window_id: i64,
        /// Layout row role for ordering.
        row_role: GlyphRowRole,
        /// Authoritative clip rect in frame coordinates.
        clip_rect: Option<Rect>,
        /// Stable identity of the covered display slot.
        slot_id: DisplaySlotId,
        /// Bidirectional resolved level for this displayed slot.
        ///
        /// 0 is the default LTR level; odd values indicate RTL runs.
        bidi_level: u8,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        bg: Color,
        face_id: u32,
        /// Stipple pattern ID (0 = none, references stipple_patterns in FrameGlyphBuffer)
        stipple_id: i32,
        /// Foreground color for stipple pattern (stipple bits use fg, gaps use bg)
        stipple_fg: Option<Color>,
    },

    /// Image glyph
    Image {
        window_id: i64,
        row_role: GlyphRowRole,
        clip_rect: Option<Rect>,
        slot_id: Option<DisplaySlotId>,
        image_id: u32,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },

    /// Video glyph (inline in buffer)
    Video {
        window_id: i64,
        row_role: GlyphRowRole,
        clip_rect: Option<Rect>,
        slot_id: Option<DisplaySlotId>,
        video_id: u32,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        loop_count: i32,
        autoplay: bool,
    },

    /// WebKit glyph (inline in buffer)
    WebKit {
        window_id: i64,
        row_role: GlyphRowRole,
        clip_rect: Option<Rect>,
        slot_id: Option<DisplaySlotId>,
        webkit_id: u32,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },

    /// Window background
    Background { bounds: Rect, color: Color },

    /// Window border (vertical/horizontal divider)
    Border {
        window_id: i64,
        row_role: GlyphRowRole,
        clip_rect: Option<Rect>,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        color: Color,
    },

    /// Scroll bar (GPU-rendered)
    ScrollBar {
        /// True for horizontal, false for vertical
        horizontal: bool,
        /// Frame-absolute position and dimensions of the scroll bar track
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        /// Thumb start position (pixels from track start)
        thumb_start: f32,
        /// Thumb size (pixels)
        thumb_size: f32,
        /// Track background color
        track_color: Color,
        /// Thumb color
        thumb_color: Color,
    },

    /// Terminal glyph (inline in buffer or window-mode)
    #[cfg(feature = "neo-term")]
    Terminal {
        terminal_id: u32,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
}

impl FrameGlyph {
    /// Returns true if this glyph belongs to a chrome row
    /// that should be rendered above regular text rows.
    pub fn is_chrome_row(&self) -> bool {
        match self {
            FrameGlyph::Char { row_role, .. } => row_role.is_chrome(),
            FrameGlyph::Stretch { row_role, .. } => row_role.is_chrome(),
            FrameGlyph::Image { row_role, .. } => row_role.is_chrome(),
            FrameGlyph::Video { row_role, .. } => row_role.is_chrome(),
            FrameGlyph::WebKit { row_role, .. } => row_role.is_chrome(),
            FrameGlyph::Border { row_role, .. } => row_role.is_chrome(),
            _ => false,
        }
    }

    /// Backward-compatible alias for callers not yet renamed.
    pub fn is_overlay(&self) -> bool {
        self.is_chrome_row()
    }

    /// Slot identity for displayed content that occupies a character cell.
    pub fn slot_id(&self) -> Option<DisplaySlotId> {
        match self {
            FrameGlyph::Char { slot_id, .. } | FrameGlyph::Stretch { slot_id, .. } => {
                Some(*slot_id)
            }
            FrameGlyph::Image { slot_id, .. }
            | FrameGlyph::Video { slot_id, .. }
            | FrameGlyph::WebKit { slot_id, .. } => *slot_id,
            _ => None,
        }
    }

    /// Bidirectional resolved level for displayed character/stretch slots.
    pub fn bidi_level(&self) -> Option<u8> {
        match self {
            FrameGlyph::Char { bidi_level, .. } | FrameGlyph::Stretch { bidi_level, .. } => {
                Some(*bidi_level)
            }
            _ => None,
        }
    }
}

/// Authoritative physical cursor snapshot for a frame.
///
/// This mirrors GNU's `phys_cursor` / `phys_cursor_*` split at the
/// display-protocol level: layout owns the cursor slot and geometry,
/// the renderer only consumes it.
#[derive(Debug, Clone, PartialEq)]
pub struct PhysCursor {
    /// Window that owns the cursor.
    pub window_id: i32,
    /// Buffer position covered by the cursor slot.
    pub charpos: usize,
    /// Matrix row that owns the cursor.
    pub row: usize,
    /// Column within the owning row.
    pub col: u16,
    /// Stable identity of the covered display slot.
    pub slot_id: DisplaySlotId,
    /// Frame-absolute cursor origin.
    pub x: f32,
    pub y: f32,
    /// Cursor rectangle dimensions in pixels.
    pub width: f32,
    pub height: f32,
    /// Pixels above the baseline.
    pub ascent: f32,
    /// Visual cursor style.
    pub style: CursorStyle,
    /// Cursor color.
    pub color: Color,
    /// Foreground color to use when redrawing the covered slot.
    pub cursor_fg: Color,
}

/// Decorative per-window cursor visual emitted by layout.
///
/// This covers non-selected-window hollow cursors and any other
/// non-physical cursor hints that should be drawn without owning the
/// selected frame cursor. The authoritative selected cursor lives in
/// `FrameGlyphBuffer::phys_cursor`.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowCursorVisual {
    /// Window that owns the cursor visual.
    pub window_id: i32,
    /// Display slot the visual should stay attached to.
    pub slot_id: DisplaySlotId,
    /// Frame-absolute origin.
    pub x: f32,
    pub y: f32,
    /// Cursor rectangle dimensions in pixels.
    pub width: f32,
    pub height: f32,
    /// Visual cursor style.
    pub style: CursorStyle,
    /// Cursor color.
    pub color: Color,
}

/// Frame-level tab bar metadata published alongside rendered glyphs.
///
/// Rendering still comes from the tab-bar row glyphs. This metadata exists so
/// hit-testing can use the same published snapshot instead of a side-channel
/// runtime command.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameTabBarState {
    pub items: Vec<TabBarItem>,
    pub y: f32,
    pub height: f32,
}

/// Stipple pattern: XBM bitmap data for tiled background patterns
#[derive(Debug, Clone)]
pub struct StipplePattern {
    /// Pattern width in pixels
    pub width: u32,
    /// Pattern height in pixels
    pub height: u32,
    /// Raw XBM bits: row-by-row, each row is (width+7)/8 bytes, LSB-first
    pub bits: Vec<u8>,
}

/// Per-window metadata for animation transition detection
#[derive(Debug, Clone, PartialEq)]
pub struct WindowInfo {
    /// Window pointer as i64 (unique window identifier)
    pub window_id: i64,
    /// Buffer pointer as u64 (unique buffer identifier)
    pub buffer_id: u64,
    /// First visible character position (marker_position(w->start))
    pub window_start: i64,
    /// Last visible character position
    pub window_end: i64,
    /// Total buffer size in characters (BUF_Z)
    pub buffer_size: i64,
    /// Frame-absolute window bounds (includes mode-line)
    pub bounds: Rect,
    /// Height of the mode-line in pixels (0 if no mode-line)
    pub mode_line_height: f32,
    /// Height of the header-line in pixels (0 if no header-line)
    pub header_line_height: f32,
    /// Height of the tab-line in pixels (0 if no tab-line)
    pub tab_line_height: f32,
    /// Whether this is the selected (active) window
    pub selected: bool,
    /// Whether this is the minibuffer window
    pub is_minibuffer: bool,
    /// Character cell height for this window (tracks text-scale-adjust)
    pub char_height: f32,
    /// Buffer file name (empty string if no file)
    pub buffer_file_name: String,
    /// Whether the buffer has unsaved modifications
    pub modified: bool,
}

/// Transition kind emitted by authoritative layout producers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WindowTransitionKind {
    /// Crossfade the window bounds.
    Crossfade,
    /// Slide old content by a scroll delta.
    ScrollSlide {
        /// +1 = scroll down (content moves up), -1 = scroll up.
        direction: i32,
        /// Pixel distance to slide.
        scroll_distance: f32,
    },
}

/// Explicit transition hint from layout producers to render thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowTransitionHint {
    /// Target window id.
    pub window_id: i64,
    /// Target bounds in frame coordinates.
    pub bounds: Rect,
    /// Transition kind payload.
    pub kind: WindowTransitionKind,
    /// Optional effect override. `None` means "use current policy default".
    pub effect: Option<ScrollEffect>,
    /// Optional easing override. `None` means "use current policy default".
    pub easing: Option<ScrollEasing>,
}

/// Explicit effect hint from layout producers to render thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WindowEffectHint {
    /// Fade in newly shown text in a window region.
    TextFadeIn { window_id: i64, bounds: Rect },
    /// Animate per-line spacing during scroll.
    ScrollLineSpacing {
        window_id: i64,
        bounds: Rect,
        direction: i32,
    },
    /// Show scroll momentum glow.
    ScrollMomentum {
        window_id: i64,
        bounds: Rect,
        direction: i32,
    },
    /// Velocity-based fade intensity during scroll.
    ScrollVelocityFade {
        window_id: i64,
        bounds: Rect,
        delta: f32,
    },
    /// Animate line insertion/deletion below edit point.
    LineAnimation {
        window_id: i64,
        bounds: Rect,
        edit_y: f32,
        offset: f32,
    },
    /// Fade highlight when selected window changes.
    WindowSwitchFade { window_id: i64, bounds: Rect },
    /// Theme/background changed; request a full-frame theme crossfade.
    ThemeTransition { bounds: Rect },
}

/// Buffer collecting glyphs for current frame.
///
/// With matrix-based rendering, this buffer is cleared and rebuilt from scratch
/// each frame by the C-side matrix walker. No incremental state management needed.
#[derive(Debug, Default, Clone)]
pub struct FrameGlyphBuffer {
    /// Frame dimensions
    pub width: f32,
    pub height: f32,

    /// Default character cell dimensions (from FRAME_COLUMN_WIDTH / FRAME_LINE_HEIGHT)
    pub char_width: f32,
    pub char_height: f32,
    /// Default font pixel size (from FRAME_FONT(f)->pixel_size)
    pub font_pixel_size: f32,

    /// Frame background color
    pub background: Color,

    // --- Child frame identity (Phase 1) ---
    /// Frame pointer cast to u64 (0 = root/unset)
    pub frame_id: u64,
    /// Parent frame pointer (0 = root frame, no parent)
    pub parent_id: u64,
    /// Position relative to parent frame (pixels)
    pub parent_x: f32,
    pub parent_y: f32,
    /// Stacking order among sibling child frames
    pub z_order: i32,
    /// Whether child-frame decorations are suppressed.
    pub undecorated: bool,
    /// Child frame border width (pixels)
    pub border_width: f32,
    /// Child frame border color
    pub border_color: Color,
    /// Background opacity (1.0 = opaque, 0.0 = transparent)
    pub background_alpha: f32,
    /// Whether this frame should not accept keyboard focus
    pub no_accept_focus: bool,

    /// All glyphs to render this frame
    pub glyphs: Vec<FrameGlyph>,

    /// Per-window metadata for animation detection
    pub window_infos: Vec<WindowInfo>,

    /// Explicit transition requests emitted by layout producers.
    pub transition_hints: Vec<WindowTransitionHint>,

    /// Explicit effect requests emitted by layout producers.
    pub effect_hints: Vec<WindowEffectHint>,

    /// Authoritative active cursor for the frame.
    pub phys_cursor: Option<PhysCursor>,

    /// Decorative per-window cursor visuals emitted by layout.
    pub window_cursors: Vec<WindowCursorVisual>,

    /// Frame-level tab bar metadata for hit-testing.
    pub tab_bar: Option<FrameTabBarState>,

    /// Current face attributes (set before adding char glyphs)
    current_face_id: u32,
    current_fg: Color,
    current_bg: Option<Color>,
    current_font_family: String,
    current_font_weight: u16,
    current_italic: bool,
    current_font_size: f32,
    current_underline: u8,
    current_underline_color: Option<Color>,
    current_strike_through: u8,
    current_strike_through_color: Option<Color>,
    current_overline: u8,
    current_overline_color: Option<Color>,
    current_overstrike: bool,
    current_window_id: i64,
    current_row_role: GlyphRowRole,
    current_clip_rect: Option<Rect>,

    /// Full face data: face_id -> Face (includes box, underline, etc.)
    /// Rebuilt from scratch each frame by apply_face() in the layout engine.
    pub faces: HashMap<u32, Face>,

    /// Stipple patterns: bitmap_id -> StipplePattern
    pub stipple_patterns: HashMap<i32, StipplePattern>,
}

impl FrameGlyphBuffer {
    fn synthesize_face(
        &self,
        face_id: u32,
        fg: Color,
        bg: Option<Color>,
        font_family: &str,
        font_weight: u16,
        italic: bool,
        font_size: f32,
        underline: u8,
        underline_color: Option<Color>,
        strike_through: u8,
        strike_through_color: Option<Color>,
        overline: u8,
        overline_color: Option<Color>,
        _overstrike: bool,
    ) -> Face {
        let mut attrs = FaceAttributes::empty();
        if font_weight >= 700 {
            attrs |= FaceAttributes::BOLD;
        }
        if italic {
            attrs |= FaceAttributes::ITALIC;
        }
        if underline > 0 {
            attrs |= FaceAttributes::UNDERLINE;
        }
        if strike_through > 0 {
            attrs |= FaceAttributes::STRIKE_THROUGH;
        }
        if overline > 0 {
            attrs |= FaceAttributes::OVERLINE;
        }

        let underline_style = match underline {
            1 => UnderlineStyle::Line,
            2 => UnderlineStyle::Wave,
            3 => UnderlineStyle::Double,
            4 => UnderlineStyle::Dotted,
            5 => UnderlineStyle::Dashed,
            _ => UnderlineStyle::None,
        };

        Face {
            id: face_id,
            foreground: fg,
            background: bg.unwrap_or(Color::TRANSPARENT),
            use_default_foreground: false,
            use_default_background: false,
            underline_color,
            overline_color,
            strike_through_color,
            box_color: None,
            font_family: font_family.to_string(),
            font_size,
            font_weight,
            attributes: attrs,
            underline_style,
            box_type: BoxType::None,
            box_line_width: 0,
            box_corner_radius: 0,
            box_border_style: 0,
            box_border_speed: 1.0,
            box_color2: None,
            font_file_path: None,
            font_ascent: 0,
            font_descent: 0,
            underline_position: 1,
            underline_thickness: 1,
        }
    }

    pub fn new() -> Self {
        Self {
            width: 0.0,
            height: 0.0,
            char_width: 8.0,
            char_height: 16.0,
            font_pixel_size: 14.0,
            background: Color::BLACK,
            frame_id: 0,
            parent_id: 0,
            parent_x: 0.0,
            parent_y: 0.0,
            z_order: 0,
            undecorated: false,
            border_width: 0.0,
            border_color: Color::BLACK,
            background_alpha: 1.0,
            no_accept_focus: false,
            glyphs: Vec::with_capacity(10000),
            window_infos: Vec::with_capacity(16),
            transition_hints: Vec::with_capacity(16),
            effect_hints: Vec::with_capacity(16),
            phys_cursor: None,
            window_cursors: Vec::with_capacity(8),
            tab_bar: None,
            current_face_id: 0,
            current_fg: Color::WHITE,
            current_bg: None,
            current_font_family: "monospace".to_string(),
            current_font_weight: 400,
            current_italic: false,
            current_font_size: 14.0,
            current_underline: 0,
            current_underline_color: None,
            current_strike_through: 0,
            current_strike_through_color: None,
            current_overline: 0,
            current_overline_color: None,
            current_overstrike: false,
            current_window_id: 0,
            current_row_role: GlyphRowRole::Text,
            current_clip_rect: None,
            faces: HashMap::new(),
            stipple_patterns: HashMap::new(),
        }
    }

    /// Create a new buffer with specified dimensions
    pub fn with_size(width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            ..Self::new()
        }
    }

    /// Clear all glyphs for a fresh full-frame rebuild.
    /// Called at the start of each frame by the matrix walker.
    pub fn clear_all(&mut self) {
        self.glyphs.clear();
        self.window_infos.clear();
        self.transition_hints.clear();
        self.effect_hints.clear();
        self.phys_cursor = None;
        self.window_cursors.clear();
        self.tab_bar = None;
        self.stipple_patterns.clear();
        self.faces.clear();
        self.current_window_id = 0;
        self.current_row_role = GlyphRowRole::Text;
        self.current_clip_rect = None;
    }

    fn current_slot_id(&self, x: f32, y: f32) -> DisplaySlotId {
        DisplaySlotId::from_pixels(
            self.current_window_id,
            x,
            y,
            self.char_width,
            self.char_height,
        )
    }

    /// Drain producer-emitted transition and effect hints exactly once.
    pub fn take_runtime_hints(&mut self) -> (Vec<WindowTransitionHint>, Vec<WindowEffectHint>) {
        (
            std::mem::take(&mut self.transition_hints),
            std::mem::take(&mut self.effect_hints),
        )
    }

    /// Set frame identity for child frame support.
    /// Called after begin_frame, before glyphs are added.
    pub fn set_frame_identity(
        &mut self,
        frame_id: u64,
        parent_id: u64,
        parent_x: f32,
        parent_y: f32,
        z_order: i32,
        undecorated: bool,
        border_width: f32,
        border_color: Color,
        no_accept_focus: bool,
        background_alpha: f32,
    ) {
        self.frame_id = frame_id;
        self.parent_id = parent_id;
        self.parent_x = parent_x;
        self.parent_y = parent_y;
        self.z_order = z_order;
        self.undecorated = undecorated;
        self.border_width = border_width;
        self.border_color = border_color;
        self.no_accept_focus = no_accept_focus;
        self.background_alpha = background_alpha;
    }

    /// Set current face attributes for subsequent char glyphs (with font family).
    ///
    /// This also synthesizes a baseline `Face` entry for `face_id`, so the
    /// display IR stays self-consistent even when a caller only switches the
    /// current face state and does not separately populate `faces`.
    pub fn set_face_with_font(
        &mut self,
        face_id: u32,
        fg: Color,
        bg: Option<Color>,
        font_family: &str,
        font_weight: u16,
        italic: bool,
        font_size: f32,
        underline: u8,
        underline_color: Option<Color>,
        strike_through: u8,
        strike_through_color: Option<Color>,
        overline: u8,
        overline_color: Option<Color>,
        overstrike: bool,
    ) {
        self.current_face_id = face_id;
        self.current_fg = fg;
        self.current_bg = bg;
        self.current_font_family = font_family.to_string();
        self.current_font_weight = font_weight;
        self.current_italic = italic;
        self.current_font_size = font_size;
        self.current_underline = underline;
        self.current_underline_color = underline_color;
        self.current_strike_through = strike_through;
        self.current_strike_through_color = strike_through_color;
        self.current_overline = overline;
        self.current_overline_color = overline_color;
        self.current_overstrike = overstrike;
        self.faces.insert(
            face_id,
            self.synthesize_face(
                face_id,
                fg,
                bg,
                font_family,
                font_weight,
                italic,
                font_size,
                underline,
                underline_color,
                strike_through,
                strike_through_color,
                overline,
                overline_color,
                overstrike,
            ),
        );
    }

    /// Set current face attributes for subsequent char glyphs.
    ///
    /// Uses the current font family and size when synthesizing the baseline
    /// `Face` entry for `face_id`.
    pub fn set_face(
        &mut self,
        face_id: u32,
        fg: Color,
        bg: Option<Color>,
        font_weight: u16,
        italic: bool,
        underline: u8,
        underline_color: Option<Color>,
        strike_through: u8,
        strike_through_color: Option<Color>,
        overline: u8,
        overline_color: Option<Color>,
    ) {
        self.current_face_id = face_id;
        self.current_fg = fg;
        self.current_bg = bg;
        self.current_font_weight = font_weight;
        self.current_italic = italic;
        self.current_underline = underline;
        self.current_underline_color = underline_color;
        self.current_strike_through = strike_through;
        self.current_strike_through_color = strike_through_color;
        self.current_overline = overline;
        self.current_overline_color = overline_color;
        self.faces.insert(
            face_id,
            self.synthesize_face(
                face_id,
                fg,
                bg,
                &self.current_font_family,
                font_weight,
                italic,
                self.current_font_size,
                underline,
                underline_color,
                strike_through,
                strike_through_color,
                overline,
                overline_color,
                self.current_overstrike,
            ),
        );
    }

    /// Set authoritative layout draw context for subsequent glyph emissions.
    pub fn set_draw_context(
        &mut self,
        window_id: i64,
        row_role: GlyphRowRole,
        clip_rect: Option<Rect>,
    ) {
        self.current_window_id = window_id;
        self.current_row_role = row_role;
        self.current_clip_rect = clip_rect;
    }

    /// Get font family for a face_id
    pub fn get_face_font(&self, face_id: u32) -> &str {
        self.faces
            .get(&face_id)
            .map(|f| f.font_family.as_str())
            .unwrap_or("monospace")
    }

    /// Get current font family
    pub fn get_current_font_family(&self) -> &str {
        &self.current_font_family
    }

    /// Get current foreground color
    pub fn get_current_fg(&self) -> Color {
        self.current_fg
    }

    /// Get current face background color (for stretch glyphs)
    pub fn get_current_bg(&self) -> Option<Color> {
        self.current_bg
    }

    /// Temporarily set fg/bg colors for margin rendering.
    pub fn set_colors(&mut self, fg: Color, bg: Option<Color>) {
        self.current_fg = fg;
        self.current_bg = bg;
    }

    /// Add a window background rectangle.
    /// With full-frame rebuild, no stale-background removal is needed.
    pub fn add_background(&mut self, x: f32, y: f32, width: f32, height: f32, color: Color) {
        self.glyphs.push(FrameGlyph::Background {
            bounds: Rect::new(x, y, width, height),
            color,
        });
    }

    /// Add a character glyph. No overlap removal needed with full-frame rebuild.
    pub fn add_char(
        &mut self,
        char: char,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        ascent: f32,
        _overlay_hint: bool,
    ) {
        self.glyphs.push(FrameGlyph::Char {
            window_id: self.current_window_id,
            row_role: self.current_row_role,
            clip_rect: self.current_clip_rect,
            slot_id: self.current_slot_id(x, y),
            bidi_level: 0,
            char,
            composed: None,
            x,
            y,
            baseline: y + ascent,
            width,
            height,
            ascent,
            fg: self.current_fg,
            bg: self.current_bg,
            face_id: self.current_face_id,
            font_weight: self.current_font_weight,
            italic: self.current_italic,
            font_size: self.current_font_size,
            underline: self.current_underline,
            underline_color: self.current_underline_color,
            strike_through: self.current_strike_through,
            strike_through_color: self.current_strike_through_color,
            overline: self.current_overline,
            overline_color: self.current_overline_color,
            overstrike: self.current_overstrike,
        });
    }

    /// Add a composed (multi-codepoint) character glyph.
    /// Used for grapheme clusters like emoji ZWJ sequences, combining diacritics.
    pub fn add_composed_char(
        &mut self,
        text: &str,
        base_char: char,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        ascent: f32,
        _overlay_hint: bool,
    ) {
        self.glyphs.push(FrameGlyph::Char {
            window_id: self.current_window_id,
            row_role: self.current_row_role,
            clip_rect: self.current_clip_rect,
            slot_id: self.current_slot_id(x, y),
            bidi_level: 0,
            char: base_char,
            composed: Some(text.into()),
            x,
            y,
            baseline: y + ascent,
            width,
            height,
            ascent,
            fg: self.current_fg,
            bg: self.current_bg,
            face_id: self.current_face_id,
            font_weight: self.current_font_weight,
            italic: self.current_italic,
            font_size: self.current_font_size,
            underline: self.current_underline,
            underline_color: self.current_underline_color,
            strike_through: self.current_strike_through,
            strike_through_color: self.current_strike_through_color,
            overline: self.current_overline,
            overline_color: self.current_overline_color,
            overstrike: self.current_overstrike,
        });
    }

    /// Get current font size
    pub fn font_size(&self) -> f32 {
        self.current_font_size
    }

    /// Set current font size (for display property height scaling)
    pub fn set_font_size(&mut self, size: f32) {
        self.current_font_size = size;
    }

    /// Add a stretch (whitespace) glyph. No overlap removal needed.
    pub fn add_stretch(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        bg: Color,
        face_id: u32,
        _overlay_hint: bool,
    ) {
        self.glyphs.push(FrameGlyph::Stretch {
            window_id: self.current_window_id,
            row_role: self.current_row_role,
            clip_rect: self.current_clip_rect,
            slot_id: self.current_slot_id(x, y),
            bidi_level: 0,
            x,
            y,
            width,
            height,
            bg,
            face_id,
            stipple_id: 0,
            stipple_fg: None,
        });
    }

    /// Add a stretch glyph with a stipple pattern
    pub fn add_stretch_stipple(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        bg: Color,
        fg: Color,
        face_id: u32,
        _overlay_hint: bool,
        stipple_id: i32,
    ) {
        self.glyphs.push(FrameGlyph::Stretch {
            window_id: self.current_window_id,
            row_role: self.current_row_role,
            clip_rect: self.current_clip_rect,
            slot_id: self.current_slot_id(x, y),
            bidi_level: 0,
            x,
            y,
            width,
            height,
            bg,
            face_id,
            stipple_id,
            stipple_fg: Some(fg),
        });
    }

    /// Add an image glyph
    pub fn add_image(&mut self, image_id: u32, x: f32, y: f32, width: f32, height: f32) {
        self.glyphs.push(FrameGlyph::Image {
            window_id: self.current_window_id,
            row_role: self.current_row_role,
            clip_rect: self.current_clip_rect,
            slot_id: Some(self.current_slot_id(x, y)),
            image_id,
            x,
            y,
            width,
            height,
        });
    }

    /// Add a video glyph
    pub fn add_video(
        &mut self,
        video_id: u32,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        loop_count: i32,
        autoplay: bool,
    ) {
        self.glyphs.push(FrameGlyph::Video {
            window_id: self.current_window_id,
            row_role: self.current_row_role,
            clip_rect: self.current_clip_rect,
            slot_id: Some(self.current_slot_id(x, y)),
            video_id,
            x,
            y,
            width,
            height,
            loop_count,
            autoplay,
        });
    }

    /// Add a webkit glyph
    pub fn add_webkit(&mut self, webkit_id: u32, x: f32, y: f32, width: f32, height: f32) {
        self.glyphs.push(FrameGlyph::WebKit {
            window_id: self.current_window_id,
            row_role: self.current_row_role,
            clip_rect: self.current_clip_rect,
            slot_id: Some(self.current_slot_id(x, y)),
            webkit_id,
            x,
            y,
            width,
            height,
        });
    }

    /// Add cursor
    pub fn add_cursor(
        &mut self,
        window_id: i32,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        style: CursorStyle,
        color: Color,
    ) {
        self.window_cursors.push(WindowCursorVisual {
            window_id,
            slot_id: DisplaySlotId::from_pixels(
                window_id as i64,
                x,
                y,
                self.char_width,
                self.char_height,
            ),
            x,
            y,
            width,
            height,
            style,
            color,
        });
    }

    /// Add per-window metadata for animation detection
    pub fn add_window_info(
        &mut self,
        window_id: i64,
        buffer_id: u64,
        window_start: i64,
        window_end: i64,
        buffer_size: i64,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        mode_line_height: f32,
        header_line_height: f32,
        tab_line_height: f32,
        selected: bool,
        is_minibuffer: bool,
        char_height: f32,
        buffer_file_name: String,
        modified: bool,
    ) {
        self.window_infos.push(WindowInfo {
            window_id,
            buffer_id,
            window_start,
            window_end,
            buffer_size,
            bounds: Rect::new(x, y, width, height),
            mode_line_height,
            header_line_height,
            tab_line_height,
            selected,
            is_minibuffer,
            char_height,
            buffer_file_name,
            modified,
        });
    }

    /// Add an explicit transition hint.
    pub fn add_transition_hint(&mut self, hint: WindowTransitionHint) {
        self.transition_hints.push(hint);
    }

    /// Add an explicit effect hint.
    pub fn add_effect_hint(&mut self, hint: WindowEffectHint) {
        self.effect_hints.push(hint);
    }

    /// Derive a transition hint by comparing previous/current window metadata.
    ///
    /// This centralizes transition geometry decisions outside the renderer.
    pub fn derive_transition_hint(
        prev: &WindowInfo,
        curr: &WindowInfo,
    ) -> Option<WindowTransitionHint> {
        if curr.is_minibuffer {
            return None;
        }

        if prev.buffer_id != 0 && curr.buffer_id != 0 && prev.buffer_id != curr.buffer_id {
            return Some(WindowTransitionHint {
                window_id: curr.window_id,
                bounds: curr.bounds,
                kind: WindowTransitionKind::Crossfade,
                effect: None,
                easing: None,
            });
        }

        if prev.window_start != curr.window_start {
            let top_chrome = curr.tab_line_height + curr.header_line_height;
            let content_height = curr.bounds.height - curr.mode_line_height - top_chrome;
            if content_height < 50.0 {
                return None;
            }

            let direction = if curr.window_start > prev.window_start {
                1
            } else {
                -1
            };

            let content_bounds = Rect::new(
                curr.bounds.x,
                curr.bounds.y + top_chrome,
                curr.bounds.width,
                content_height,
            );

            // Keep legacy estimate shape to preserve current feel.
            let cols = (curr.bounds.width / curr.char_height).max(1.0);
            let char_delta = (curr.window_start - prev.window_start).unsigned_abs() as f32;
            let est_lines = (char_delta / cols).max(1.0);
            let scroll_px = (est_lines * curr.char_height).min(content_height);

            return Some(WindowTransitionHint {
                window_id: curr.window_id,
                bounds: content_bounds,
                kind: WindowTransitionKind::ScrollSlide {
                    direction,
                    scroll_distance: scroll_px,
                },
                effect: None,
                easing: None,
            });
        }

        if (prev.char_height - curr.char_height).abs() > 1.0
            || (prev.bounds.width - curr.bounds.width).abs() > 2.0
            || (prev.bounds.height - curr.bounds.height).abs() > 2.0
        {
            return Some(WindowTransitionHint {
                window_id: curr.window_id,
                bounds: curr.bounds,
                kind: WindowTransitionKind::Crossfade,
                effect: None,
                easing: None,
            });
        }

        None
    }

    /// Set the authoritative physical cursor for the frame.
    pub fn set_phys_cursor(&mut self, mut cursor: PhysCursor) {
        if let Some(slot) = self.slot_glyph(cursor.slot_id) {
            match slot {
                FrameGlyph::Image {
                    x,
                    y,
                    width,
                    height,
                    ..
                }
                | FrameGlyph::Video {
                    x,
                    y,
                    width,
                    height,
                    ..
                }
                | FrameGlyph::WebKit {
                    x,
                    y,
                    width,
                    height,
                    ..
                } => {
                    cursor.style = CursorStyle::Hollow;
                    cursor.x = *x;
                    cursor.y = *y;
                    cursor.width = *width;
                    cursor.height = *height;
                    cursor.ascent = cursor.ascent.min(*height).max(1.0);
                }
                _ => {}
            }
        }
        self.phys_cursor = Some(cursor);
    }

    /// Look up the text or stretch glyph occupying a given display slot.
    pub fn slot_glyph(&self, slot_id: DisplaySlotId) -> Option<&FrameGlyph> {
        self.glyphs
            .iter()
            .find(|glyph| glyph.slot_id().is_some_and(|slot| slot == slot_id))
    }

    /// Add border
    pub fn add_border(&mut self, x: f32, y: f32, width: f32, height: f32, color: Color) {
        self.glyphs.push(FrameGlyph::Border {
            window_id: self.current_window_id,
            row_role: self.current_row_role,
            clip_rect: self.current_clip_rect,
            x,
            y,
            width,
            height,
            color,
        });
    }

    /// Add a scroll bar glyph (GPU-rendered)
    pub fn add_scroll_bar(
        &mut self,
        horizontal: bool,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        thumb_start: f32,
        thumb_size: f32,
        track_color: Color,
        thumb_color: Color,
    ) {
        self.glyphs.push(FrameGlyph::ScrollBar {
            horizontal,
            x,
            y,
            width,
            height,
            thumb_start,
            thumb_size,
            track_color,
            thumb_color,
        });
    }

    /// Add terminal glyph (inline or window mode)
    #[cfg(feature = "neo-term")]
    pub fn add_terminal(&mut self, terminal_id: u32, x: f32, y: f32, width: f32, height: f32) {
        self.glyphs.push(FrameGlyph::Terminal {
            terminal_id,
            x,
            y,
            width,
            height,
        });
    }

    /// Get glyph count
    pub fn len(&self) -> usize {
        self.glyphs.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }
}

#[cfg(test)]
#[path = "frame_glyphs_test.rs"]
mod tests;
