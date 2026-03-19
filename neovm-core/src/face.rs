//! Face system for text appearance attributes.
//!
//! A *face* defines how text is displayed: foreground/background colors,
//! font weight, slant, underline, etc.  Faces can inherit from each other
//! and are merged at display time.
//!
//! This module provides:
//! - `FaceAttribute` — individual attribute values
//! - `Face` — a collection of attributes (some may be unspecified)
//! - `FaceTable` — global registry mapping names to face definitions
//! - Face merging (overlay face on top of base face)

use crate::emacs_core::intern::resolve_sym;
use crate::emacs_core::value::{Value, next_float_id};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Color
// ---------------------------------------------------------------------------

/// RGBA color in sRGB space (0-255 per channel).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Parse "#RRGGBB" or "#RGB" hex color.
    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.strip_prefix('#')?;
        match s.len() {
            6 => {
                let r = u8::from_str_radix(&s[0..2], 16).ok()?;
                let g = u8::from_str_radix(&s[2..4], 16).ok()?;
                let b = u8::from_str_radix(&s[4..6], 16).ok()?;
                Some(Color::rgb(r, g, b))
            }
            3 => {
                let r = u8::from_str_radix(&s[0..1], 16).ok()? * 17;
                let g = u8::from_str_radix(&s[1..2], 16).ok()? * 17;
                let b = u8::from_str_radix(&s[2..3], 16).ok()? * 17;
                Some(Color::rgb(r, g, b))
            }
            _ => None,
        }
    }

    /// Convert to "#RRGGBB" hex string.
    pub fn to_hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Named color lookup (common X11/Emacs colors).
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "black" => Some(Color::rgb(0, 0, 0)),
            "white" => Some(Color::rgb(255, 255, 255)),
            "red" => Some(Color::rgb(255, 0, 0)),
            "green" => Some(Color::rgb(0, 128, 0)),
            "blue" => Some(Color::rgb(0, 0, 255)),
            "cyan" => Some(Color::rgb(0, 255, 255)),
            "magenta" => Some(Color::rgb(255, 0, 255)),
            "yellow" => Some(Color::rgb(255, 255, 0)),
            "gray" | "grey" => Some(Color::rgb(128, 128, 128)),
            "darkgray" | "darkgrey" => Some(Color::rgb(64, 64, 64)),
            "lightgray" | "lightgrey" => Some(Color::rgb(192, 192, 192)),
            "orange" => Some(Color::rgb(255, 165, 0)),
            "pink" => Some(Color::rgb(255, 192, 203)),
            "brown" => Some(Color::rgb(165, 42, 42)),
            "purple" => Some(Color::rgb(128, 0, 128)),
            "violet" => Some(Color::rgb(238, 130, 238)),
            "gold" => Some(Color::rgb(255, 215, 0)),
            "navy" => Some(Color::rgb(0, 0, 128)),
            "teal" => Some(Color::rgb(0, 128, 128)),
            "olive" => Some(Color::rgb(128, 128, 0)),
            "maroon" => Some(Color::rgb(128, 0, 0)),
            "coral" => Some(Color::rgb(255, 127, 80)),
            "salmon" => Some(Color::rgb(250, 128, 114)),
            "tomato" => Some(Color::rgb(255, 99, 71)),
            "aquamarine" => Some(Color::rgb(127, 255, 212)),
            "turquoise" => Some(Color::rgb(64, 224, 208)),
            "ivory" => Some(Color::rgb(255, 255, 240)),
            "beige" => Some(Color::rgb(245, 245, 220)),
            "khaki" => Some(Color::rgb(240, 230, 140)),
            "wheat" => Some(Color::rgb(245, 222, 179)),
            "tan" => Some(Color::rgb(210, 180, 140)),
            "chocolate" => Some(Color::rgb(210, 105, 30)),
            "firebrick" => Some(Color::rgb(178, 34, 34)),
            "crimson" => Some(Color::rgb(220, 20, 60)),
            "indianred" => Some(Color::rgb(205, 92, 92)),
            "lavender" => Some(Color::rgb(230, 230, 250)),
            "plum" => Some(Color::rgb(221, 160, 221)),
            "orchid" => Some(Color::rgb(218, 112, 214)),
            "thistle" => Some(Color::rgb(216, 191, 216)),
            "linen" => Some(Color::rgb(250, 240, 230)),
            "mintcream" => Some(Color::rgb(245, 255, 250)),
            "snow" => Some(Color::rgb(255, 250, 250)),
            "seashell" => Some(Color::rgb(255, 245, 238)),
            "honeydew" => Some(Color::rgb(240, 255, 240)),
            _ => {
                // X11 greyNN / grayNN colors (grey0-grey100)
                let lower = name.to_lowercase();
                let num_part = lower
                    .strip_prefix("grey")
                    .or_else(|| lower.strip_prefix("gray"));
                if let Some(digits) = num_part {
                    if let Ok(n) = digits.parse::<u32>() {
                        if n <= 100 {
                            let v = (n * 255 / 100) as u8;
                            return Some(Color::rgb(v, v, v));
                        }
                    }
                }
                None
            }
        }
    }

    /// Parse a color spec: hex string or named color.
    pub fn parse(spec: &str) -> Option<Self> {
        if spec.starts_with('#') {
            Self::from_hex(spec)
        } else {
            Self::from_name(spec)
        }
    }
}

