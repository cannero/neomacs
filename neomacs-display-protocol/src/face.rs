//! Face (text styling) types.

use crate::types::Color;
use bitflags::bitflags;

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

/// Face cache for efficient lookup
#[derive(Debug, Default)]
pub struct FaceCache {
    faces: Vec<Face>,
    next_id: u32,
}

impl FaceCache {
    pub fn new() -> Self {
        Self {
            faces: Vec::new(),
            next_id: 1, // 0 is reserved for default
        }
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
mod tests {
    use super::*;

    #[test]
    fn test_face_creation() {
        let face = Face::new(1);
        assert_eq!(face.id, 1);
        assert!(!face.is_bold());
    }

    #[test]
    fn test_pango_font_desc() {
        let mut face = Face::new(0);
        face.font_family = "DejaVu Sans Mono".to_string();
        face.font_size = 14.0;
        face.attributes = FaceAttributes::BOLD | FaceAttributes::ITALIC;

        let desc = face.to_pango_font_description();
        assert!(desc.contains("DejaVu Sans Mono"));
        assert!(desc.contains("Bold"));
        assert!(desc.contains("Italic"));
        assert!(desc.contains("14"));
    }

    #[test]
    fn test_default_face_values() {
        let face = Face::default();
        assert_eq!(face.id, 0);
        assert_eq!(face.foreground, Color::WHITE);
        assert_eq!(face.background, Color::BLACK);
        assert_eq!(face.font_family, "monospace");
        assert_eq!(face.font_size, 12.0);
        assert_eq!(face.font_weight, 400);
        assert_eq!(face.attributes, FaceAttributes::empty());
        assert_eq!(face.underline_style, UnderlineStyle::None);
        assert_eq!(face.box_type, BoxType::None);
        assert_eq!(face.box_line_width, 0);
        assert_eq!(face.box_corner_radius, 0);
        assert!(face.underline_color.is_none());
        assert!(face.overline_color.is_none());
        assert!(face.strike_through_color.is_none());
        assert!(face.box_color.is_none());
        assert!(face.font_file_path.is_none());
        assert_eq!(face.font_ascent, 0);
        assert_eq!(face.font_descent, 0);
        assert_eq!(face.underline_position, 1);
        assert_eq!(face.underline_thickness, 1);
    }

    #[test]
    fn test_face_foreground_background_colors() {
        let mut face = Face::new(1);
        let red = Color::rgb(1.0, 0.0, 0.0);
        let blue = Color::rgb(0.0, 0.0, 1.0);
        face.foreground = red;
        face.background = blue;
        assert_eq!(face.foreground, Color::RED);
        assert_eq!(face.background, Color::BLUE);
    }

    #[test]
    fn test_bold_via_attribute_flag() {
        let mut face = Face::new(2);
        assert!(!face.is_bold());
        face.attributes |= FaceAttributes::BOLD;
        assert!(face.is_bold());
        // font_weight stays at 400 but is_bold returns true via attribute
        assert_eq!(face.font_weight, 400);
    }

    #[test]
    fn test_bold_via_font_weight() {
        let mut face = Face::new(3);
        assert!(!face.is_bold());
        // Bold via high font_weight without the BOLD attribute flag
        face.font_weight = 700;
        assert!(face.is_bold());
        assert!(!face.attributes.contains(FaceAttributes::BOLD));

        // Extra-bold weight
        face.font_weight = 900;
        assert!(face.is_bold());

        // Semi-bold (600) should NOT be bold
        face.font_weight = 600;
        assert!(!face.is_bold());
    }

    #[test]
    fn test_italic_attribute() {
        let mut face = Face::new(4);
        assert!(!face.is_italic());
        face.attributes |= FaceAttributes::ITALIC;
        assert!(face.is_italic());
    }

    #[test]
    fn test_underline_style_none() {
        let face = Face::new(5);
        assert!(!face.has_underline());
        assert_eq!(face.underline_style, UnderlineStyle::None);
    }

    #[test]
    fn test_underline_style_line() {
        let mut face = Face::new(6);
        face.underline_style = UnderlineStyle::Line;
        assert!(face.has_underline());
    }

    #[test]
    fn test_underline_style_wave() {
        let mut face = Face::new(7);
        face.underline_style = UnderlineStyle::Wave;
        assert!(face.has_underline());
    }

    #[test]
    fn test_underline_style_double() {
        let mut face = Face::new(8);
        face.underline_style = UnderlineStyle::Double;
        assert!(face.has_underline());
    }

    #[test]
    fn test_underline_style_dotted() {
        let mut face = Face::new(9);
        face.underline_style = UnderlineStyle::Dotted;
        assert!(face.has_underline());
    }

    #[test]
    fn test_underline_style_dashed() {
        let mut face = Face::new(10);
        face.underline_style = UnderlineStyle::Dashed;
        assert!(face.has_underline());
    }

    #[test]
    fn test_all_underline_styles_detected() {
        // Verify every non-None variant is detected by has_underline
        let styles = [
            UnderlineStyle::Line,
            UnderlineStyle::Wave,
            UnderlineStyle::Double,
            UnderlineStyle::Dotted,
            UnderlineStyle::Dashed,
        ];
        for style in &styles {
            let mut face = Face::new(0);
            face.underline_style = *style;
            assert!(
                face.has_underline(),
                "has_underline() should be true for {:?}",
                style
            );
        }
        // None should NOT be detected
        let mut face = Face::new(0);
        face.underline_style = UnderlineStyle::None;
        assert!(!face.has_underline());
    }

    #[test]
    fn test_underline_color_fallback_to_foreground() {
        let mut face = Face::new(11);
        face.foreground = Color::RED;
        face.underline_color = None;
        // When no explicit underline color, get_underline_color returns foreground
        assert_eq!(face.get_underline_color(), Color::RED);
    }

    #[test]
    fn test_underline_color_explicit() {
        let mut face = Face::new(12);
        face.foreground = Color::RED;
        face.underline_color = Some(Color::BLUE);
        // When explicit underline color is set, it takes precedence
        assert_eq!(face.get_underline_color(), Color::BLUE);
    }

    #[test]
    fn test_strike_through_attribute() {
        let mut face = Face::new(13);
        assert!(!face.attributes.contains(FaceAttributes::STRIKE_THROUGH));
        face.attributes |= FaceAttributes::STRIKE_THROUGH;
        assert!(face.attributes.contains(FaceAttributes::STRIKE_THROUGH));
    }

    #[test]
    fn test_overline_attribute() {
        let mut face = Face::new(14);
        assert!(!face.attributes.contains(FaceAttributes::OVERLINE));
        face.attributes |= FaceAttributes::OVERLINE;
        assert!(face.attributes.contains(FaceAttributes::OVERLINE));
    }

    #[test]
    fn test_inverse_attribute() {
        let mut face = Face::new(15);
        assert!(!face.attributes.contains(FaceAttributes::INVERSE));
        face.attributes |= FaceAttributes::INVERSE;
        assert!(face.attributes.contains(FaceAttributes::INVERSE));
    }

    #[test]
    fn test_strike_through_and_overline_colors() {
        let mut face = Face::new(16);
        assert!(face.strike_through_color.is_none());
        assert!(face.overline_color.is_none());
        face.strike_through_color = Some(Color::GREEN);
        face.overline_color = Some(Color::BLUE);
        assert_eq!(face.strike_through_color.unwrap(), Color::GREEN);
        assert_eq!(face.overline_color.unwrap(), Color::BLUE);
    }

    #[test]
    fn test_box_attribute_and_types() {
        let mut face = Face::new(17);
        assert_eq!(face.box_type, BoxType::None);
        assert!(!face.attributes.contains(FaceAttributes::BOX));

        // Line box
        face.box_type = BoxType::Line;
        face.attributes |= FaceAttributes::BOX;
        face.box_line_width = 2;
        face.box_corner_radius = 4;
        face.box_color = Some(Color::RED);
        assert!(face.attributes.contains(FaceAttributes::BOX));
        assert_eq!(face.box_type, BoxType::Line);
        assert_eq!(face.box_line_width, 2);
        assert_eq!(face.box_corner_radius, 4);
        assert_eq!(face.box_color.unwrap(), Color::RED);

        // Raised3D box
        face.box_type = BoxType::Raised3D;
        assert_eq!(face.box_type, BoxType::Raised3D);

        // Sunken3D box
        face.box_type = BoxType::Sunken3D;
        assert_eq!(face.box_type, BoxType::Sunken3D);
    }

    #[test]
    fn test_combined_attributes() {
        let mut face = Face::new(18);
        face.attributes = FaceAttributes::BOLD
            | FaceAttributes::ITALIC
            | FaceAttributes::UNDERLINE
            | FaceAttributes::STRIKE_THROUGH
            | FaceAttributes::OVERLINE;
        assert!(face.attributes.contains(FaceAttributes::BOLD));
        assert!(face.attributes.contains(FaceAttributes::ITALIC));
        assert!(face.attributes.contains(FaceAttributes::UNDERLINE));
        assert!(face.attributes.contains(FaceAttributes::STRIKE_THROUGH));
        assert!(face.attributes.contains(FaceAttributes::OVERLINE));
        assert!(!face.attributes.contains(FaceAttributes::INVERSE));
        assert!(!face.attributes.contains(FaceAttributes::BOX));
        assert!(face.is_bold());
        assert!(face.is_italic());
    }

    #[test]
    fn test_pango_font_desc_plain() {
        // No bold, no italic — should just be family + size
        let mut face = Face::new(0);
        face.font_family = "Fira Code".to_string();
        face.font_size = 16.0;
        let desc = face.to_pango_font_description();
        assert_eq!(desc, "Fira Code 16");
    }

    #[test]
    fn test_pango_font_desc_bold_only() {
        let mut face = Face::new(0);
        face.font_family = "monospace".to_string();
        face.font_size = 10.0;
        face.attributes = FaceAttributes::BOLD;
        let desc = face.to_pango_font_description();
        assert_eq!(desc, "monospace Bold 10");
    }

    #[test]
    fn test_pango_font_desc_italic_only() {
        let mut face = Face::new(0);
        face.font_family = "monospace".to_string();
        face.font_size = 10.0;
        face.attributes = FaceAttributes::ITALIC;
        let desc = face.to_pango_font_description();
        assert_eq!(desc, "monospace Italic 10");
    }

    #[test]
    fn test_pango_font_desc_bold_via_weight() {
        // Bold should appear in description when font_weight >= 700 even without BOLD attribute
        let mut face = Face::new(0);
        face.font_family = "serif".to_string();
        face.font_size = 12.0;
        face.font_weight = 700;
        let desc = face.to_pango_font_description();
        assert!(desc.contains("Bold"));
        assert_eq!(desc, "serif Bold 12");
    }

    #[test]
    fn test_pango_font_desc_truncates_size() {
        // font_size 13.7 should be truncated to 13 (cast as i32)
        let mut face = Face::new(0);
        face.font_family = "monospace".to_string();
        face.font_size = 13.7;
        let desc = face.to_pango_font_description();
        assert_eq!(desc, "monospace 13");
    }

    #[test]
    fn test_font_weight_and_slant_values() {
        let mut face = Face::new(19);
        // Test various CSS font weight values
        face.font_weight = 100; // Thin
        assert!(!face.is_bold());
        face.font_weight = 300; // Light
        assert!(!face.is_bold());
        face.font_weight = 400; // Normal
        assert!(!face.is_bold());
        face.font_weight = 500; // Medium
        assert!(!face.is_bold());
        face.font_weight = 600; // Semi-bold
        assert!(!face.is_bold());
        face.font_weight = 700; // Bold
        assert!(face.is_bold());
        face.font_weight = 800; // Extra-bold
        assert!(face.is_bold());
        face.font_weight = 900; // Black
        assert!(face.is_bold());
    }

    #[test]
    fn test_font_metrics() {
        let mut face = Face::new(20);
        face.font_ascent = 14;
        face.font_descent = 4;
        face.underline_position = 2;
        face.underline_thickness = 1;
        assert_eq!(face.font_ascent, 14);
        assert_eq!(face.font_descent, 4);
        assert_eq!(face.underline_position, 2);
        assert_eq!(face.underline_thickness, 1);
    }

    // --- FaceCache tests ---

    #[test]
    fn test_face_cache_new_empty() {
        let cache = FaceCache::new();
        assert!(cache.get(0).is_none());
        assert!(cache.get(1).is_none());
        assert!(cache.default_face().is_none());
    }

    #[test]
    fn test_face_cache_insert_and_get() {
        let mut cache = FaceCache::new();
        let mut face = Face::new(5);
        face.foreground = Color::GREEN;
        cache.insert(face);

        let retrieved = cache.get(5).unwrap();
        assert_eq!(retrieved.id, 5);
        assert_eq!(retrieved.foreground, Color::GREEN);
    }

    #[test]
    fn test_face_cache_insert_updates_existing() {
        let mut cache = FaceCache::new();
        let mut face = Face::new(5);
        face.foreground = Color::GREEN;
        cache.insert(face);

        // Insert again with same ID but different color
        let mut face2 = Face::new(5);
        face2.foreground = Color::RED;
        cache.insert(face2);

        let retrieved = cache.get(5).unwrap();
        assert_eq!(retrieved.foreground, Color::RED);
    }

    #[test]
    fn test_face_cache_get_or_create() {
        let mut cache = FaceCache::new();
        // Should not exist yet
        assert!(cache.get(42).is_none());
        // get_or_create should create it
        let face = cache.get_or_create(42);
        assert_eq!(face.id, 42);
        // Now it should exist
        assert!(cache.get(42).is_some());
    }

    #[test]
    fn test_face_cache_get_or_create_returns_existing() {
        let mut cache = FaceCache::new();
        let mut face = Face::new(7);
        face.font_size = 24.0;
        cache.insert(face);

        // get_or_create should return the existing face, not overwrite
        let retrieved = cache.get_or_create(7);
        assert_eq!(retrieved.font_size, 24.0);
    }

    #[test]
    fn test_face_cache_default_face() {
        let mut cache = FaceCache::new();
        assert!(cache.default_face().is_none());

        let default = Face::new(0);
        cache.insert(default);
        assert!(cache.default_face().is_some());
        assert_eq!(cache.default_face().unwrap().id, 0);
    }

    #[test]
    fn test_face_cache_multiple_faces() {
        let mut cache = FaceCache::new();
        for i in 0..10 {
            let mut face = Face::new(i);
            face.font_size = 10.0 + i as f32;
            cache.insert(face);
        }
        for i in 0..10 {
            let face = cache.get(i).unwrap();
            assert_eq!(face.id, i);
            assert_eq!(face.font_size, 10.0 + i as f32);
        }
        assert!(cache.get(10).is_none());
    }

    // --- Enum default tests ---

    #[test]
    fn test_underline_style_default() {
        let style: UnderlineStyle = Default::default();
        assert_eq!(style, UnderlineStyle::None);
    }

    #[test]
    fn test_box_type_default() {
        let bt: BoxType = Default::default();
        assert_eq!(bt, BoxType::None);
    }

    #[test]
    fn test_face_attributes_bitflags_all() {
        let all = FaceAttributes::BOLD
            | FaceAttributes::ITALIC
            | FaceAttributes::UNDERLINE
            | FaceAttributes::OVERLINE
            | FaceAttributes::STRIKE_THROUGH
            | FaceAttributes::INVERSE
            | FaceAttributes::BOX;
        assert!(all.contains(FaceAttributes::BOLD));
        assert!(all.contains(FaceAttributes::ITALIC));
        assert!(all.contains(FaceAttributes::UNDERLINE));
        assert!(all.contains(FaceAttributes::OVERLINE));
        assert!(all.contains(FaceAttributes::STRIKE_THROUGH));
        assert!(all.contains(FaceAttributes::INVERSE));
        assert!(all.contains(FaceAttributes::BOX));
        // All 7 flags set: bits 0-6
        assert_eq!(all.bits(), 0b1111111);
    }
}
