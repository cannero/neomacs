//! Frame glyph buffer for matrix-based full-frame rendering.
//!
//! Each frame, the C-side matrix walker extracts ALL visible glyphs from
//! Emacs's current_matrix and rebuilds this buffer from scratch. No
//! incremental overlap tracking is needed.

use crate::face::{BoxType, Face, FaceAttributes, UnderlineStyle};
use crate::scroll_animation::{ScrollEasing, ScrollEffect};
use crate::types::{Color, Rect};
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
/// The cursor glyph always stores full cell dimensions (char_width, face_height).
/// Bar/Hbar variants carry the thin dimension (width or height) for rendering.
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
        webkit_id: u32,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },

    /// Cursor
    Cursor {
        window_id: i32, // Window ID to track which window this cursor belongs to
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        style: CursorStyle,
        color: Color,
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
}

/// Inverse video info for the character under a filled box cursor
#[derive(Debug, Clone)]
pub struct CursorInverseInfo {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Cursor rect color (drawn as background)
    pub cursor_bg: Color,
    /// Text color for the character at cursor position
    pub cursor_fg: Color,
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

    /// Window regions for this frame (rebuilt each frame by add_window calls)
    pub window_regions: Vec<Rect>,

    /// Window regions from previous frame (kept for compatibility)
    pub prev_window_regions: Vec<Rect>,

    /// Per-window metadata for animation detection
    pub window_infos: Vec<WindowInfo>,

    /// Explicit transition requests emitted by layout producers.
    pub transition_hints: Vec<WindowTransitionHint>,

    /// Explicit effect requests emitted by layout producers.
    pub effect_hints: Vec<WindowEffectHint>,

    /// Inverse video info for filled box cursor (set by C for style 0)
    pub cursor_inverse: Option<CursorInverseInfo>,

