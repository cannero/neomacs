//! Face (text styling) types.

use crate::types::Color;
use bitflags::bitflags;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::os::raw::c_int;

bitflags! {
    /// Face attributes flags
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct FaceAttributes: u32 {
        const BOLD = 1 << 0;
        const ITALIC = 1 << 1;
        const UNDERLINE = 1 << 2;
        const OVERLINE = 1 << 3;
        const STRIKE_THROUGH = 1 << 4;
        const INVERSE = 1 << 5;
        const BOX = 1 << 6;
    }
}

/// Underline style
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnderlineStyle {
    #[default]
    None,
    Line,
    Wave,
    Double,
    Dotted,
    Dashed,
}

/// Box type for face
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BoxType {
    #[default]
    None,
    Line,
    Raised3D,
    Sunken3D,
}

/// A face defines text styling (colors, font, decorations)
#[repr(C)]
#[derive(Debug, Clone)]
pub struct Face {
    /// Face ID
    pub id: u32,

    /// Foreground color
    pub foreground: Color,

    /// Background color
    pub background: Color,

    /// Use the terminal's default foreground instead of `foreground`.
    pub use_default_foreground: bool,

    /// Use the terminal's default background instead of `background`.
    pub use_default_background: bool,

    /// Underline color (if different from foreground)
    pub underline_color: Option<Color>,

    /// Overline color
    pub overline_color: Option<Color>,

    /// Strike-through color
    pub strike_through_color: Option<Color>,

    /// Box color
    pub box_color: Option<Color>,

    /// Font family name
    pub font_family: String,

    /// Font size in points (1/72 inch)
    pub font_size: f32,

    /// Font weight (400 = normal, 700 = bold)
    pub font_weight: u16,

    /// Attribute flags
    pub attributes: FaceAttributes,

    /// Underline style
    pub underline_style: UnderlineStyle,

    /// Box type
    pub box_type: BoxType,

    /// Box line width
    pub box_line_width: i32,

    /// Box corner radius (0 = sharp corners)
    pub box_corner_radius: i32,

    /// Fancy border style (0=solid, 1=rainbow, 2=animated-rainbow, 3=gradient,
    /// 4=glow, 5=neon, 6=dashed, 7=comet, 8=iridescent, 9=fire, 10=heartbeat)
    pub box_border_style: u32,

    /// Animation speed multiplier for fancy border effects (default 1.0)
    pub box_border_speed: f32,

    /// Secondary box color (for gradient, neon, etc.)
    pub box_color2: Option<Color>,

    /// Absolute path to the resolved font file (from Fontconfig), if available.
    /// Used to pre-load the exact font file into cosmic-text's fontdb.
    pub font_file_path: Option<String>,

    /// Font metrics from Emacs's realized font
    /// Font ascent (FONT_BASE) in pixels
    pub font_ascent: i32,
    /// Font descent (FONT_DESCENT) in pixels
    pub font_descent: i32,
    /// Underline position below baseline (font->underline_position)
    pub underline_position: i32,
    /// Underline thickness (font->underline_thickness)
    pub underline_thickness: i32,
}