// ---------------------------------------------------------------------------
// Underline style
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnderlineStyle {
    Line,
    Wave,
    Dot,
    Dash,
    DoubleLine,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Underline {
    pub style: UnderlineStyle,
    pub color: Option<Color>,
    pub position: Option<i32>,
}

// ---------------------------------------------------------------------------
// Box border
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct BoxBorder {
    pub color: Option<Color>,
    pub width: i32,
    pub style: BoxStyle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxStyle {
    Flat,
    Raised,
    Pressed,
}

// ---------------------------------------------------------------------------
// Font weight / slant / width
// ---------------------------------------------------------------------------

/// CSS-style numeric font weight (100-900).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FontWeight(pub u16);

impl FontWeight {
    pub const THIN: Self = Self(100);
    pub const EXTRA_LIGHT: Self = Self(200);
    pub const LIGHT: Self = Self(300);
    pub const NORMAL: Self = Self(400);
    pub const MEDIUM: Self = Self(500);
    pub const SEMI_BOLD: Self = Self(600);
    pub const BOLD: Self = Self(700);
    pub const EXTRA_BOLD: Self = Self(800);
    pub const BLACK: Self = Self(900);

    pub fn from_symbol(name: &str) -> Option<Self> {
        match name {
            "thin" | "ultra-light" => Some(Self::THIN),
            "extra-light" => Some(Self::EXTRA_LIGHT),
            "light" => Some(Self::LIGHT),
            "normal" | "regular" | "book" => Some(Self::NORMAL),
            "medium" => Some(Self::MEDIUM),
            "semi-bold" | "demi-bold" | "demibold" => Some(Self::SEMI_BOLD),
            "bold" => Some(Self::BOLD),
            "extra-bold" | "ultra-bold" => Some(Self::EXTRA_BOLD),
            "black" | "heavy" | "ultra" => Some(Self::BLACK),
            _ => None,
        }
    }

    pub fn is_bold(&self) -> bool {
        self.0 >= 700
    }
}

/// Font slant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontSlant {
    Normal,
    Italic,
    Oblique,
    ReverseItalic,
    ReverseOblique,
}

impl FontSlant {
    pub fn from_symbol(name: &str) -> Option<Self> {
        match name {
            "normal" | "roman" => Some(Self::Normal),
            "italic" => Some(Self::Italic),
            "oblique" => Some(Self::Oblique),
            "reverse-italic" => Some(Self::ReverseItalic),
            "reverse-oblique" => Some(Self::ReverseOblique),
            _ => None,
        }
    }

    pub fn is_italic(&self) -> bool {
        matches!(self, Self::Italic | Self::Oblique)
    }
}

/// Font width (condensed, normal, expanded).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontWidth {
    UltraCondensed,
    ExtraCondensed,
    Condensed,
    SemiCondensed,
    Normal,
    SemiExpanded,
    Expanded,
    ExtraExpanded,
    UltraExpanded,
}