    /// Flag: layout changed last frame (kept for compatibility)
    pub layout_changed: bool,

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
            border_width: 0.0,
            border_color: Color::BLACK,
            background_alpha: 1.0,
            no_accept_focus: false,
            glyphs: Vec::with_capacity(10000),
            window_regions: Vec::with_capacity(16),
            prev_window_regions: Vec::with_capacity(16),
            window_infos: Vec::with_capacity(16),
            transition_hints: Vec::with_capacity(16),
            effect_hints: Vec::with_capacity(16),
            cursor_inverse: None,
            layout_changed: false,
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
        self.window_regions.clear();
        self.window_infos.clear();
        self.transition_hints.clear();
        self.effect_hints.clear();
        self.cursor_inverse = None;
        self.stipple_patterns.clear();
        self.faces.clear();
        self.current_window_id = 0;
        self.current_row_role = GlyphRowRole::Text;
        self.current_clip_rect = None;
    }

    /// Start new frame - prepare for new content (compatibility shim)
    pub fn start_frame(&mut self) {
        std::mem::swap(&mut self.prev_window_regions, &mut self.window_regions);
        self.window_regions.clear();
    }

    /// End frame (compatibility shim, always returns false now)
    pub fn end_frame(&mut self) -> bool {
        false
    }

    /// Check and reset layout_changed flag (compatibility)
    pub fn take_layout_changed(&mut self) -> bool {
        let was_changed = self.layout_changed;
        self.layout_changed = false;
        was_changed
    }

    /// Clear buffer for new frame (legacy API)
    pub fn begin_frame(&mut self, width: f32, height: f32, background: Color) {
        self.width = width;
        self.height = height;
        self.background = background;
        self.glyphs.clear();
        self.transition_hints.clear();
        self.effect_hints.clear();
        self.cursor_inverse = None;
        self.stipple_patterns.clear();
        self.faces.clear();
        self.current_window_id = 0;
        self.current_row_role = GlyphRowRole::Text;
        self.current_clip_rect = None;
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

    /// Add a window background rectangle and record the window region.
    /// With full-frame rebuild, no stale-background removal is needed.
    pub fn add_background(&mut self, x: f32, y: f32, width: f32, height: f32, color: Color) {
        self.window_regions.push(Rect::new(x, y, width, height));
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
        self.glyphs.push(FrameGlyph::Cursor {
            window_id,
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

    /// Set cursor inverse video info (for filled box cursor)
    pub fn set_cursor_inverse(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        cursor_bg: Color,
        cursor_fg: Color,
    ) {
        self.cursor_inverse = Some(CursorInverseInfo {
            x,
            y,
            width,
            height,
            cursor_bg,
            cursor_fg,
        });
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
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Helper: assert a Color matches expected RGBA (with tolerance for floats)
    // -----------------------------------------------------------------------
    fn assert_color_eq(actual: &Color, expected: &Color) {
        assert!(
            (actual.r - expected.r).abs() < 1e-5
                && (actual.g - expected.g).abs() < 1e-5
                && (actual.b - expected.b).abs() < 1e-5
                && (actual.a - expected.a).abs() < 1e-5,
            "Colors differ: actual {:?} vs expected {:?}",
            actual,
            expected,
        );
    }

    fn make_window_info(
        window_id: i64,
        buffer_id: u64,
        window_start: i64,
        bounds: Rect,
    ) -> WindowInfo {
        WindowInfo {
            window_id,
            buffer_id,
            window_start,
            window_end: window_start + 200,
            buffer_size: 10_000,
            bounds,
            mode_line_height: 20.0,
            header_line_height: 0.0,
            tab_line_height: 0.0,
            selected: false,
            is_minibuffer: false,
            char_height: 16.0,
            buffer_file_name: String::new(),
            modified: false,
        }
    }

    // =======================================================================
    // new() - initial state
    // =======================================================================

    #[test]
    fn new_creates_empty_buffer() {
        let buf = FrameGlyphBuffer::new();
        assert!(buf.glyphs.is_empty());
        assert!(buf.window_regions.is_empty());
        assert!(buf.window_infos.is_empty());
        assert!(buf.faces.is_empty());
        assert!(buf.stipple_patterns.is_empty());
        assert!(buf.cursor_inverse.is_none());
        assert!(!buf.layout_changed);
    }

    #[test]
    fn new_has_correct_defaults() {
        let buf = FrameGlyphBuffer::new();
        assert_eq!(buf.width, 0.0);
        assert_eq!(buf.height, 0.0);
        assert_eq!(buf.char_width, 8.0);
        assert_eq!(buf.char_height, 16.0);
        assert_eq!(buf.font_pixel_size, 14.0);
        assert_color_eq(&buf.background, &Color::BLACK);
        assert_eq!(buf.frame_id, 0);
        assert_eq!(buf.parent_id, 0);
        assert_eq!(buf.background_alpha, 1.0);
        assert!(!buf.no_accept_focus);
    }

    #[test]
    fn new_is_empty_and_len_zero() {
        let buf = FrameGlyphBuffer::new();
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
    }

    // =======================================================================
    // with_size()
    // =======================================================================

    #[test]
    fn with_size_sets_dimensions() {
        let buf = FrameGlyphBuffer::with_size(1920.0, 1080.0);
        assert_eq!(buf.width, 1920.0);
        assert_eq!(buf.height, 1080.0);
        // Everything else should match new()
        assert!(buf.glyphs.is_empty());
        assert_eq!(buf.char_width, 8.0);
    }

    // =======================================================================
    // clear_all()
    // =======================================================================

    #[test]
    fn clear_all_resets_glyphs_and_metadata() {
        let mut buf = FrameGlyphBuffer::new();

        // Populate some data
        buf.add_char('A', 0.0, 0.0, 8.0, 16.0, 12.0, false);
        buf.add_stretch(0.0, 0.0, 100.0, 16.0, Color::RED, 0, false);
        buf.add_cursor(
            1,
            10.0,
            20.0,
            2.0,
            16.0,
            CursorStyle::Bar(2.0),
            Color::WHITE,
        );
        buf.add_window_info(
            1,
            100,
            0,
            500,
            1000,
            0.0,
            0.0,
            800.0,
            600.0,
            20.0,
            0.0,
            0.0,
            true,
            false,
            16.0,
            "test.rs".to_string(),
            false,
        );
        buf.set_cursor_inverse(10.0, 20.0, 8.0, 16.0, Color::WHITE, Color::BLACK);
        buf.stipple_patterns.insert(
            1,
            StipplePattern {
                width: 8,
                height: 8,
                bits: vec![0xAA; 8],
            },
        );
        assert!(!buf.glyphs.is_empty());
        assert!(!buf.window_infos.is_empty());

        buf.clear_all();

        assert!(buf.glyphs.is_empty());
        assert!(buf.window_regions.is_empty());
        assert!(buf.window_infos.is_empty());
        assert!(buf.transition_hints.is_empty());
        assert!(buf.effect_hints.is_empty());
        assert!(buf.cursor_inverse.is_none());
        assert!(buf.stipple_patterns.is_empty());
        assert!(buf.faces.is_empty());
    }

    #[test]
    fn clear_all_preserves_frame_dimensions() {
        let mut buf = FrameGlyphBuffer::new();
        buf.begin_frame(1920.0, 1080.0, Color::BLUE);
        buf.add_char('X', 0.0, 0.0, 8.0, 16.0, 12.0, false);

        buf.clear_all();

        // Dimensions and background should survive clear_all
        assert_eq!(buf.width, 1920.0);
        assert_eq!(buf.height, 1080.0);
        assert_color_eq(&buf.background, &Color::BLUE);
    }

    // =======================================================================
    // begin_frame()
    // =======================================================================

    #[test]
    fn begin_frame_sets_dimensions_and_background() {
        let mut buf = FrameGlyphBuffer::new();
        let bg = Color::rgb(0.1, 0.2, 0.3);
        buf.begin_frame(800.0, 600.0, bg);

        assert_eq!(buf.width, 800.0);
        assert_eq!(buf.height, 600.0);
        assert_color_eq(&buf.background, &bg);
    }

    #[test]
    fn begin_frame_clears_glyphs() {
        let mut buf = FrameGlyphBuffer::new();
        buf.add_char('Z', 5.0, 5.0, 8.0, 16.0, 12.0, false);
        assert_eq!(buf.len(), 1);

        buf.begin_frame(800.0, 600.0, Color::BLACK);
        assert!(buf.is_empty());
    }

    #[test]
    fn begin_frame_clears_cursor_inverse() {
        let mut buf = FrameGlyphBuffer::new();
        buf.set_cursor_inverse(0.0, 0.0, 8.0, 16.0, Color::WHITE, Color::BLACK);
        assert!(buf.cursor_inverse.is_some());

        buf.begin_frame(800.0, 600.0, Color::BLACK);
        assert!(buf.cursor_inverse.is_none());
    }

    #[test]
    fn begin_frame_clears_transition_hints() {
        let mut buf = FrameGlyphBuffer::new();
        buf.add_transition_hint(WindowTransitionHint {
            window_id: 1,
            bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
            kind: WindowTransitionKind::Crossfade,
            effect: None,
            easing: None,
        });
        buf.add_effect_hint(WindowEffectHint::TextFadeIn {
            window_id: 1,
            bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
        });
        assert_eq!(buf.transition_hints.len(), 1);
        assert_eq!(buf.effect_hints.len(), 1);

        buf.begin_frame(800.0, 600.0, Color::BLACK);
        assert!(buf.transition_hints.is_empty());
        assert!(buf.effect_hints.is_empty());
    }

    #[test]
    fn take_runtime_hints_drains_transition_and_effect_hints() {
        let mut buf = FrameGlyphBuffer::new();
        buf.add_transition_hint(WindowTransitionHint {
            window_id: 1,
            bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
            kind: WindowTransitionKind::Crossfade,
            effect: None,
            easing: None,
        });
        buf.add_effect_hint(WindowEffectHint::TextFadeIn {
            window_id: 1,
            bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
        });

        let (transition_hints, effect_hints) = buf.take_runtime_hints();
        assert_eq!(transition_hints.len(), 1);
        assert_eq!(effect_hints.len(), 1);
        assert!(buf.transition_hints.is_empty());
        assert!(buf.effect_hints.is_empty());
    }

    #[test]
    fn begin_frame_clears_stipple_patterns_and_faces() {
        let mut buf = FrameGlyphBuffer::new();
        buf.stipple_patterns.insert(
            1,
            StipplePattern {
                width: 4,
                height: 4,
                bits: vec![0xFF; 2],
            },
        );
        buf.faces.insert(1, Face::new(1));

        buf.begin_frame(800.0, 600.0, Color::BLACK);
        assert!(buf.stipple_patterns.is_empty());
        assert!(buf.faces.is_empty());
    }

    #[test]
    fn begin_frame_then_add_then_begin_frame_clears_previous() {
        let mut buf = FrameGlyphBuffer::new();

        // First frame
        buf.begin_frame(800.0, 600.0, Color::BLACK);
        buf.add_char('A', 0.0, 0.0, 8.0, 16.0, 12.0, false);
        buf.add_char('B', 8.0, 0.0, 8.0, 16.0, 12.0, false);
        buf.add_cursor(1, 16.0, 0.0, 2.0, 16.0, CursorStyle::Bar(2.0), Color::WHITE);
        buf.add_stretch(0.0, 16.0, 800.0, 16.0, Color::BLACK, 0, false);
        buf.add_window_info(
            1,
            100,
            0,
            100,
            200,
            0.0,
            0.0,
            800.0,
            600.0,
            20.0,
            0.0,
            0.0,
            true,
            false,
            16.0,
            String::new(),
            false,
        );
        assert_eq!(buf.len(), 4);
        assert_eq!(buf.window_infos.len(), 1);

        // Second frame - should clear all glyphs
        buf.begin_frame(1024.0, 768.0, Color::WHITE);
        assert!(buf.is_empty());
        assert_eq!(buf.width, 1024.0);
        assert_eq!(buf.height, 768.0);
        assert_color_eq(&buf.background, &Color::WHITE);
        // Note: begin_frame does NOT clear window_infos (that's clear_all's job)
    }

    #[test]
    fn set_face_with_font_registers_baseline_render_face() {
        let mut buf = FrameGlyphBuffer::new();
        let fg = Color::rgb(0.8, 0.7, 0.6);
        let bg = Color::rgb(0.1, 0.2, 0.3);
        let ul = Color::rgb(0.9, 0.1, 0.2);

        buf.set_face_with_font(
            42,
            fg,
            Some(bg),
            "DejaVu Sans",
            700,
            true,
            18.0,
            2,
            Some(ul),
            1,
            None,
            0,
            None,
            false,
        );

        let face = buf.faces.get(&42).expect("face entry should exist");
        assert_eq!(face.id, 42);
        assert_eq!(face.font_family, "DejaVu Sans");
        assert_eq!(face.font_size, 18.0);
        assert_eq!(face.font_weight, 700);
        assert!(face.attributes.contains(FaceAttributes::BOLD));
        assert!(face.attributes.contains(FaceAttributes::ITALIC));
        assert!(face.attributes.contains(FaceAttributes::UNDERLINE));
        assert!(face.attributes.contains(FaceAttributes::STRIKE_THROUGH));
        assert_eq!(face.underline_style, UnderlineStyle::Wave);
        assert_eq!(face.underline_color, Some(ul));
        assert_color_eq(&face.foreground, &fg);
        assert_color_eq(&face.background, &bg);
    }

    #[test]
    fn set_face_uses_current_font_context_for_face_entry() {
        let mut buf = FrameGlyphBuffer::new();
        let fg = Color::rgb(0.4, 0.5, 0.6);

        buf.set_face_with_font(
            1, fg, None, "Iosevka", 400, false, 15.0, 0, None, 0, None, 0, None, false,
        );
        buf.set_face(2, fg, None, 600, true, 0, None, 0, None, 1, None);

        let face = buf.faces.get(&2).expect("face entry should exist");
        assert_eq!(face.font_family, "Iosevka");
        assert_eq!(face.font_size, 15.0);
        assert_eq!(face.font_weight, 600);
        assert!(face.attributes.contains(FaceAttributes::ITALIC));
        assert!(face.attributes.contains(FaceAttributes::OVERLINE));
    }

    // =======================================================================
    // add_char()
    // =======================================================================

    #[test]
    fn add_char_appends_char_glyph() {
        let mut buf = FrameGlyphBuffer::new();
        buf.add_char('H', 10.0, 20.0, 8.0, 16.0, 12.0, false);

        assert_eq!(buf.len(), 1);
        match &buf.glyphs[0] {
            FrameGlyph::Char {
                char: ch,
                x,
                y,
                width,
                height,
                ascent,
                composed,
                ..
            } => {
                assert_eq!(*ch, 'H');
                assert_eq!(*x, 10.0);
                assert_eq!(*y, 20.0);
                assert_eq!(*width, 8.0);
                assert_eq!(*height, 16.0);
                assert_eq!(*ascent, 12.0);
                assert!(!buf.glyphs[0].is_overlay());
                assert!(composed.is_none());
            }
            other => panic!("Expected Char glyph, got {:?}", other),
        }
    }

    #[test]
    fn add_char_uses_current_face_attributes() {
        let mut buf = FrameGlyphBuffer::new();
        let fg = Color::rgb(1.0, 0.0, 0.0);
        let bg = Color::rgb(0.0, 0.0, 1.0);
        buf.set_face(
            42,
            fg,
            Some(bg),
            700,
            true,
            1,
            Some(Color::GREEN), // underline
            1,
            Some(Color::RED), // strike-through
            1,
            Some(Color::BLUE), // overline
        );
        buf.set_draw_context(1, GlyphRowRole::ModeLine, None);
        buf.add_char('X', 0.0, 0.0, 8.0, 16.0, 12.0, true);

        match &buf.glyphs[0] {
            FrameGlyph::Char {
                fg: glyph_fg,
                bg: glyph_bg,
                face_id,
                font_weight,
                italic,
                underline,
                strike_through,
                overline,
                underline_color,
                strike_through_color,
                overline_color,
                ..
            } => {
                assert_color_eq(glyph_fg, &fg);
                assert_eq!(*glyph_bg, Some(bg));
                assert_eq!(*face_id, 42);
                assert_eq!(*font_weight, 700);
                assert!(*italic);
                assert_eq!(*underline, 1);
                assert_eq!(*underline_color, Some(Color::GREEN));
                assert_eq!(*strike_through, 1);
                assert_eq!(*strike_through_color, Some(Color::RED));
                assert_eq!(*overline, 1);
                assert_eq!(*overline_color, Some(Color::BLUE));
                assert!(buf.glyphs[0].is_overlay());
            }
            other => panic!("Expected Char glyph, got {:?}", other),
        }
    }

    #[test]
    fn add_char_multiple_appends_in_order() {
        let mut buf = FrameGlyphBuffer::new();
        buf.add_char('A', 0.0, 0.0, 8.0, 16.0, 12.0, false);
        buf.add_char('B', 8.0, 0.0, 8.0, 16.0, 12.0, false);
        buf.add_char('C', 16.0, 0.0, 8.0, 16.0, 12.0, false);

        assert_eq!(buf.len(), 3);
        let chars: Vec<char> = buf
            .glyphs
            .iter()
            .map(|g| match g {
                FrameGlyph::Char { char: ch, .. } => *ch,
                _ => panic!("Expected Char"),
            })
            .collect();
        assert_eq!(chars, vec!['A', 'B', 'C']);
    }

    #[test]
    fn add_char_overlay_flag() {
        let mut buf = FrameGlyphBuffer::new();
        buf.set_draw_context(1, GlyphRowRole::ModeLine, None);
        buf.add_char('M', 0.0, 0.0, 8.0, 16.0, 12.0, true);
        assert!(buf.glyphs[0].is_overlay());

        buf.set_draw_context(1, GlyphRowRole::Text, None);
        buf.add_char('N', 0.0, 0.0, 8.0, 16.0, 12.0, false);
        assert!(!buf.glyphs[1].is_overlay());
    }

    // =======================================================================
    // add_composed_char()
    // =======================================================================

    #[test]
    fn add_composed_char_stores_text_and_base() {
        let mut buf = FrameGlyphBuffer::new();
        // Emoji ZWJ sequence: family emoji
        let composed_text = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        buf.add_composed_char(
            composed_text,
            '\u{1F468}',
            0.0,
            0.0,
            24.0,
            16.0,
            12.0,
            false,
        );

        assert_eq!(buf.len(), 1);
        match &buf.glyphs[0] {
            FrameGlyph::Char {
                char: ch,
                composed,
                width,
                ..
            } => {
                assert_eq!(*ch, '\u{1F468}');
                assert!(composed.is_some());
                assert_eq!(&**composed.as_ref().unwrap(), composed_text);
                assert_eq!(*width, 24.0);
            }
            other => panic!("Expected Char glyph, got {:?}", other),
        }
    }

    #[test]
    fn add_composed_char_uses_current_face() {
        let mut buf = FrameGlyphBuffer::new();
        let fg = Color::rgb(0.5, 0.5, 0.5);
        buf.set_face(10, fg, None, 400, false, 0, None, 0, None, 0, None);
        buf.add_composed_char("e\u{0301}", 'e', 0.0, 0.0, 8.0, 16.0, 12.0, false);

        match &buf.glyphs[0] {
            FrameGlyph::Char {
                face_id,
                fg: glyph_fg,
                bg: glyph_bg,
                ..
            } => {
                assert_eq!(*face_id, 10);
                assert_color_eq(glyph_fg, &fg);
                assert_eq!(*glyph_bg, None);
            }
            other => panic!("Expected Char glyph, got {:?}", other),
        }
    }

    // =======================================================================
    // add_cursor()
    // =======================================================================

    #[test]
    fn add_cursor_appends_cursor_glyph() {
        let mut buf = FrameGlyphBuffer::new();
        let cursor_color = Color::rgb(0.0, 1.0, 0.0);
        buf.add_cursor(
            42,
            100.0,
            200.0,
            2.0,
            16.0,
            CursorStyle::Bar(2.0),
            cursor_color,
        );

        assert_eq!(buf.len(), 1);
        match &buf.glyphs[0] {
            FrameGlyph::Cursor {
                window_id,
                x,
                y,
                width,
                height,
                style,
                color,
            } => {
                assert_eq!(*window_id, 42);
                assert_eq!(*x, 100.0);
                assert_eq!(*y, 200.0);
                assert_eq!(*width, 2.0);
                assert_eq!(*height, 16.0);
                assert_eq!(*style, CursorStyle::Bar(2.0));
                assert_color_eq(color, &cursor_color);
            }
            other => panic!("Expected Cursor glyph, got {:?}", other),
        }
    }

    #[test]
    fn add_cursor_all_styles() {
        let mut buf = FrameGlyphBuffer::new();
        let c = Color::WHITE;
        buf.add_cursor(1, 0.0, 0.0, 8.0, 16.0, CursorStyle::FilledBox, c);
        buf.add_cursor(1, 0.0, 0.0, 8.0, 16.0, CursorStyle::Bar(2.0), c);
        buf.add_cursor(1, 0.0, 0.0, 8.0, 16.0, CursorStyle::Hbar(2.0), c);
        buf.add_cursor(1, 0.0, 0.0, 8.0, 16.0, CursorStyle::Hollow, c);

        assert_eq!(buf.len(), 4);
        let expected = [
            CursorStyle::FilledBox,
            CursorStyle::Bar(2.0),
            CursorStyle::Hbar(2.0),
            CursorStyle::Hollow,
        ];
        for (i, expected_style) in expected.iter().enumerate() {
            match &buf.glyphs[i] {
                FrameGlyph::Cursor { style, .. } => {
                    assert_eq!(style, expected_style, "Cursor {} has wrong style", i);
                }
                other => panic!("Expected Cursor at index {}, got {:?}", i, other),
            }
        }
    }

    #[test]
    fn cursor_glyph_is_not_overlay() {
        let mut buf = FrameGlyphBuffer::new();
        buf.add_cursor(1, 0.0, 0.0, 8.0, 16.0, CursorStyle::FilledBox, Color::WHITE);
        assert!(!buf.glyphs[0].is_overlay());
    }

    // =======================================================================
    // add_stretch()
    // =======================================================================

    #[test]
    fn add_stretch_appends_stretch_glyph() {
        let mut buf = FrameGlyphBuffer::new();
        let bg = Color::rgb(0.2, 0.2, 0.2);
        buf.add_stretch(0.0, 100.0, 800.0, 16.0, bg, 5, false);

        assert_eq!(buf.len(), 1);
        match &buf.glyphs[0] {
            FrameGlyph::Stretch {
                x,
                y,
                width,
                height,
                bg: stretch_bg,
                face_id,
                stipple_id,
                stipple_fg,
                ..
            } => {
                assert_eq!(*x, 0.0);
                assert_eq!(*y, 100.0);
                assert_eq!(*width, 800.0);
                assert_eq!(*height, 16.0);
                assert_color_eq(stretch_bg, &bg);
                assert_eq!(*face_id, 5);
                assert!(!buf.glyphs[0].is_overlay());
                assert_eq!(*stipple_id, 0);
                assert!(stipple_fg.is_none());
            }
            other => panic!("Expected Stretch glyph, got {:?}", other),
        }
    }

    #[test]
    fn add_stretch_overlay() {
        let mut buf = FrameGlyphBuffer::new();
        buf.set_draw_context(1, GlyphRowRole::ModeLine, None);
        buf.add_stretch(0.0, 0.0, 800.0, 20.0, Color::BLUE, 0, true);
        assert!(buf.glyphs[0].is_overlay());
    }

    #[test]
    fn add_stretch_stipple_stores_pattern_info() {
        let mut buf = FrameGlyphBuffer::new();
        let bg = Color::BLACK;
        let fg = Color::WHITE;
        buf.add_stretch_stipple(0.0, 0.0, 100.0, 100.0, bg, fg, 3, false, 7);

        assert_eq!(buf.len(), 1);
        match &buf.glyphs[0] {
            FrameGlyph::Stretch {
                stipple_id,
                stipple_fg,
                ..
            } => {
                assert_eq!(*stipple_id, 7);
                assert_eq!(*stipple_fg, Some(fg));
            }
            other => panic!("Expected Stretch glyph, got {:?}", other),
        }
    }

    // =======================================================================
    // add_window_info()
    // =======================================================================

    #[test]
    fn add_window_info_appends_metadata() {
        let mut buf = FrameGlyphBuffer::new();
        buf.add_window_info(
            0x1234,
            0xABCD,
            1,
            500,
            1000,
            10.0,
            20.0,
            780.0,
            560.0,
            22.0,
            0.0,
            0.0,
            true,
            false,
            16.0,
            "main.rs".to_string(),
            true,
        );

        assert_eq!(buf.window_infos.len(), 1);
        let info = &buf.window_infos[0];
        assert_eq!(info.window_id, 0x1234);
        assert_eq!(info.buffer_id, 0xABCD);
        assert_eq!(info.window_start, 1);
        assert_eq!(info.window_end, 500);
        assert_eq!(info.buffer_size, 1000);
        assert_eq!(info.bounds, Rect::new(10.0, 20.0, 780.0, 560.0));
        assert_eq!(info.mode_line_height, 22.0);
        assert!(info.selected);
        assert!(!info.is_minibuffer);
        assert_eq!(info.char_height, 16.0);
        assert_eq!(info.buffer_file_name, "main.rs");
        assert!(info.modified);
    }

    #[test]
    fn add_window_info_multiple_windows() {
        let mut buf = FrameGlyphBuffer::new();

        // Two side-by-side windows
        buf.add_window_info(
            1,
            100,
            0,
            200,
            500,
            0.0,
            0.0,
            400.0,
            600.0,
            20.0,
            0.0,
            0.0,
            true,
            false,
            16.0,
            "left.rs".to_string(),
            false,
        );
        buf.add_window_info(
            2,
            200,
            0,
            300,
            800,
            400.0,
            0.0,
            400.0,
            600.0,
            20.0,
            0.0,
            0.0,
            false,
            false,
            16.0,
            "right.rs".to_string(),
            true,
        );

        assert_eq!(buf.window_infos.len(), 2);
        assert_eq!(buf.window_infos[0].window_id, 1);
        assert!(buf.window_infos[0].selected);
        assert_eq!(buf.window_infos[1].window_id, 2);
        assert!(!buf.window_infos[1].selected);
    }

    #[test]
    fn add_window_info_minibuffer() {
        let mut buf = FrameGlyphBuffer::new();
        buf.add_window_info(
            99,
            50,
            0,
            0,
            0,
            0.0,
            580.0,
            800.0,
            20.0,
            0.0,
            0.0,
            0.0,
            false,
            true,
            16.0,
            String::new(),
            false,
        );

        let info = &buf.window_infos[0];
        assert!(info.is_minibuffer);
        assert!(!info.selected);
        assert_eq!(info.buffer_file_name, "");
    }

    #[test]
    fn derive_transition_hint_buffer_switch_crossfade() {
        let prev = make_window_info(1, 100, 10, Rect::new(0.0, 0.0, 800.0, 600.0));
        let curr = make_window_info(1, 200, 10, Rect::new(0.0, 0.0, 800.0, 600.0));

        let hint = FrameGlyphBuffer::derive_transition_hint(&prev, &curr).unwrap();
        assert_eq!(hint.window_id, 1);
        assert_eq!(hint.bounds, curr.bounds);
        assert!(matches!(hint.kind, WindowTransitionKind::Crossfade));
    }

    #[test]
    fn derive_transition_hint_scroll_slide() {
        let prev = make_window_info(1, 100, 10, Rect::new(0.0, 0.0, 800.0, 600.0));
        let curr = make_window_info(1, 100, 42, Rect::new(0.0, 0.0, 800.0, 600.0));

        let hint = FrameGlyphBuffer::derive_transition_hint(&prev, &curr).unwrap();
        assert_eq!(hint.window_id, 1);
        match hint.kind {
            WindowTransitionKind::ScrollSlide {
                direction,
                scroll_distance,
            } => {
                assert_eq!(direction, 1);
                assert!(scroll_distance > 0.0);
            }
            other => panic!("expected ScrollSlide, got {:?}", other),
        }
    }

    #[test]
    fn derive_transition_hint_skips_minibuffer() {
        let prev = make_window_info(1, 100, 10, Rect::new(0.0, 0.0, 800.0, 600.0));
        let mut curr = make_window_info(1, 100, 20, Rect::new(0.0, 0.0, 800.0, 600.0));
        curr.is_minibuffer = true;

        assert!(FrameGlyphBuffer::derive_transition_hint(&prev, &curr).is_none());
    }

    // =======================================================================
    // set_face() / set_face_with_font()
    // =======================================================================

    #[test]
    fn set_face_affects_subsequent_chars() {
        let mut buf = FrameGlyphBuffer::new();

        // Default face
        buf.add_char('A', 0.0, 0.0, 8.0, 16.0, 12.0, false);

        // Change face
        let red = Color::rgb(1.0, 0.0, 0.0);
        buf.set_face(5, red, None, 700, true, 0, None, 0, None, 0, None);
        buf.add_char('B', 8.0, 0.0, 8.0, 16.0, 12.0, false);

        // First char uses default face
        match &buf.glyphs[0] {
            FrameGlyph::Char {
                face_id,
                font_weight,
                italic,
                ..
            } => {
                assert_eq!(*face_id, 0);
                assert_eq!(*font_weight, 400);
                assert!(!*italic);
            }
            _ => panic!("Expected Char"),
        }

        // Second char uses newly set face
        match &buf.glyphs[1] {
            FrameGlyph::Char {
                face_id,
                font_weight,
                italic,
                fg,
                ..
            } => {
                assert_eq!(*face_id, 5);
                assert_eq!(*font_weight, 700);
                assert!(*italic);
                assert_color_eq(fg, &red);
            }
            _ => panic!("Expected Char"),
        }
    }

    #[test]
    fn set_face_with_font_stores_font_family() {
        let mut buf = FrameGlyphBuffer::new();
        let fg = Color::WHITE;
        buf.set_face_with_font(
            7,
            fg,
            None,
            "Fira Code",
            400,
            false,
            14.0,
            0,
            None,
            0,
            None,
            0,
            None,
            false,
        );

        // current_font_family is set by set_face_with_font
        assert_eq!(buf.get_current_font_family(), "Fira Code");

        // set_face_with_font now keeps the face table coherent as well.
        assert_eq!(buf.get_face_font(7), "Fira Code");
    }

    #[test]
    fn set_face_with_font_updates_font_size() {
        let mut buf = FrameGlyphBuffer::new();
        buf.set_face_with_font(
            1,
            Color::WHITE,
            None,
            "monospace",
            400,
            false,
            24.0,
            0,
            None,
            0,
            None,
            0,
            None,
            false,
        );
        buf.add_char('A', 0.0, 0.0, 12.0, 24.0, 18.0, false);

        match &buf.glyphs[0] {
            FrameGlyph::Char { font_size, .. } => {
                assert_eq!(*font_size, 24.0);
            }
            _ => panic!("Expected Char"),
        }
    }

    #[test]
    fn get_face_font_reads_from_faces_map() {
        let mut buf = FrameGlyphBuffer::new();

        // No face inserted yet — falls back to "monospace"
        assert_eq!(buf.get_face_font(1), "monospace");

        // Insert faces (as layout engine's apply_face would)
        let mut face1 = Face::new(1);
        face1.font_family = "JetBrains Mono".to_string();
        buf.faces.insert(1, face1);

        assert_eq!(buf.get_face_font(1), "JetBrains Mono");
        assert_eq!(buf.get_face_font(2), "monospace"); // not inserted
    }

    #[test]
    fn set_face_with_font_decoration_attributes() {
        let mut buf = FrameGlyphBuffer::new();
        let ul_color = Color::rgb(1.0, 1.0, 0.0);
        let st_color = Color::rgb(1.0, 0.0, 1.0);
        let ol_color = Color::rgb(0.0, 1.0, 1.0);
        buf.set_face_with_font(
            3,
            Color::WHITE,
            None,
            "monospace",
            400,
            false,
            14.0,
            2,
            Some(ul_color), // wave underline
            1,
            Some(st_color), // strike-through
            1,
            Some(ol_color), // overline
            false,
        );
        buf.add_char('D', 0.0, 0.0, 8.0, 16.0, 12.0, false);

        match &buf.glyphs[0] {
            FrameGlyph::Char {
                underline,
                underline_color,
                strike_through,
                strike_through_color,
                overline,
                overline_color,
                ..
            } => {
                assert_eq!(*underline, 2);
                assert_eq!(*underline_color, Some(ul_color));
                assert_eq!(*strike_through, 1);
                assert_eq!(*strike_through_color, Some(st_color));
                assert_eq!(*overline, 1);
                assert_eq!(*overline_color, Some(ol_color));
            }
            _ => panic!("Expected Char"),
        }
    }

    #[test]
    fn get_current_bg_returns_current_face_bg() {
        let mut buf = FrameGlyphBuffer::new();
        assert_eq!(buf.get_current_bg(), None);

        let bg = Color::rgb(0.1, 0.2, 0.3);
        buf.set_face(
            1,
            Color::WHITE,
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
        assert_eq!(buf.get_current_bg(), Some(bg));
    }

    // =======================================================================
    // set_frame_identity()
    // =======================================================================

    #[test]
    fn set_frame_identity_stores_all_fields() {
        let mut buf = FrameGlyphBuffer::new();
        let border_color = Color::rgb(0.5, 0.5, 0.5);
        buf.set_frame_identity(0x100, 0x200, 50.0, 75.0, 5, 2.0, border_color, true, 0.85);

        assert_eq!(buf.frame_id, 0x100);
        assert_eq!(buf.parent_id, 0x200);
        assert_eq!(buf.parent_x, 50.0);
        assert_eq!(buf.parent_y, 75.0);
        assert_eq!(buf.z_order, 5);
        assert_eq!(buf.border_width, 2.0);
        assert_color_eq(&buf.border_color, &border_color);
        assert!(buf.no_accept_focus);
        assert_eq!(buf.background_alpha, 0.85);
    }

    #[test]
    fn set_frame_identity_root_frame() {
        let mut buf = FrameGlyphBuffer::new();
        buf.set_frame_identity(
            0x100,
            0, // parent_id 0 = root frame
            0.0,
            0.0,
            0,
            0.0,
            Color::BLACK,
            false,
            1.0,
        );

        assert_eq!(buf.frame_id, 0x100);
        assert_eq!(buf.parent_id, 0);
        assert!(!buf.no_accept_focus);
        assert_eq!(buf.background_alpha, 1.0);
    }

    // =======================================================================
    // set_cursor_inverse()
    // =======================================================================

    #[test]
    fn set_cursor_inverse_stores_info() {
        let mut buf = FrameGlyphBuffer::new();
        let cursor_bg = Color::rgb(0.9, 0.9, 0.0);
        let cursor_fg = Color::rgb(0.0, 0.0, 0.0);
        buf.set_cursor_inverse(50.0, 100.0, 8.0, 16.0, cursor_bg, cursor_fg);

        assert!(buf.cursor_inverse.is_some());
        let inv = buf.cursor_inverse.as_ref().unwrap();
        assert_eq!(inv.x, 50.0);
        assert_eq!(inv.y, 100.0);
        assert_eq!(inv.width, 8.0);
        assert_eq!(inv.height, 16.0);
        assert_color_eq(&inv.cursor_bg, &cursor_bg);
        assert_color_eq(&inv.cursor_fg, &cursor_fg);
    }

    // =======================================================================
    // font_size() / set_font_size()
    // =======================================================================

    #[test]
    fn font_size_accessors() {
        let mut buf = FrameGlyphBuffer::new();
        assert_eq!(buf.font_size(), 14.0); // default

        buf.set_font_size(20.0);
        assert_eq!(buf.font_size(), 20.0);

        // Affects subsequently added chars
        buf.add_char('X', 0.0, 0.0, 10.0, 20.0, 15.0, false);
        match &buf.glyphs[0] {
            FrameGlyph::Char { font_size, .. } => assert_eq!(*font_size, 20.0),
            _ => panic!("Expected Char"),
        }
    }

    // =======================================================================
    // start_frame() / end_frame() / take_layout_changed()
    // =======================================================================

    #[test]
    fn start_frame_swaps_window_regions() {
        let mut buf = FrameGlyphBuffer::new();
        buf.add_background(0.0, 0.0, 400.0, 300.0, Color::BLACK);
        buf.add_background(400.0, 0.0, 400.0, 300.0, Color::BLACK);
        assert_eq!(buf.window_regions.len(), 2);
        assert!(buf.prev_window_regions.is_empty());

        buf.start_frame();
        // Previous regions moved to prev, current cleared
        assert_eq!(buf.prev_window_regions.len(), 2);
        assert!(buf.window_regions.is_empty());
    }

    #[test]
    fn end_frame_returns_false() {
        let mut buf = FrameGlyphBuffer::new();
        assert!(!buf.end_frame());
    }

    #[test]
    fn take_layout_changed_returns_and_resets() {
        let mut buf = FrameGlyphBuffer::new();
        assert!(!buf.take_layout_changed());

        buf.layout_changed = true;
        assert!(buf.take_layout_changed());
        assert!(!buf.take_layout_changed()); // second call returns false
    }

    // =======================================================================
    // add_background()
    // =======================================================================

    #[test]
    fn add_background_adds_glyph_and_window_region() {
        let mut buf = FrameGlyphBuffer::new();
        let bg = Color::rgb(0.15, 0.15, 0.15);
        buf.add_background(10.0, 20.0, 780.0, 560.0, bg);

        assert_eq!(buf.len(), 1);
        assert_eq!(buf.window_regions.len(), 1);
        assert_eq!(buf.window_regions[0], Rect::new(10.0, 20.0, 780.0, 560.0));

        match &buf.glyphs[0] {
            FrameGlyph::Background { bounds, color } => {
                assert_eq!(*bounds, Rect::new(10.0, 20.0, 780.0, 560.0));
                assert_color_eq(color, &bg);
            }
            other => panic!("Expected Background glyph, got {:?}", other),
        }
    }

    // =======================================================================
    // add_border()
    // =======================================================================

    #[test]
    fn add_border_appends_border_glyph() {
        let mut buf = FrameGlyphBuffer::new();
        let border_color = Color::rgb(0.3, 0.3, 0.3);
        buf.add_border(400.0, 0.0, 1.0, 600.0, border_color);

        assert_eq!(buf.len(), 1);
        match &buf.glyphs[0] {
            FrameGlyph::Border {
                x,
                y,
                width,
                height,
                color,
                ..
            } => {
                assert_eq!(*x, 400.0);
                assert_eq!(*y, 0.0);
                assert_eq!(*width, 1.0);
                assert_eq!(*height, 600.0);
                assert_color_eq(color, &border_color);
            }
            other => panic!("Expected Border glyph, got {:?}", other),
        }
    }

    #[test]
    fn border_glyph_is_not_overlay() {
        let mut buf = FrameGlyphBuffer::new();
        buf.add_border(0.0, 0.0, 1.0, 100.0, Color::WHITE);
        assert!(!buf.glyphs[0].is_overlay());
    }

    // =======================================================================
    // add_image() / add_video() / add_webkit()
    // =======================================================================

    #[test]
    fn add_image_appends_image_glyph() {
        let mut buf = FrameGlyphBuffer::new();
        buf.add_image(42, 100.0, 200.0, 320.0, 240.0);

        assert_eq!(buf.len(), 1);
        match &buf.glyphs[0] {
            FrameGlyph::Image {
                image_id,
                x,
                y,
                width,
                height,
                ..
            } => {
                assert_eq!(*image_id, 42);
                assert_eq!(*x, 100.0);
                assert_eq!(*y, 200.0);
                assert_eq!(*width, 320.0);
                assert_eq!(*height, 240.0);
            }
            other => panic!("Expected Image glyph, got {:?}", other),
        }
    }

    #[test]
    fn add_video_appends_video_glyph() {
        let mut buf = FrameGlyphBuffer::new();
        buf.add_video(7, 0.0, 0.0, 640.0, 480.0, 0, false);

        match &buf.glyphs[0] {
            FrameGlyph::Video { video_id, .. } => assert_eq!(*video_id, 7),
            other => panic!("Expected Video glyph, got {:?}", other),
        }
    }

    #[test]
    fn add_webkit_appends_webkit_glyph() {
        let mut buf = FrameGlyphBuffer::new();
        buf.add_webkit(99, 0.0, 0.0, 800.0, 600.0);

        match &buf.glyphs[0] {
            FrameGlyph::WebKit { webkit_id, .. } => assert_eq!(*webkit_id, 99),
            other => panic!("Expected WebKit glyph, got {:?}", other),
        }
    }

    // =======================================================================
    // add_scroll_bar()
    // =======================================================================

    #[test]
    fn add_scroll_bar_appends_scrollbar_glyph() {
        let mut buf = FrameGlyphBuffer::new();
        let track = Color::rgb(0.1, 0.1, 0.1);
        let thumb = Color::rgb(0.5, 0.5, 0.5);
        buf.add_scroll_bar(false, 790.0, 0.0, 10.0, 600.0, 50.0, 100.0, track, thumb);

        assert_eq!(buf.len(), 1);
        match &buf.glyphs[0] {
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
                assert!(!*horizontal);
                assert_eq!(*x, 790.0);
                assert_eq!(*y, 0.0);
                assert_eq!(*width, 10.0);
                assert_eq!(*height, 600.0);
                assert_eq!(*thumb_start, 50.0);
                assert_eq!(*thumb_size, 100.0);
                assert_color_eq(track_color, &track);
                assert_color_eq(thumb_color, &thumb);
            }
            other => panic!("Expected ScrollBar glyph, got {:?}", other),
        }
    }

    // =======================================================================
    // is_overlay() dispatch
    // =======================================================================

    #[test]
    fn is_overlay_returns_false_for_non_char_stretch_types() {
        let mut buf = FrameGlyphBuffer::new();
        buf.add_border(0.0, 0.0, 1.0, 100.0, Color::WHITE);
        buf.add_cursor(1, 0.0, 0.0, 8.0, 16.0, CursorStyle::FilledBox, Color::WHITE);
        buf.add_image(1, 0.0, 0.0, 100.0, 100.0);

        for glyph in &buf.glyphs {
            assert!(!glyph.is_overlay());
        }
    }

    // =======================================================================
    // Full frame simulation: realistic multi-window frame
    // =======================================================================

    #[test]
    fn full_frame_simulation() {
        let mut buf = FrameGlyphBuffer::new();
        let frame_bg = Color::rgb(0.12, 0.12, 0.12);

        // Begin frame
        buf.begin_frame(1920.0, 1080.0, frame_bg);
        buf.set_frame_identity(0x1, 0, 0.0, 0.0, 0, 0.0, Color::BLACK, false, 1.0);

        // Window 1: left pane background
        let win_bg = Color::rgb(0.13, 0.13, 0.13);
        buf.add_background(0.0, 0.0, 960.0, 1060.0, win_bg);

        // Window 1: some text
        let text_fg = Color::rgb(0.87, 0.87, 0.87);
        buf.set_face_with_font(
            0, text_fg, None, "Iosevka", 400, false, 14.0, 0, None, 0, None, 0, None, false,
        );
        for (i, ch) in "Hello, Neomacs!".chars().enumerate() {
            buf.add_char(ch, i as f32 * 8.0, 0.0, 8.0, 16.0, 12.0, false);
        }

        // Window 1: cursor
        buf.add_cursor(
            1,
            15.0 * 8.0,
            0.0,
            2.0,
            16.0,
            CursorStyle::Bar(2.0),
            Color::WHITE,
        );
        buf.set_cursor_inverse(15.0 * 8.0, 0.0, 8.0, 16.0, Color::WHITE, Color::BLACK);

        // Vertical border
        buf.add_border(960.0, 0.0, 1.0, 1060.0, Color::rgb(0.3, 0.3, 0.3));

        // Window 2: right pane background
        buf.add_background(961.0, 0.0, 959.0, 1060.0, win_bg);

        // Mode-line (overlay)
        let ml_bg = Color::rgb(0.2, 0.2, 0.3);
        buf.set_face(
            10,
            Color::WHITE,
            Some(ml_bg),
            700,
            false,
            0,
            None,
            0,
            None,
            0,
            None,
        );
        buf.set_draw_context(1, GlyphRowRole::ModeLine, None);
        buf.add_stretch(0.0, 1060.0, 1920.0, 20.0, ml_bg, 10, true);

        // Window infos
        buf.add_window_info(
            1,
            100,
            0,
            500,
            1000,
            0.0,
            0.0,
            960.0,
            1060.0,
            20.0,
            0.0,
            0.0,
            true,
            false,
            16.0,
            "left.rs".to_string(),
            false,
        );
        buf.add_window_info(
            2,
            200,
            0,
            300,
            800,
            961.0,
            0.0,
            959.0,
            1060.0,
            20.0,
            0.0,
            0.0,
            false,
            false,
            16.0,
            "right.rs".to_string(),
            true,
        );

        // Verify totals
        // 15 chars + 1 cursor + 2 backgrounds + 1 border + 1 mode-line stretch = 20
        assert_eq!(buf.len(), 20);
        assert_eq!(buf.window_infos.len(), 2);
        assert_eq!(buf.window_regions.len(), 2);
        assert!(buf.cursor_inverse.is_some());
        assert_eq!(buf.frame_id, 0x1);
        assert_eq!(buf.width, 1920.0);
        assert_eq!(buf.height, 1080.0);

        // Verify overlay count
        let overlay_count = buf.glyphs.iter().filter(|g| g.is_overlay()).count();
        assert_eq!(overlay_count, 1); // just the mode-line stretch
    }
}