impl Default for Face {
    fn default() -> Self {
        Self {
            id: 0,
            foreground: Color::WHITE,
            background: Color::BLACK,
            use_default_foreground: false,
            use_default_background: false,
            underline_color: None,
            overline_color: None,
            strike_through_color: None,
            box_color: None,
            font_family: "monospace".to_string(),
            font_size: 12.0,
            font_weight: 400,
            attributes: FaceAttributes::empty(),
            underline_style: UnderlineStyle::None,
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
}

impl Face {
    /// Create a new face with default values
    pub fn new(id: u32) -> Self {
        Self {
            id,
            ..Default::default()
        }
    }

    /// Check if face is bold
    pub fn is_bold(&self) -> bool {
        self.attributes.contains(FaceAttributes::BOLD) || self.font_weight >= 700
    }

    /// Check if face is italic
    pub fn is_italic(&self) -> bool {
        self.attributes.contains(FaceAttributes::ITALIC)
    }

    /// Check if face has underline
    pub fn has_underline(&self) -> bool {
        self.underline_style != UnderlineStyle::None
    }

    /// Get the underline color (foreground if not explicitly set)
    pub fn get_underline_color(&self) -> Color {
        self.underline_color.unwrap_or(self.foreground)
    }

    /// Create a Pango font description string
    pub fn to_pango_font_description(&self) -> String {
        let mut desc = self.font_family.clone();

        if self.is_italic() {
            desc.push_str(" Italic");
        }

        if self.is_bold() {
            desc.push_str(" Bold");
        }

        desc.push_str(&format!(" {}", self.font_size as i32));
        desc
    }
}

/// FFI-safe face data struct, populated by C's `fill_face_data()`.
///
/// This is the canonical bridge type between C (Emacs face system) and Rust.
/// Layout must match the C `struct FaceDataFFI` in `neomacsterm.c`.
#[repr(C)]
#[derive(Debug, Clone, Default)]
pub struct FaceDataFFI {
    /// Face ID
    pub face_id: u32,
    /// Foreground color (sRGB pixel: 0x00RRGGBB)
    pub fg: u32,
    /// Background color (sRGB pixel: 0x00RRGGBB)
    pub bg: u32,
    /// Font family name (null-terminated C string, valid for duration of layout)
    pub font_family: *const c_char,
    /// Font weight (CSS scale: 400=normal, 700=bold)
    pub font_weight: c_int,
    /// Italic flag
    pub italic: c_int,
    /// Font pixel size
    pub font_size: c_int,
    /// Underline style (0=none, 1=single, 2=wave, 3=double, 4=dotted, 5=dashed)
    pub underline_style: c_int,
    /// Underline color (sRGB pixel)
    pub underline_color: u32,
    /// Strike-through (0=none, 1=enabled)
    pub strike_through: c_int,
    /// Strike-through color
    pub strike_through_color: u32,
    /// Overline (0=none, 1=enabled)
    pub overline: c_int,
    /// Overline color
    pub overline_color: u32,
    /// Box type (0=none, 1=line)
    pub box_type: c_int,
    /// Box color
    pub box_color: u32,
    /// Box line width
    pub box_line_width: c_int,
    /// Box corner radius (0 = sharp corners)
    pub box_corner_radius: c_int,
    /// Fancy border style (0=solid, 1=rainbow, 2=animated-rainbow, 3=gradient,
    /// 4=glow, 5=neon, 6=dashed, 7=comet, 8=iridescent, 9=fire, 10=heartbeat)
    pub box_border_style: c_int,
    /// Animation speed multiplier (100 = 1.0x)
    pub box_border_speed: c_int,
    /// Secondary box color (sRGB pixel: 0x00RRGGBB)
    pub box_color2: u32,
    /// Signed box horizontal (top/bottom) line width.
    /// >0: box adds height (borders drawn outside text area).
    /// <0: box drawn within text area (no extra height).
    pub box_h_line_width: c_int,
    /// Extend: face bg extends to end of visual line (0=no, 1=yes)
    pub extend: c_int,
    /// Per-face font character width (0.0 = use window default)
    pub font_char_width: f32,
    /// Per-face font ascent (0.0 = use window default)
    pub font_ascent: f32,
    /// Per-face space width (for tab stop calculations with proportional fonts)
    pub font_space_width: f32,
    /// Whether the face's font is monospace (1=monospace, 0=proportional)
    pub font_is_monospace: c_int,
    /// Stipple bitmap ID (0 = none, positive = 1-based bitmap index)
    pub stipple: c_int,
    /// Overstrike flag (1 = simulate bold by drawing twice at x and x+1)
    pub overstrike: c_int,
    /// Font descent in pixels (FONT_DESCENT)
    pub font_descent: c_int,
    /// Underline position below baseline (font->underline_position, >=1)
    pub underline_position: c_int,
    /// Underline thickness in pixels (font->underline_thickness, >=1)
    pub underline_thickness: c_int,
    /// Absolute path to resolved font file (from Fontconfig), or NULL.
    pub font_file_path: *const c_char,
}

// Safety: FaceDataFFI contains raw pointers that are only valid during
// the FFI call. The to_face() method copies all data into owned types.
unsafe impl Send for FaceDataFFI {}
unsafe impl Sync for FaceDataFFI {}

impl FaceDataFFI {
    /// Convert FFI face data to the Rust `Face` type.
    ///
    /// # Safety
    /// Caller must ensure `font_family` and `font_file_path` pointers
    /// (if non-null) point to valid, null-terminated C strings.
    pub unsafe fn to_face(&self) -> Face {
        let font_family = if !self.font_family.is_null() {
            unsafe { CStr::from_ptr(self.font_family) }
                .to_str()
                .unwrap_or("monospace")
                .to_string()
        } else {
            "monospace".to_string()
        };

        let font_file_path = if !self.font_file_path.is_null() {
            unsafe { CStr::from_ptr(self.font_file_path) }
                .to_str()
                .ok()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        } else {
            None
        };

        let underline_style_code = self.underline_style.max(0) as u8;
        let strike_through = self.strike_through > 0;
        let overline = self.overline > 0;
        let font_weight = self.font_weight.max(0) as u16;

        let mut attrs = FaceAttributes::empty();
        if font_weight >= 700 {
            attrs |= FaceAttributes::BOLD;
        }
        if self.italic != 0 {
            attrs |= FaceAttributes::ITALIC;
        }
        if underline_style_code > 0 {
            attrs |= FaceAttributes::UNDERLINE;
        }
        if strike_through {
            attrs |= FaceAttributes::STRIKE_THROUGH;
        }
        if overline {
            attrs |= FaceAttributes::OVERLINE;
        }
        let box_type = if self.box_type == 1 {
            BoxType::Line
        } else {
            BoxType::None
        };
        if !matches!(box_type, BoxType::None) {
            attrs |= FaceAttributes::BOX;
        }

        let underline_style = match underline_style_code {
            1 => UnderlineStyle::Line,
            2 => UnderlineStyle::Wave,
            3 => UnderlineStyle::Double,
            4 => UnderlineStyle::Dotted,
            5 => UnderlineStyle::Dashed,
            _ => UnderlineStyle::None,
        };

        Face {
            id: self.face_id,
            foreground: Color::from_pixel(self.fg),
            background: Color::from_pixel(self.bg),
            use_default_foreground: false,
            use_default_background: false,
            underline_color: (underline_style_code > 0)
                .then(|| Color::from_pixel(self.underline_color)),
            overline_color: overline.then(|| Color::from_pixel(self.overline_color)),
            strike_through_color: strike_through
                .then(|| Color::from_pixel(self.strike_through_color)),
            box_color: (self.box_type > 0).then(|| Color::from_pixel(self.box_color)),
            font_family,
            font_size: self.font_size.max(0) as f32,
            font_weight,
            attributes: attrs,
            underline_style,
            box_type,
            box_line_width: self.box_line_width,
            box_corner_radius: self.box_corner_radius,
            box_border_style: self.box_border_style.max(0) as u32,
            box_border_speed: self.box_border_speed as f32 / 100.0,
            box_color2: (self.box_color2 != 0).then(|| Color::from_pixel(self.box_color2)),
            font_file_path,
            font_ascent: self.font_ascent as i32,
            font_descent: self.font_descent,
            underline_position: self.underline_position.max(1),
            underline_thickness: self.underline_thickness.max(1),
        }
    }
}

/// Face cache for efficient lookup
#[derive(Debug, Default)]
pub struct FaceCache {
    faces: Vec<Face>,
}

impl FaceCache {
    pub fn new() -> Self {
        Self { faces: Vec::new() }
    }

    /// Get face by ID
    pub fn get(&self, id: u32) -> Option<&Face> {
        self.faces.iter().find(|f| f.id == id)
    }

    /// Get or create a face by ID
    pub fn get_or_create(&mut self, id: u32) -> &Face {
        // Check if exists
        if self.get(id).is_some() {
            return self.get(id).unwrap();
        }
        // Create new
        let face = Face::new(id);
        self.faces.push(face);
        self.faces.last().unwrap()
    }

    /// Add or update a face, returns the face ID
    pub fn insert(&mut self, face: Face) -> u32 {
        let id = face.id;
        if let Some(existing) = self.faces.iter_mut().find(|f| f.id == face.id) {
            *existing = face;
        } else {
            self.faces.push(face);
        }
        id
    }

    /// Get default face (ID 0)
    pub fn default_face(&self) -> Option<&Face> {
        self.get(0)
    }
}

#[cfg(test)]
#[path = "face_test.rs"]
mod tests;