impl FontWidth {
    pub fn from_symbol(name: &str) -> Option<Self> {
        match name {
            "ultra-condensed" => Some(Self::UltraCondensed),
            "extra-condensed" => Some(Self::ExtraCondensed),
            "condensed" | "compressed" | "narrow" => Some(Self::Condensed),
            "semi-condensed" => Some(Self::SemiCondensed),
            "normal" | "medium" | "regular" => Some(Self::Normal),
            "semi-expanded" => Some(Self::SemiExpanded),
            "expanded" => Some(Self::Expanded),
            "extra-expanded" => Some(Self::ExtraExpanded),
            "ultra-expanded" => Some(Self::UltraExpanded),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Face attribute value (for set_attribute)
// ---------------------------------------------------------------------------

/// A typed face attribute value for `FaceTable::set_attribute()`.
#[derive(Clone, Debug)]
pub enum FaceAttrValue {
    Color(Color),
    Weight(FontWeight),
    Slant(FontSlant),
    Height(FaceHeight),
    Width(FontWidth),
    Underline(Underline),
    Box(BoxBorder),
    Bool(bool),
    Str(String),
    Inherit(Vec<String>),
    Unspecified,
}

// ---------------------------------------------------------------------------
// Face
// ---------------------------------------------------------------------------

/// A face definition.  Fields are `Option` to support partial specification
/// (inheriting unset attributes from the default face).
#[derive(Clone, Debug, Default)]
pub struct Face {
    /// Face name.
    pub name: String,
    /// Foreground color.
    pub foreground: Option<Color>,
    /// Background color.
    pub background: Option<Color>,
    /// Font family name.
    pub family: Option<String>,
    /// Font height in 1/10 pt (e.g. 120 = 12pt).
    /// Can also be a float relative to the default face (e.g. 1.5).
    pub height: Option<FaceHeight>,
    /// Font weight.
    pub weight: Option<FontWeight>,
    /// Font slant.
    pub slant: Option<FontSlant>,
    /// Underline.
    pub underline: Option<Underline>,
    /// Overline (true = draw overline).
    pub overline: Option<bool>,
    /// Overline color (None = use foreground).
    pub overline_color: Option<Color>,
    /// Strike-through.
    pub strike_through: Option<bool>,
    /// Strike-through color (None = use foreground).
    pub strike_through_color: Option<Color>,
    /// Box border.
    pub box_border: Option<BoxBorder>,
    /// Inverse video.
    pub inverse_video: Option<bool>,
    /// Stipple pattern name.
    pub stipple: Option<String>,
    /// Whether to extend face background to end of line.
    pub extend: Option<bool>,
    /// Inherit from these faces (processed in order).
    pub inherit: Vec<String>,
    /// Whether bold is simulated via overstrike.
    pub overstrike: bool,
    /// Face documentation.
    pub doc: Option<String>,
    /// Distant foreground color (used when fg matches bg).
    pub distant_foreground: Option<Color>,
    /// Font foundry name.
    pub foundry: Option<String>,
    /// Font width (condensed/expanded).
    pub width: Option<FontWidth>,
}

/// Height specification.
#[derive(Clone, Debug, PartialEq)]
pub enum FaceHeight {
    /// Absolute height in 1/10 pt.
    Absolute(i32),
    /// Relative to default face (multiplier).
    Relative(f64),
}

impl Face {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ..Default::default()
        }
    }

    /// Merge `overlay` on top of `self`.  Non-None fields in `overlay`
    /// override those in `self`.
    pub fn merge(&self, overlay: &Face) -> Face {
        Face {
            name: self.name.clone(),
            foreground: overlay.foreground.or(self.foreground),
            background: overlay.background.or(self.background),
            family: overlay.family.clone().or_else(|| self.family.clone()),
            height: overlay.height.clone().or_else(|| self.height.clone()),
            weight: overlay.weight.or(self.weight),
            slant: overlay.slant.or(self.slant),
            underline: overlay.underline.clone().or_else(|| self.underline.clone()),
            overline: overlay.overline.or(self.overline),
            strike_through: overlay.strike_through.or(self.strike_through),
            box_border: overlay
                .box_border
                .clone()
                .or_else(|| self.box_border.clone()),
            inverse_video: overlay.inverse_video.or(self.inverse_video),
            stipple: overlay.stipple.clone().or_else(|| self.stipple.clone()),
            extend: overlay.extend.or(self.extend),
            inherit: if overlay.inherit.is_empty() {
                self.inherit.clone()
            } else {
                overlay.inherit.clone()
            },
            overstrike: overlay.overstrike || self.overstrike,
            doc: overlay.doc.clone().or_else(|| self.doc.clone()),
            overline_color: overlay.overline_color.or(self.overline_color),
            strike_through_color: overlay.strike_through_color.or(self.strike_through_color),
            distant_foreground: overlay.distant_foreground.or(self.distant_foreground),
            foundry: overlay.foundry.clone().or_else(|| self.foundry.clone()),
            width: overlay.width.or(self.width),
        }
    }

    /// Effective foreground, accounting for inverse video.
    pub fn effective_foreground(&self) -> Option<Color> {
        if self.inverse_video == Some(true) {
            self.background
        } else {
            self.foreground
        }
    }

    /// Effective background, accounting for inverse video.
    pub fn effective_background(&self) -> Option<Color> {
        if self.inverse_video == Some(true) {
            self.foreground
        } else {
            self.background
        }
    }

    /// Convert to a Lisp plist.
    pub fn to_plist(&self) -> Value {
        let mut items = Vec::new();

        if let Some(fg) = &self.foreground {
            items.push(Value::keyword("foreground-color"));
            items.push(Value::string(fg.to_hex()));
        }
        if let Some(bg) = &self.background {
            items.push(Value::keyword("background-color"));
            items.push(Value::string(bg.to_hex()));
        }
        if let Some(w) = &self.weight {
            items.push(Value::keyword("weight"));
            items.push(Value::Int(w.0 as i64));
        }
        if let Some(s) = &self.slant {
            items.push(Value::keyword("slant"));
            items.push(Value::symbol(match s {
                FontSlant::Normal => "normal",
                FontSlant::Italic => "italic",
                FontSlant::Oblique => "oblique",
                FontSlant::ReverseItalic => "reverse-italic",
                FontSlant::ReverseOblique => "reverse-oblique",
            }));
        }
        if let Some(h) = &self.height {
            items.push(Value::keyword("height"));
            match h {
                FaceHeight::Absolute(n) => items.push(Value::Int(*n as i64)),
                FaceHeight::Relative(f) => items.push(Value::Float(*f, next_float_id())),
            }
        }

        Value::list(items)
    }

    /// Parse face attributes from a Lisp plist.
    pub fn from_plist(name: &str, plist: &[Value]) -> Self {
        let mut face = Face::new(name);
        let mut i = 0;

        while i + 1 < plist.len() {
            let key = match &plist[i] {
                Value::Keyword(id) => resolve_sym(*id),
                Value::Symbol(id) => resolve_sym(*id),
                _ => {
                    i += 2;
                    continue;
                }
            };
            let val = &plist[i + 1];

            match key {
                "foreground" | "foreground-color" => {
                    if let Some(s) = val.as_str() {
                        face.foreground = Color::parse(s);
                    }
                }
                "background" | "background-color" => {
                    if let Some(s) = val.as_str() {
                        face.background = Color::parse(s);
                    }
                }
                "weight" => {
                    if let Some(s) = val.as_symbol_name() {
                        face.weight = FontWeight::from_symbol(s);
                    } else if let Some(n) = val.as_int() {
                        face.weight = Some(FontWeight(n as u16));
                    }
                }
                "slant" => {
                    if let Some(s) = val.as_symbol_name() {
                        face.slant = FontSlant::from_symbol(s);
                    }
                }
                "height" => match val {
                    Value::Int(n) => face.height = Some(FaceHeight::Absolute(*n as i32)),
                    Value::Float(f, _) => face.height = Some(FaceHeight::Relative(*f)),
                    _ => {}
                },
                "family" => {
                    if let Some(s) = val.as_str() {
                        face.family = Some(s.to_string());
                    }
                }
                "underline" => match val {
                    Value::True => {
                        face.underline = Some(Underline {
                            style: UnderlineStyle::Line,
                            color: None,
                            position: None,
                        });
                    }
                    Value::Nil => face.underline = None,
                    _ => {
                        if let Some(s) = val.as_str() {
                            face.underline = Some(Underline {
                                style: UnderlineStyle::Line,
                                color: Color::parse(s),
                                position: None,
                            });
                        }
                    }
                },
                "overline" => {
                    face.overline = Some(val.is_truthy());
                }
                "strike-through" => {
                    face.strike_through = Some(val.is_truthy());
                }
                "inverse-video" => {
                    face.inverse_video = Some(val.is_truthy());
                }
                "extend" => {
                    face.extend = Some(val.is_truthy());
                }
                "inherit" => {
                    if let Some(s) = val.as_symbol_name() {
                        face.inherit = vec![s.to_string()];
                    }
                }
                _ => {}
            }

            i += 2;
        }

        face
    }
}

// ---------------------------------------------------------------------------
// FaceTable
// ---------------------------------------------------------------------------

/// Global face registry.
#[derive(Clone)]
pub struct FaceTable {
    faces: HashMap<String, Face>,
}

impl FaceTable {
    pub fn new() -> Self {
        let mut table = Self {
            faces: HashMap::new(),
        };
        table.register_standard_faces();
        table
    }

    /// Register the standard Emacs faces.
    fn register_standard_faces(&mut self) {
        // default face
        let mut default = Face::new("default");
        default.foreground = Some(Color::rgb(0, 0, 0));
        default.background = Some(Color::rgb(255, 255, 255));
        default.weight = Some(FontWeight::NORMAL);
        default.slant = Some(FontSlant::Normal);
        // 100 = 10pt, matching GNU Emacs's "monospace-10" default
        default.height = Some(FaceHeight::Absolute(100));
        default.family = Some("Monospace".to_string());
        self.define(default);

        // bold
        let mut bold = Face::new("bold");
        bold.weight = Some(FontWeight::BOLD);
        bold.inherit = vec!["default".into()];
        self.define(bold);

        // italic
        let mut italic = Face::new("italic");
        italic.slant = Some(FontSlant::Italic);
        italic.inherit = vec!["default".into()];
        self.define(italic);

        // bold-italic
        let mut bold_italic = Face::new("bold-italic");
        bold_italic.weight = Some(FontWeight::BOLD);
        bold_italic.slant = Some(FontSlant::Italic);
        bold_italic.inherit = vec!["default".into()];
        self.define(bold_italic);

        // underline
        let mut underline = Face::new("underline");
        underline.underline = Some(Underline {
            style: UnderlineStyle::Line,
            color: None,
            position: None,
        });
        underline.inherit = vec!["default".into()];
        self.define(underline);

        // fixed-pitch
        let mut fixed_pitch = Face::new("fixed-pitch");
        fixed_pitch.inherit = vec!["default".into()];
        self.define(fixed_pitch);

        // variable-pitch
        let mut variable_pitch = Face::new("variable-pitch");
        variable_pitch.inherit = vec!["default".into()];
        self.define(variable_pitch);

        // mode-line
        let mut mode_line = Face::new("mode-line");
        mode_line.foreground = Some(Color::rgb(0, 0, 0));
        mode_line.background = Some(Color::rgb(192, 192, 192));
        mode_line.weight = Some(FontWeight::NORMAL);
        mode_line.box_border = Some(BoxBorder {
            color: None,
            width: 1,
            style: BoxStyle::Raised,
        });
        self.define(mode_line);

        // mode-line-inactive
        let mut mode_line_inactive = Face::new("mode-line-inactive");
        mode_line_inactive.foreground = Some(Color::rgb(64, 64, 64));
        mode_line_inactive.background = Some(Color::rgb(224, 224, 224));
        mode_line_inactive.weight = Some(FontWeight::NORMAL);
        self.define(mode_line_inactive);

        // mode-line-highlight
        let mut mode_line_highlight = Face::new("mode-line-highlight");
        mode_line_highlight.box_border = Some(BoxBorder {
            color: Some(Color::rgb(64, 64, 64)),
            width: 2,
            style: BoxStyle::Raised,
        });
        mode_line_highlight.inherit = vec!["highlight".into()];
        self.define(mode_line_highlight);

        // mode-line-emphasis
        let mut mode_line_emphasis = Face::new("mode-line-emphasis");
        mode_line_emphasis.weight = Some(FontWeight::BOLD);
        self.define(mode_line_emphasis);

        // mode-line-buffer-id
        let mut mode_line_buffer_id = Face::new("mode-line-buffer-id");
        mode_line_buffer_id.weight = Some(FontWeight::BOLD);
        self.define(mode_line_buffer_id);

        // header-line
        let mut header = Face::new("header-line");
        header.inherit = vec!["mode-line".into()];
        self.define(header);

        // header-line-highlight
        let mut header_line_highlight = Face::new("header-line-highlight");
        header_line_highlight.inherit = vec!["mode-line-highlight".into()];
        self.define(header_line_highlight);

        // header-line-active
        let mut header_line_active = Face::new("header-line-active");
        header_line_active.inherit = vec!["header-line".into()];
        self.define(header_line_active);

        // header-line-inactive
        let mut header_line_inactive = Face::new("header-line-inactive");
        header_line_inactive.inherit = vec!["header-line".into()];
        self.define(header_line_inactive);

        // highlight
        let mut highlight = Face::new("highlight");
        highlight.background = Some(Color::rgb(180, 210, 240));
        self.define(highlight);

        // region
        let mut region = Face::new("region");
        region.background = Some(Color::rgb(100, 149, 237));
        region.extend = Some(true);
        self.define(region);

        // minibuffer-prompt
        let mut prompt = Face::new("minibuffer-prompt");
        prompt.foreground = Some(Color::rgb(0, 0, 128));
        prompt.weight = Some(FontWeight::BOLD);
        self.define(prompt);

        // cursor
        let mut cursor = Face::new("cursor");
        cursor.background = Some(Color::rgb(0, 0, 0));
        self.define(cursor);

        // fringe
        let mut fringe = Face::new("fringe");
        fringe.background = Some(Color::rgb(240, 240, 240));
        self.define(fringe);

        // vertical-border
        let mut vertical_border = Face::new("vertical-border");
        vertical_border.inherit = vec!["mode-line-inactive".into()];
        self.define(vertical_border);

        // scroll-bar
        self.define(Face::new("scroll-bar"));

        // border
        self.define(Face::new("border"));

        // internal-border
        self.define(Face::new("internal-border"));

        // child-frame-border
        self.define(Face::new("child-frame-border"));

        // line-number
        let mut line_num = Face::new("line-number");
        line_num.foreground = Some(Color::rgb(160, 160, 160));
        line_num.inherit = vec!["default".into()];
        self.define(line_num);

        // line-number-current-line
        let mut line_num_cur = Face::new("line-number-current-line");
        line_num_cur.foreground = Some(Color::rgb(0, 0, 0));
        line_num_cur.weight = Some(FontWeight::BOLD);
        line_num_cur.inherit = vec!["line-number".into()];
        self.define(line_num_cur);

        // shadow
        let mut shadow = Face::new("shadow");
        shadow.foreground = Some(Color::rgb(128, 128, 128));
        self.define(shadow);

        // mouse
        self.define(Face::new("mouse"));

        // tool-bar
        let mut tool_bar = Face::new("tool-bar");
        tool_bar.foreground = Some(Color::rgb(0, 0, 0));
        tool_bar.background = Some(Color::rgb(191, 191, 191));
        tool_bar.box_border = Some(BoxBorder {
            color: None,
            width: 1,
            style: BoxStyle::Raised,
        });
        self.define(tool_bar);

        // tab-bar
        let mut tab_bar = Face::new("tab-bar");
        tab_bar.foreground = Some(Color::rgb(0, 0, 0));
        tab_bar.background = Some(Color::rgb(217, 217, 217));
        tab_bar.inherit = vec!["variable-pitch".into()];
        self.define(tab_bar);

        // tab-line
        let mut tab_line = Face::new("tab-line");
        tab_line.foreground = Some(Color::rgb(0, 0, 0));
        tab_line.background = Some(Color::rgb(217, 217, 217));
        tab_line.inherit = vec!["variable-pitch".into()];
        self.define(tab_line);

        // error
        let mut error = Face::new("error");
        error.foreground = Some(Color::rgb(255, 0, 0));
        error.weight = Some(FontWeight::BOLD);
        self.define(error);

        // warning
        let mut warning = Face::new("warning");
        warning.foreground = Some(Color::rgb(255, 165, 0));
        warning.weight = Some(FontWeight::BOLD);
        self.define(warning);

        // success
        let mut success = Face::new("success");
        success.foreground = Some(Color::rgb(0, 128, 0));
        success.weight = Some(FontWeight::BOLD);
        self.define(success);

        // font-lock faces
        self.define_font_lock(
            "font-lock-comment-face",
            Color::rgb(128, 128, 128),
            Some(FontSlant::Italic),
        );
        self.define_font_lock("font-lock-string-face", Color::rgb(0, 128, 0), None);
        self.define_font_lock("font-lock-keyword-face", Color::rgb(128, 0, 128), None);
        self.define_font_lock("font-lock-function-name-face", Color::rgb(0, 0, 255), None);
        self.define_font_lock(
            "font-lock-variable-name-face",
            Color::rgb(139, 69, 19),
            None,
        );
        self.define_font_lock("font-lock-type-face", Color::rgb(0, 128, 0), None);
        self.define_font_lock("font-lock-constant-face", Color::rgb(0, 128, 128), None);
        self.define_font_lock("font-lock-builtin-face", Color::rgb(128, 0, 128), None);
        self.define_font_lock("font-lock-preprocessor-face", Color::rgb(128, 128, 0), None);
        self.define_font_lock("font-lock-negation-char-face", Color::rgb(255, 0, 0), None);
        self.define_font_lock("font-lock-warning-face", Color::rgb(255, 165, 0), None);
        self.define_font_lock(
            "font-lock-doc-face",
            Color::rgb(128, 128, 0),
            Some(FontSlant::Italic),
        );

        // isearch
        let mut isearch = Face::new("isearch");
        isearch.foreground = Some(Color::rgb(255, 255, 255));
        isearch.background = Some(Color::rgb(205, 92, 92));
        self.define(isearch);

        // lazy-highlight
        let mut lazy = Face::new("lazy-highlight");
        lazy.background = Some(Color::rgb(175, 238, 238));
        self.define(lazy);

        // trailing-whitespace
        let mut tw = Face::new("trailing-whitespace");
        tw.background = Some(Color::rgb(255, 0, 0));
        self.define(tw);

        // region (active selection)
        let mut region = Face::new("region");
        region.background = Some(Color::rgb(60, 100, 180));
        region.foreground = Some(Color::rgb(255, 255, 255));
        self.define(region);

        // isearch (current search match)
        let mut isearch = Face::new("isearch");
        isearch.background = Some(Color::rgb(255, 200, 50));
        isearch.foreground = Some(Color::rgb(0, 0, 0));
        self.define(isearch);

        // lazy-highlight (other search matches)
        let mut lazy = Face::new("lazy-highlight");
        lazy.background = Some(Color::rgb(150, 180, 220));
        self.define(lazy);

        // show-paren-match
        let mut spm = Face::new("show-paren-match");
        spm.background = Some(Color::rgb(180, 210, 255));
        spm.weight = Some(FontWeight::BOLD);
        self.define(spm);

        // show-paren-mismatch
        let mut spmm = Face::new("show-paren-mismatch");
        spmm.foreground = Some(Color::rgb(255, 255, 255));
        spmm.background = Some(Color::rgb(160, 0, 0));
        self.define(spmm);

        // link
        let mut link = Face::new("link");
        link.foreground = Some(Color::rgb(0, 0, 238));
        link.underline = Some(Underline {
            style: UnderlineStyle::Line,
            color: None,
            position: None,
        });
        self.define(link);
    }

    fn define_font_lock(&mut self, name: &str, fg: Color, slant: Option<FontSlant>) {
        let mut face = Face::new(name);
        face.foreground = Some(fg);
        if let Some(s) = slant {
            face.slant = Some(s);
        }
        face.inherit = vec!["default".into()];
        self.define(face);
    }

    /// Define or update a face.
    pub fn define(&mut self, face: Face) {
        self.faces.insert(face.name.clone(), face);
    }

    /// Ensure a face exists (create empty if not present).
    pub fn ensure_face(&mut self, name: &str) {
        if !self.faces.contains_key(name) {
            self.faces.insert(name.to_string(), Face::new(name));
        }
    }

    /// Update a single attribute on a face.
    /// Creates the face if it doesn't exist.
    /// Returns true if the face was actually modified.
    pub fn set_attribute(&mut self, name: &str, attr: &str, value: FaceAttrValue) -> bool {
        self.ensure_face(name);
        let face = self.faces.get_mut(name).unwrap();

        // Helper: set an Option<T> field from the matching FaceAttrValue variant.
        macro_rules! set_option {
            ($field:expr, $variant:ident) => {
                match value {
                    FaceAttrValue::$variant(v) => $field = Some(v),
                    FaceAttrValue::Unspecified => $field = None,
                    _ => return false,
                }
            };
        }

        match attr {
            ":foreground" => set_option!(face.foreground, Color),
            ":background" => set_option!(face.background, Color),
            ":distant-foreground" => set_option!(face.distant_foreground, Color),
            ":weight" => set_option!(face.weight, Weight),
            ":slant" => set_option!(face.slant, Slant),
            ":width" => set_option!(face.width, Width),
            ":height" => set_option!(face.height, Height),
            ":family" => match value {
                FaceAttrValue::Str(s) => face.family = Some(s),
                FaceAttrValue::Unspecified => face.family = None,
                _ => return false,
            },
            ":foundry" => match value {
                FaceAttrValue::Str(s) => face.foundry = Some(s),
                FaceAttrValue::Unspecified => face.foundry = None,
                _ => return false,
            },
            ":underline" => match value {
                FaceAttrValue::Underline(u) => face.underline = Some(u),
                FaceAttrValue::Bool(true) => {
                    face.underline = Some(Underline {
                        style: UnderlineStyle::Line,
                        color: None,
                        position: None,
                    });
                }
                FaceAttrValue::Bool(false) | FaceAttrValue::Unspecified => {
                    face.underline = None;
                }
                _ => face.underline = None,
            },
            ":overline" => match value {
                FaceAttrValue::Bool(b) => face.overline = Some(b),
                FaceAttrValue::Color(c) => {
                    face.overline = Some(true);
                    face.overline_color = Some(c);
                }
                FaceAttrValue::Unspecified => face.overline = None,
                _ => return false,
            },
            ":strike-through" => match value {
                FaceAttrValue::Bool(b) => face.strike_through = Some(b),
                FaceAttrValue::Color(c) => {
                    face.strike_through = Some(true);
                    face.strike_through_color = Some(c);
                }
                FaceAttrValue::Unspecified => face.strike_through = None,
                _ => return false,
            },
            ":box" => set_option!(face.box_border, Box),
            ":inverse-video" => set_option!(face.inverse_video, Bool),
            ":extend" => set_option!(face.extend, Bool),
            ":inherit" => match value {
                FaceAttrValue::Inherit(names) => face.inherit = names,
                FaceAttrValue::Unspecified => face.inherit.clear(),
                _ => return false,
            },
            _ => return false,
        }
        true
    }

    /// Look up a face by name.
    pub fn get(&self, name: &str) -> Option<&Face> {
        self.faces.get(name)
    }

    /// Resolve a face name, merging inherited faces.
    /// Returns a fully-specified face with all inherited attributes filled in.
    pub fn resolve(&self, name: &str) -> Face {
        self.resolve_depth(name, 0)
    }

    fn resolve_depth(&self, name: &str, depth: usize) -> Face {
        if depth > 10 {
            return Face::new(name);
        }

        let Some(face) = self.faces.get(name) else {
            return Face::new(name);
        };

        let mut result = face.clone();

        // Apply inheritance.
        for parent_name in &face.inherit {
            let parent = self.resolve_depth(parent_name, depth + 1);
            // Parent provides defaults — face overrides.
            result = parent.merge(&result);
        }

        result
    }

    /// Resolve face for text: merge a list of face names in order.
    /// Uses raw (non-resolved) faces for overlaying, so only explicitly
    /// set attributes override — inherited attributes don't clobber.
    pub fn merge_faces(&self, face_names: &[&str]) -> Face {
        let default = self.resolve("default");
        let mut result = default;

        for name in face_names {
            // Use the raw face definition (not resolved), so inherited
            // attributes from the parent don't override prior merges.
            if let Some(face) = self.faces.get(*name) {
                result = result.merge(face);
            }
        }

        result
    }

    /// List all defined face names.
    pub fn face_list(&self) -> Vec<String> {
        self.faces.keys().cloned().collect()
    }

    /// Number of defined faces.
    pub fn len(&self) -> usize {
        self.faces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    // pdump accessors
    pub(crate) fn dump_faces(&self) -> &HashMap<String, Face> {
        &self.faces
    }
    pub(crate) fn from_dump(faces: HashMap<String, Face>) -> Self {
        Self { faces }
    }
}

impl Default for FaceTable {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_from_hex() {
        assert_eq!(Color::from_hex("#ff0000"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(Color::from_hex("#00ff00"), Some(Color::rgb(0, 255, 0)));
        assert_eq!(Color::from_hex("#abc"), Some(Color::rgb(170, 187, 204)));
        assert_eq!(Color::from_hex("invalid"), None);
    }

    #[test]
    fn color_to_hex() {
        assert_eq!(Color::rgb(255, 0, 128).to_hex(), "#ff0080");
    }

    #[test]
    fn color_from_name() {
        assert_eq!(Color::from_name("red"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(Color::from_name("RED"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(Color::from_name("nonexistent"), None);
    }

    #[test]
    fn face_merge() {
        let base = Face {
            name: "base".into(),
            foreground: Some(Color::rgb(0, 0, 0)),
            background: Some(Color::rgb(255, 255, 255)),
            ..Default::default()
        };
        let overlay = Face {
            name: "overlay".into(),
            foreground: Some(Color::rgb(255, 0, 0)),
            ..Default::default()
        };

        let merged = base.merge(&overlay);
        assert_eq!(merged.foreground, Some(Color::rgb(255, 0, 0))); // overlay wins
        assert_eq!(merged.background, Some(Color::rgb(255, 255, 255))); // base preserved
    }

    #[test]
    fn face_inverse_video() {
        let face = Face {
            name: "test".into(),
            foreground: Some(Color::rgb(255, 255, 255)),
            background: Some(Color::rgb(0, 0, 0)),
            inverse_video: Some(true),
            ..Default::default()
        };

        assert_eq!(face.effective_foreground(), Some(Color::rgb(0, 0, 0)));
        assert_eq!(face.effective_background(), Some(Color::rgb(255, 255, 255)));
    }

    #[test]
    fn face_table_standard_faces() {
        let table = FaceTable::new();
        assert!(table.get("default").is_some());
        assert!(table.get("bold").is_some());
        assert!(table.get("italic").is_some());
        assert!(table.get("mode-line").is_some());
        assert!(table.get("tool-bar").is_some());
        assert!(table.get("tab-bar").is_some());
        assert!(table.get("tab-line").is_some());
        assert!(table.get("font-lock-keyword-face").is_some());
        assert!(table.len() > 30);
    }

    #[test]
    fn face_table_resolve_inheritance() {
        let table = FaceTable::new();
        let bold = table.resolve("bold");
        assert_eq!(bold.weight, Some(FontWeight::BOLD));
        // Should inherit foreground from default
        assert!(bold.foreground.is_some());
    }

    #[test]
    fn face_table_merge_faces() {
        let table = FaceTable::new();
        let merged = table.merge_faces(&["bold", "italic"]);
        assert_eq!(merged.weight, Some(FontWeight::BOLD));
        assert_eq!(merged.slant, Some(FontSlant::Italic));
    }

    #[test]
    fn face_from_plist() {
        let plist = vec![
            Value::keyword("foreground"),
            Value::string("#ff0000"),
            Value::keyword("weight"),
            Value::symbol("bold"),
            Value::keyword("height"),
            Value::Float(1.5, next_float_id()),
        ];
        let face = Face::from_plist("test", &plist);
        assert_eq!(face.foreground, Some(Color::rgb(255, 0, 0)));
        assert_eq!(face.weight, Some(FontWeight::BOLD));
        assert_eq!(face.height, Some(FaceHeight::Relative(1.5)));
    }

    #[test]
    fn font_weight_from_symbol() {
        assert_eq!(FontWeight::from_symbol("bold"), Some(FontWeight::BOLD));
        assert_eq!(FontWeight::from_symbol("normal"), Some(FontWeight::NORMAL));
        assert!(FontWeight::BOLD.is_bold());
        assert!(!FontWeight::NORMAL.is_bold());
    }

    #[test]
    fn face_table_custom_face() {
        let mut table = FaceTable::new();
        let mut custom = Face::new("my-face");
        custom.foreground = Some(Color::rgb(100, 200, 50));
        custom.inherit = vec!["bold".into()];
        table.define(custom);

        let resolved = table.resolve("my-face");
        assert_eq!(resolved.foreground, Some(Color::rgb(100, 200, 50)));
        assert_eq!(resolved.weight, Some(FontWeight::BOLD)); // inherited
    }

    // --- Color::parse (unified hex + named) ---

    #[test]
    fn color_parse_hex_and_named() {
        // Hex path
        assert_eq!(Color::parse("#ff0000"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(Color::parse("#abc"), Some(Color::rgb(170, 187, 204)));
        // Named color path
        assert_eq!(Color::parse("blue"), Some(Color::rgb(0, 0, 255)));
        assert_eq!(Color::parse("gold"), Some(Color::rgb(255, 215, 0)));
        // Unknown
        assert_eq!(Color::parse("nonexistent"), None);
        assert_eq!(Color::parse("#xyz"), None);
    }

    #[test]
    fn color_from_name_case_insensitive() {
        assert_eq!(Color::from_name("Black"), Some(Color::rgb(0, 0, 0)));
        assert_eq!(Color::from_name("CYAN"), Some(Color::rgb(0, 255, 255)));
        assert_eq!(Color::from_name("Gray"), Some(Color::rgb(128, 128, 128)));
        assert_eq!(Color::from_name("grey"), Some(Color::rgb(128, 128, 128)));
    }

    #[test]
    fn color_from_name_full_palette() {
        // Spot-check a wide range of named colors
        let names_and_expected = [
            ("orange", Color::rgb(255, 165, 0)),
            ("pink", Color::rgb(255, 192, 203)),
            ("navy", Color::rgb(0, 0, 128)),
            ("teal", Color::rgb(0, 128, 128)),
            ("coral", Color::rgb(255, 127, 80)),
            ("ivory", Color::rgb(255, 255, 240)),
            ("wheat", Color::rgb(245, 222, 179)),
            ("crimson", Color::rgb(220, 20, 60)),
            ("lavender", Color::rgb(230, 230, 250)),
            ("snow", Color::rgb(255, 250, 250)),
        ];
        for (name, expected) in names_and_expected {
            assert_eq!(
                Color::from_name(name),
                Some(expected),
                "failed for color: {name}"
            );
        }
    }

    // --- Font weight/slant from_symbol ---

    #[test]
    fn font_weight_from_symbol_all_names() {
        assert_eq!(FontWeight::from_symbol("thin"), Some(FontWeight::THIN));
        assert_eq!(
            FontWeight::from_symbol("ultra-light"),
            Some(FontWeight::THIN)
        );
        assert_eq!(
            FontWeight::from_symbol("extra-light"),
            Some(FontWeight::EXTRA_LIGHT)
        );
        assert_eq!(FontWeight::from_symbol("light"), Some(FontWeight::LIGHT));
        assert_eq!(FontWeight::from_symbol("regular"), Some(FontWeight::NORMAL));
        assert_eq!(FontWeight::from_symbol("book"), Some(FontWeight::NORMAL));
        assert_eq!(FontWeight::from_symbol("medium"), Some(FontWeight::MEDIUM));
        assert_eq!(
            FontWeight::from_symbol("semi-bold"),
            Some(FontWeight::SEMI_BOLD)
        );
        assert_eq!(
            FontWeight::from_symbol("demi-bold"),
            Some(FontWeight::SEMI_BOLD)
        );
        assert_eq!(
            FontWeight::from_symbol("extra-bold"),
            Some(FontWeight::EXTRA_BOLD)
        );
        assert_eq!(FontWeight::from_symbol("black"), Some(FontWeight::BLACK));
        assert_eq!(FontWeight::from_symbol("heavy"), Some(FontWeight::BLACK));
        assert_eq!(FontWeight::from_symbol("unknown"), None);
    }

    #[test]
    fn font_slant_from_symbol_all() {
        assert_eq!(FontSlant::from_symbol("normal"), Some(FontSlant::Normal));
        assert_eq!(FontSlant::from_symbol("roman"), Some(FontSlant::Normal));
        assert_eq!(FontSlant::from_symbol("italic"), Some(FontSlant::Italic));
        assert_eq!(FontSlant::from_symbol("oblique"), Some(FontSlant::Oblique));
        assert_eq!(
            FontSlant::from_symbol("reverse-italic"),
            Some(FontSlant::ReverseItalic)
        );
        assert_eq!(
            FontSlant::from_symbol("reverse-oblique"),
            Some(FontSlant::ReverseOblique)
        );
        assert_eq!(FontSlant::from_symbol("unknown"), None);
        assert!(FontSlant::Italic.is_italic());
        assert!(FontSlant::Oblique.is_italic());
        assert!(!FontSlant::Normal.is_italic());
    }

    // --- Face::to_plist round-trip ---

    #[test]
    fn face_to_plist_contains_set_attrs() {
        let mut face = Face::new("test");
        face.foreground = Some(Color::rgb(255, 0, 0));
        face.weight = Some(FontWeight::BOLD);
        face.slant = Some(FontSlant::Italic);
        face.height = Some(FaceHeight::Absolute(120));

        let plist = face.to_plist();
        let items = crate::emacs_core::value::list_to_vec(&plist).unwrap();
        // Should have keyword-value pairs
        assert!(items.len() >= 8); // 4 attrs * 2
    }

    // --- Merge with underline/box/overline/strike-through ---

    #[test]
    fn face_merge_underline_and_box() {
        let base = Face {
            name: "base".into(),
            underline: Some(Underline {
                style: UnderlineStyle::Line,
                color: None,
                position: None,
            }),
            ..Default::default()
        };
        let overlay = Face {
            name: "over".into(),
            box_border: Some(BoxBorder {
                color: Some(Color::rgb(255, 0, 0)),
                width: 2,
                style: BoxStyle::Flat,
            }),
            overline: Some(true),
            strike_through: Some(true),
            ..Default::default()
        };
        let merged = base.merge(&overlay);
        // base's underline preserved
        assert!(merged.underline.is_some());
        // overlay's box, overline, strike-through applied
        assert_eq!(merged.box_border.as_ref().unwrap().width, 2);
        assert_eq!(merged.overline, Some(true));
        assert_eq!(merged.strike_through, Some(true));
    }

    // --- Multi-level inheritance ---

    #[test]
    fn face_table_multi_level_inheritance() {
        let mut table = FaceTable::new();

        // grandparent: sets foreground
        let mut gp = Face::new("grandparent");
        gp.foreground = Some(Color::rgb(100, 100, 100));
        gp.slant = Some(FontSlant::Italic);
        table.define(gp);

        // parent: inherits grandparent, sets weight
        let mut parent = Face::new("parent");
        parent.weight = Some(FontWeight::BOLD);
        parent.inherit = vec!["grandparent".into()];
        table.define(parent);

        // child: inherits parent, sets background
        let mut child = Face::new("child");
        child.background = Some(Color::rgb(200, 200, 200));
        child.inherit = vec!["parent".into()];
        table.define(child);

        let resolved = table.resolve("child");
        assert_eq!(resolved.background, Some(Color::rgb(200, 200, 200))); // own
        assert_eq!(resolved.weight, Some(FontWeight::BOLD)); // from parent
        assert_eq!(resolved.foreground, Some(Color::rgb(100, 100, 100))); // from grandparent
        assert_eq!(resolved.slant, Some(FontSlant::Italic)); // from grandparent
    }

    // --- from_plist with underline/overline/extend/inherit ---

    #[test]
    fn face_from_plist_underline_and_flags() {
        let plist = vec![
            Value::keyword("underline"),
            Value::True,
            Value::keyword("overline"),
            Value::True,
            Value::keyword("strike-through"),
            Value::True,
            Value::keyword("inverse-video"),
            Value::True,
            Value::keyword("extend"),
            Value::True,
            Value::keyword("inherit"),
            Value::symbol("bold"),
        ];
        let face = Face::from_plist("test", &plist);
        assert!(face.underline.is_some());
        assert_eq!(face.underline.as_ref().unwrap().style, UnderlineStyle::Line);
        assert_eq!(face.overline, Some(true));
        assert_eq!(face.strike_through, Some(true));
        assert_eq!(face.inverse_video, Some(true));
        assert_eq!(face.extend, Some(true));
        assert_eq!(face.inherit, vec!["bold".to_string()]);
    }

    // --- Resolve unknown face returns empty ---

    #[test]
    fn face_table_resolve_unknown_face() {
        let table = FaceTable::new();
        let resolved = table.resolve("nonexistent");
        assert_eq!(resolved.name, "nonexistent");
        assert!(resolved.foreground.is_none());
    }

    // --- face_list and len ---

    #[test]
    fn face_table_face_list() {
        let table = FaceTable::new();
        let list = table.face_list();
        assert!(list.contains(&"default".to_string()));
        assert!(list.contains(&"bold".to_string()));
        assert_eq!(list.len(), table.len());
        assert!(!table.is_empty());
    }
}
