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

use crate::emacs_core::intern::{SymId, resolve_sym};
use crate::emacs_core::value::{Value, ValueKind, next_float_id};
use crate::gc_trace::GcTrace;
use std::collections::{HashMap, HashSet};

// X11 color table generated at compile time from etc/rgb.txt
include!(concat!(env!("OUT_DIR"), "/x11_colors.rs"));

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
            // Fall back to X11 color database (compiled from etc/rgb.txt)
            _ => x11_color_lookup(name).map(|(r, g, b)| Color::rgb(r, g, b)),
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
            "thin" => Some(Self::THIN),
            "ultra-light" | "ultralight" | "extra-light" | "extralight" => Some(Self::EXTRA_LIGHT),
            "light" | "semi-light" | "semilight" | "demilight" => Some(Self::LIGHT),
            "normal" | "regular" | "unspecified" | "book" => Some(Self::NORMAL),
            "medium" => Some(Self::MEDIUM),
            "semi-bold" | "semibold" | "demi-bold" | "demibold" | "demi" => Some(Self::SEMI_BOLD),
            "bold" => Some(Self::BOLD),
            "extra-bold" | "extrabold" | "ultra-bold" | "ultrabold" => Some(Self::EXTRA_BOLD),
            "black" | "heavy" | "ultra-heavy" | "ultraheavy" | "ultra" => Some(Self::BLACK),
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
    Text(Value),
    /// Raw `:inherit` face_ref (symbol/list/plist). `None` means nil /
    /// effectively unspecified. Matches GNU's `LFACE_INHERIT_INDEX` slot.
    Inherit(Option<Value>),
    Unspecified,
}

// ---------------------------------------------------------------------------
// Face
// ---------------------------------------------------------------------------

/// A face definition. Fields are `Option` to support partial specification
/// (inheriting unset attributes from the default face).
///
/// The face name is owned by the surrounding registry key, matching GNU
/// Emacs's frame-local face hash table design.
#[derive(Clone, Debug, Default)]
pub struct Face {
    /// Foreground color.
    pub foreground: Option<Color>,
    /// Background color.
    pub background: Option<Color>,
    /// Font family name.
    pub family: Option<Value>,
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
    /// Lisp stipple value, mirroring GNU face attribute ownership.
    pub stipple: Option<Value>,
    /// Whether to extend face background to end of line.
    pub extend: Option<bool>,
    /// `:inherit` face reference, stored raw matching GNU's
    /// `LFACE_INHERIT_INDEX` slot. `None` means unspecified. When set, the
    /// value is any valid face_ref: a symbol (named face), a list of
    /// face_refs (merged left-to-right by `merge_face_ref`), or a plist of
    /// face attributes. Resolution walks this recursively via
    /// `resolve_face_value_over`, mirroring GNU `xfaces.c:merge_face_ref`.
    pub inherit: Option<Value>,
    /// Whether bold is simulated via overstrike.
    pub overstrike: bool,
    /// Face documentation string or nil-equivalent absence.
    pub doc: Option<Value>,
    /// Distant foreground color (used when fg matches bg).
    pub distant_foreground: Option<Color>,
    /// Font foundry name.
    pub foundry: Option<Value>,
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

fn merge_face_height(
    overlay: Option<&FaceHeight>,
    base: Option<&FaceHeight>,
) -> Option<FaceHeight> {
    match overlay {
        None => base.cloned(),
        Some(FaceHeight::Absolute(height)) => Some(FaceHeight::Absolute(*height)),
        Some(FaceHeight::Relative(scale)) => match base {
            Some(FaceHeight::Absolute(height)) => {
                Some(FaceHeight::Absolute((*scale * *height as f64) as i32))
            }
            Some(FaceHeight::Relative(other_scale)) => {
                Some(FaceHeight::Relative(*scale * *other_scale))
            }
            None => Some(FaceHeight::Relative(*scale)),
        },
    }
}

fn face_symbol_value(name: &str) -> Value {
    Value::symbol(name)
}

fn normalized_face_name_value(value: &Value) -> Option<Value> {
    if let Some(name) = value.as_symbol_name() {
        Some(face_symbol_value(name))
    } else if value.is_string() {
        value
            .as_runtime_string_owned()
            .map(|name| face_symbol_value(&name))
    } else {
        None
    }
}

impl Face {
    pub fn family_runtime_string_owned(&self) -> Option<String> {
        self.family
            .and_then(|value| value.as_runtime_string_owned())
    }

    pub fn foundry_runtime_string_owned(&self) -> Option<String> {
        self.foundry
            .and_then(|value| value.as_runtime_string_owned())
    }

    /// Compatibility constructor for existing call sites. The name is owned
    /// by `FaceTable`, not by `Face` itself.
    pub fn new(_name: &str) -> Self {
        Self::default()
    }

    /// Merge `overlay` on top of `self`.  Non-None fields in `overlay`
    /// override those in `self`.
    pub fn merge(&self, overlay: &Face) -> Face {
        Face {
            foreground: overlay.foreground.or(self.foreground),
            background: overlay.background.or(self.background),
            family: overlay.family.or(self.family),
            height: merge_face_height(overlay.height.as_ref(), self.height.as_ref()),
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
            inherit: overlay.inherit.or(self.inherit),
            overstrike: overlay.overstrike || self.overstrike,
            doc: overlay.doc.clone().or_else(|| self.doc.clone()),
            overline_color: overlay.overline_color.or(self.overline_color),
            strike_through_color: overlay.strike_through_color.or(self.strike_through_color),
            distant_foreground: overlay.distant_foreground.or(self.distant_foreground),
            foundry: overlay.foundry.or(self.foundry),
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
            items.push(Value::fixnum(w.0 as i64));
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
                FaceHeight::Absolute(n) => items.push(Value::fixnum(*n as i64)),
                FaceHeight::Relative(f) => items.push(Value::make_float(*f)),
            }
        }

        Value::list(items)
    }

    /// Parse face attributes from a Lisp plist.
    pub fn from_plist(name: &str, plist: &[Value]) -> Self {
        let mut face = Face::new(name);
        let mut i = 0;

        while i + 1 < plist.len() {
            let key = match plist[i].kind() {
                ValueKind::Symbol(id) => resolve_sym(id),
                _ => {
                    i += 2;
                    continue;
                }
            };
            let key = key.trim_start_matches(':');
            let val = &plist[i + 1];

            match key {
                "foreground" | "foreground-color" => {
                    if let Some(s) = val.as_utf8_str() {
                        face.foreground = Color::parse(s);
                    }
                }
                "background" | "background-color" => {
                    if let Some(s) = val.as_utf8_str() {
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
                "height" => match val.kind() {
                    ValueKind::Fixnum(n) => face.height = Some(FaceHeight::Absolute(n as i32)),
                    ValueKind::Float => face.height = Some(FaceHeight::Relative(val.xfloat())),
                    _ => {}
                },
                "family" => {
                    if val.is_string() {
                        face.family = Some(*val);
                    }
                }
                "underline" => face.underline = parse_underline_value(val),
                "overline" => {
                    if let Some(s) = val.as_utf8_str() {
                        face.overline = Some(true);
                        face.overline_color = Color::parse(s);
                    } else {
                        face.overline = Some(val.is_truthy());
                    }
                }
                "strike-through" => {
                    if let Some(s) = val.as_utf8_str() {
                        face.strike_through = Some(true);
                        face.strike_through_color = Color::parse(s);
                    } else {
                        face.strike_through = Some(val.is_truthy());
                    }
                }
                "inverse-video" => {
                    face.inverse_video = Some(val.is_truthy());
                }
                "extend" => {
                    face.extend = Some(val.is_truthy());
                }
                "inherit" => {
                    // Store the raw face_ref. Matches GNU's
                    // `merge_face_ref` (xfaces.c:2960-2980) which accepts
                    // any face_ref — symbol, list, or plist — and defers
                    // type dispatch to the recursive resolver.
                    face.inherit = if val.is_nil() || val.is_symbol_named("nil") {
                        None
                    } else {
                        Some(*val)
                    };
                }
                "box" => {
                    face.box_border = parse_box_value(val);
                }
                "distant-foreground" => {
                    if let Some(s) = val.as_utf8_str() {
                        face.distant_foreground = Color::parse(s);
                    }
                }
                "foundry" => {
                    if val.is_string() {
                        face.foundry = Some(*val);
                    }
                }
                "width" => {
                    if let Some(s) = val.as_symbol_name() {
                        face.width = FontWidth::from_symbol(s);
                    }
                }
                _ => {}
            }

            i += 2;
        }

        face
    }
}

fn parse_underline_value(value: &Value) -> Option<Underline> {
    match value.kind() {
        ValueKind::T => Some(Underline {
            style: UnderlineStyle::Line,
            color: None,
            position: None,
        }),
        ValueKind::Nil => None,
        _ if value.is_string() => {
            let text = face_runtime_string(value)?;
            Some(Underline {
                style: UnderlineStyle::Line,
                color: Color::parse(&text),
                position: None,
            })
        }
        ValueKind::Cons => {
            let items = crate::emacs_core::value::list_to_vec(value)?;
            let mut style = UnderlineStyle::Line;
            let mut color = None;
            let mut position = None;
            let mut i = 0;
            while i + 1 < items.len() {
                let key = items[i]
                    .as_symbol_name()
                    .unwrap_or("")
                    .trim_start_matches(':');
                let item = &items[i + 1];
                match key {
                    "color" => {
                        color = parse_color_value(item);
                    }
                    "style" => {
                        if let Some(name) = item.as_symbol_name() {
                            style = match name {
                                "wave" => UnderlineStyle::Wave,
                                "double-line" => UnderlineStyle::DoubleLine,
                                "dots" => UnderlineStyle::Dot,
                                "dashes" => UnderlineStyle::Dash,
                                _ => UnderlineStyle::Line,
                            };
                        }
                    }
                    "position" => {
                        if let Some(n) = item.as_fixnum() {
                            position = Some(n as i32);
                        }
                    }
                    _ => {}
                }
                i += 2;
            }
            Some(Underline {
                style,
                color,
                position,
            })
        }
        _ if value.is_truthy() => Some(Underline {
            style: UnderlineStyle::Line,
            color: None,
            position: None,
        }),
        _ => None,
    }
}

fn parse_box_value(value: &Value) -> Option<BoxBorder> {
    match value.kind() {
        ValueKind::T => Some(BoxBorder {
            color: None,
            width: 1,
            style: BoxStyle::Flat,
        }),
        ValueKind::Nil => None,
        ValueKind::Fixnum(n) => Some(BoxBorder {
            color: None,
            width: n as i32,
            style: BoxStyle::Flat,
        }),
        _ if value.is_string() => {
            let text = face_runtime_string(value)?;
            Some(BoxBorder {
                color: Color::parse(&text),
                width: 1,
                style: BoxStyle::Flat,
            })
        }
        ValueKind::Cons => {
            let items = crate::emacs_core::value::list_to_vec(value)?;
            let mut color = None;
            let mut width = 1i32;
            let mut style = BoxStyle::Flat;
            let mut i = 0;
            while i + 1 < items.len() {
                let key = items[i]
                    .as_symbol_name()
                    .unwrap_or("")
                    .trim_start_matches(':');
                let item = &items[i + 1];
                match key {
                    "line-width" => match item.kind() {
                        ValueKind::Fixnum(n) => width = n as i32,
                        ValueKind::Cons => {
                            let pair_car = item.cons_car();
                            let pair_cdr = item.cons_cdr();
                            if let Some(n) = pair_car.as_fixnum() {
                                width = n as i32;
                            }
                        }
                        _ => {}
                    },
                    "color" => {
                        color = parse_color_value(item);
                    }
                    "style" => {
                        if let Some(name) = item.as_symbol_name() {
                            style = match name {
                                "released-button" => BoxStyle::Raised,
                                "pressed-button" => BoxStyle::Pressed,
                                _ => BoxStyle::Flat,
                            };
                        }
                    }
                    _ => {}
                }
                i += 2;
            }
            Some(BoxBorder {
                color,
                width,
                style,
            })
        }
        _ => None,
    }
}

fn face_runtime_string(value: &Value) -> Option<String> {
    value.as_runtime_string_owned()
}

fn parse_color_value(value: &Value) -> Option<Color> {
    face_runtime_string(value).as_deref().and_then(Color::parse)
}

// ---------------------------------------------------------------------------
// Face remapping (face-remapping-alist support)
// ---------------------------------------------------------------------------

/// A single entry in a remapping specification.
///
/// Corresponds to the CDR of an entry in `face-remapping-alist`:
/// - `(FACE . other-face)`        -> `[RemapFace("other-face")]`
/// - `(FACE . (:attr val ...))`   -> `[RemapAttrs(face)]`
/// - `(FACE . (a b (:k v) ...))`  -> mixed list of face names & attr plists
#[derive(Clone, Debug)]
pub enum FaceRemapEntry {
    /// Remap to another named face.
    RemapFace(Value),
    /// Inline attribute plist parsed into a `Face`.
    RemapAttrs(Face),
}

/// Parsed form of the buffer-local `face-remapping-alist`.
///
/// Maps original face name -> ordered list of remapping entries.
/// When resolving face `X`, if `X` is in this map the entries replace the
/// original face definition.
#[derive(Clone, Debug, Default)]
pub struct FaceRemapping {
    map: HashMap<Value, Vec<FaceRemapEntry>>,
}

impl FaceRemapping {
    /// Create an empty (no remapping) instance.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether there are any remappings.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Insert a remapping for the given face name.
    pub fn insert(&mut self, face_name: Value, entries: Vec<FaceRemapEntry>) {
        self.map.insert(face_name, entries);
    }

    /// Look up the remapping entries for a face name.
    pub fn get(&self, face_name: &str) -> Option<&[FaceRemapEntry]> {
        self.map
            .get(&face_symbol_value(face_name))
            .map(|v| v.as_slice())
    }

    /// Parse `face-remapping-alist` from its Lisp value.
    ///
    /// The alist has the form `((FACE . SPEC) ...)` where SPEC can be:
    /// - A symbol (face name)
    /// - A plist `(:attr val ...)`
    /// - A list of specs `(face1 face2 (:attr val ...) ...)`
    pub fn from_lisp(value: &Value) -> Self {
        use crate::emacs_core::value::list_to_vec;

        let mut remapping = Self::new();

        let Some(alist) = list_to_vec(value) else {
            return remapping;
        };

        for entry in &alist {
            // Each entry is (FACE . SPEC) — a cons cell
            if !entry.is_cons() {
                continue;
            };
            let cell_car = entry.cons_car();
            let cell_cdr = entry.cons_cdr();
            let Some(face_name) = normalized_face_name_value(&cell_car) else {
                continue;
            };
            if face_name.is_symbol_named("nil") {
                continue;
            }

            let entries = Self::parse_remap_spec(&cell_cdr);
            if !entries.is_empty() {
                remapping.insert(face_name, entries);
            }
        }

        remapping
    }

    /// Parse a single remapping spec (the CDR of an alist entry).
    fn parse_remap_spec(spec: &Value) -> Vec<FaceRemapEntry> {
        use crate::emacs_core::value::list_to_vec;

        match spec.kind() {
            // Simple symbol remap: (FACE . other-face)
            ValueKind::Symbol(_) | ValueKind::T | ValueKind::String => {
                if let Some(name) = normalized_face_name_value(spec) {
                    if !name.is_symbol_named("nil") {
                        return vec![FaceRemapEntry::RemapFace(name)];
                    }
                }
                Vec::new()
            }
            ValueKind::Nil => Vec::new(),
            // List form: could be a plist or a list of specs
            ValueKind::Cons => {
                let Some(items) = list_to_vec(spec) else {
                    return Vec::new();
                };
                if items.is_empty() {
                    return Vec::new();
                }

                // Check if it's a plist (starts with keyword)
                if items[0].as_keyword_id().is_some() {
                    let face = Face::from_plist("--remap--", &items);
                    return vec![FaceRemapEntry::RemapAttrs(face)];
                }

                // Otherwise it's a list of specs: (face1 face2 (:k v ...) ...)
                let mut entries = Vec::new();
                for item in &items {
                    match item.kind() {
                        ValueKind::Symbol(_) | ValueKind::T | ValueKind::String => {
                            if let Some(name) = normalized_face_name_value(item) {
                                if !name.is_symbol_named("nil") {
                                    entries.push(FaceRemapEntry::RemapFace(name));
                                }
                            }
                        }
                        ValueKind::Cons => {
                            if let Some(sub_items) = list_to_vec(item) {
                                if sub_items.first().is_some_and(|v| v.is_keyword()) {
                                    let face = Face::from_plist("--remap--", &sub_items);
                                    entries.push(FaceRemapEntry::RemapAttrs(face));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                entries
            }
            _ => Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// FaceTable
// ---------------------------------------------------------------------------

/// Global face registry.
#[derive(Clone)]
pub struct FaceTable {
    faces: HashMap<Value, Face>,
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
        // GNU realizes the TTY default face with FACE_TTY_DEFAULT_FG_COLOR /
        // FACE_TTY_DEFAULT_BG_COLOR, exposed to Lisp as "unspecified-fg" /
        // "unspecified-bg".  Keep these colors unset here so the display
        // realization layer can preserve the terminal-default sentinel.
        default.weight = Some(FontWeight::NORMAL);
        default.slant = Some(FontSlant::Normal);
        self.define("default", default);

        // bold
        let mut bold = Face::new("bold");
        bold.weight = Some(FontWeight::BOLD);
        bold.inherit = Some(face_symbol_value("default"));
        self.define("bold", bold);

        // italic
        let mut italic = Face::new("italic");
        italic.slant = Some(FontSlant::Italic);
        italic.inherit = Some(face_symbol_value("default"));
        self.define("italic", italic);

        // bold-italic
        let mut bold_italic = Face::new("bold-italic");
        bold_italic.weight = Some(FontWeight::BOLD);
        bold_italic.slant = Some(FontSlant::Italic);
        bold_italic.inherit = Some(face_symbol_value("default"));
        self.define("bold-italic", bold_italic);

        // underline
        let mut underline = Face::new("underline");
        underline.underline = Some(Underline {
            style: UnderlineStyle::Line,
            color: None,
            position: None,
        });
        underline.inherit = Some(face_symbol_value("default"));
        self.define("underline", underline);

        // fixed-pitch
        let mut fixed_pitch = Face::new("fixed-pitch");
        fixed_pitch.inherit = Some(face_symbol_value("default"));
        self.define("fixed-pitch", fixed_pitch);

        // variable-pitch
        let mut variable_pitch = Face::new("variable-pitch");
        variable_pitch.inherit = Some(face_symbol_value("default"));
        self.define("variable-pitch", variable_pitch);

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
        self.define("mode-line", mode_line);

        // mode-line-inactive
        let mut mode_line_inactive = Face::new("mode-line-inactive");
        mode_line_inactive.foreground = Some(Color::rgb(64, 64, 64));
        mode_line_inactive.background = Some(Color::rgb(224, 224, 224));
        mode_line_inactive.weight = Some(FontWeight::NORMAL);
        self.define("mode-line-inactive", mode_line_inactive);

        // mode-line-highlight
        let mut mode_line_highlight = Face::new("mode-line-highlight");
        mode_line_highlight.box_border = Some(BoxBorder {
            color: Some(Color::rgb(64, 64, 64)),
            width: 2,
            style: BoxStyle::Raised,
        });
        mode_line_highlight.inherit = Some(face_symbol_value("highlight"));
        self.define("mode-line-highlight", mode_line_highlight);

        // mode-line-emphasis
        let mut mode_line_emphasis = Face::new("mode-line-emphasis");
        mode_line_emphasis.weight = Some(FontWeight::BOLD);
        self.define("mode-line-emphasis", mode_line_emphasis);

        // mode-line-buffer-id
        let mut mode_line_buffer_id = Face::new("mode-line-buffer-id");
        mode_line_buffer_id.weight = Some(FontWeight::BOLD);
        self.define("mode-line-buffer-id", mode_line_buffer_id);

        // header-line
        let mut header = Face::new("header-line");
        header.inherit = Some(face_symbol_value("mode-line"));
        self.define("header-line", header);

        // header-line-highlight
        let mut header_line_highlight = Face::new("header-line-highlight");
        header_line_highlight.inherit = Some(face_symbol_value("mode-line-highlight"));
        self.define("header-line-highlight", header_line_highlight);

        // header-line-active
        let mut header_line_active = Face::new("header-line-active");
        header_line_active.inherit = Some(face_symbol_value("header-line"));
        self.define("header-line-active", header_line_active);

        // header-line-inactive
        let mut header_line_inactive = Face::new("header-line-inactive");
        header_line_inactive.inherit = Some(face_symbol_value("header-line"));
        self.define("header-line-inactive", header_line_inactive);

        // highlight
        let mut highlight = Face::new("highlight");
        highlight.background = Some(Color::rgb(180, 210, 240));
        self.define("highlight", highlight);

        // region
        let mut region = Face::new("region");
        region.background = Some(Color::rgb(100, 149, 237));
        region.extend = Some(true);
        self.define("region", region);

        // minibuffer-prompt
        let mut prompt = Face::new("minibuffer-prompt");
        prompt.foreground = Some(Color::rgb(0, 0, 128));
        prompt.weight = Some(FontWeight::BOLD);
        self.define("minibuffer-prompt", prompt);

        // cursor
        let mut cursor = Face::new("cursor");
        cursor.background = Some(Color::rgb(0, 0, 0));
        self.define("cursor", cursor);

        // fringe
        let mut fringe = Face::new("fringe");
        fringe.background = Some(Color::rgb(240, 240, 240));
        self.define("fringe", fringe);

        // vertical-border
        let mut vertical_border = Face::new("vertical-border");
        vertical_border.inherit = Some(face_symbol_value("mode-line-inactive"));
        self.define("vertical-border", vertical_border);

        // scroll-bar
        self.define("scroll-bar", Face::new("scroll-bar"));

        // border
        self.define("border", Face::new("border"));

        // internal-border
        self.define("internal-border", Face::new("internal-border"));

        // child-frame-border
        self.define("child-frame-border", Face::new("child-frame-border"));

        // line-number
        let mut line_num = Face::new("line-number");
        line_num.foreground = Some(Color::rgb(160, 160, 160));
        line_num.inherit = Some(face_symbol_value("default"));
        self.define("line-number", line_num);

        // line-number-current-line
        let mut line_num_cur = Face::new("line-number-current-line");
        line_num_cur.foreground = Some(Color::rgb(0, 0, 0));
        line_num_cur.weight = Some(FontWeight::BOLD);
        line_num_cur.inherit = Some(face_symbol_value("line-number"));
        self.define("line-number-current-line", line_num_cur);

        // shadow
        let mut shadow = Face::new("shadow");
        shadow.foreground = Some(Color::rgb(128, 128, 128));
        self.define("shadow", shadow);

        // mouse
        self.define("mouse", Face::new("mouse"));

        // tool-bar
        let mut tool_bar = Face::new("tool-bar");
        tool_bar.foreground = Some(Color::rgb(0, 0, 0));
        tool_bar.background = Some(Color::rgb(191, 191, 191));
        tool_bar.box_border = Some(BoxBorder {
            color: None,
            width: 1,
            style: BoxStyle::Raised,
        });
        self.define("tool-bar", tool_bar);

        // tab-bar
        let mut tab_bar = Face::new("tab-bar");
        tab_bar.foreground = Some(Color::rgb(0, 0, 0));
        tab_bar.background = Some(Color::rgb(217, 217, 217));
        tab_bar.inherit = Some(face_symbol_value("variable-pitch"));
        self.define("tab-bar", tab_bar);

        // tab-line
        let mut tab_line = Face::new("tab-line");
        tab_line.foreground = Some(Color::rgb(0, 0, 0));
        tab_line.background = Some(Color::rgb(217, 217, 217));
        tab_line.inherit = Some(face_symbol_value("variable-pitch"));
        self.define("tab-line", tab_line);

        // error
        let mut error = Face::new("error");
        error.foreground = Some(Color::rgb(255, 0, 0));
        error.weight = Some(FontWeight::BOLD);
        self.define("error", error);

        // warning
        let mut warning = Face::new("warning");
        warning.foreground = Some(Color::rgb(255, 165, 0));
        warning.weight = Some(FontWeight::BOLD);
        self.define("warning", warning);

        // success
        let mut success = Face::new("success");
        success.foreground = Some(Color::rgb(0, 128, 0));
        success.weight = Some(FontWeight::BOLD);
        self.define("success", success);

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
        self.define("isearch", isearch);

        // lazy-highlight
        let mut lazy = Face::new("lazy-highlight");
        lazy.background = Some(Color::rgb(175, 238, 238));
        self.define("lazy-highlight", lazy);

        // trailing-whitespace
        let mut tw = Face::new("trailing-whitespace");
        tw.background = Some(Color::rgb(255, 0, 0));
        self.define("trailing-whitespace", tw);

        // region (active selection)
        let mut region = Face::new("region");
        region.background = Some(Color::rgb(60, 100, 180));
        region.foreground = Some(Color::rgb(255, 255, 255));
        self.define("region", region);

        // isearch (current search match)
        let mut isearch = Face::new("isearch");
        isearch.background = Some(Color::rgb(255, 200, 50));
        isearch.foreground = Some(Color::rgb(0, 0, 0));
        self.define("isearch", isearch);

        // lazy-highlight (other search matches)
        let mut lazy = Face::new("lazy-highlight");
        lazy.background = Some(Color::rgb(150, 180, 220));
        self.define("lazy-highlight", lazy);

        // show-paren-match
        let mut spm = Face::new("show-paren-match");
        spm.background = Some(Color::rgb(180, 210, 255));
        spm.weight = Some(FontWeight::BOLD);
        self.define("show-paren-match", spm);

        // show-paren-mismatch
        let mut spmm = Face::new("show-paren-mismatch");
        spmm.foreground = Some(Color::rgb(255, 255, 255));
        spmm.background = Some(Color::rgb(160, 0, 0));
        self.define("show-paren-mismatch", spmm);

        // link
        let mut link = Face::new("link");
        link.foreground = Some(Color::rgb(0, 0, 238));
        link.underline = Some(Underline {
            style: UnderlineStyle::Line,
            color: None,
            position: None,
        });
        self.define("link", link);
    }

    fn define_font_lock(&mut self, name: &str, fg: Color, slant: Option<FontSlant>) {
        let mut face = Face::new(name);
        face.foreground = Some(fg);
        if let Some(s) = slant {
            face.slant = Some(s);
        }
        face.inherit = Some(face_symbol_value("default"));
        self.define(name, face);
    }

    /// Define or update a face.
    pub fn define(&mut self, name: &str, face: Face) {
        self.faces.insert(face_symbol_value(name), face);
    }

    /// Ensure a face exists (create empty if not present).
    pub fn ensure_face(&mut self, name: &str) {
        let key = face_symbol_value(name);
        if !self.faces.contains_key(&key) {
            self.faces.insert(key, Face::new(name));
        }
    }

    /// Update a single attribute on a face.
    /// Creates the face if it doesn't exist.
    /// Returns true if the face was actually modified.
    pub fn set_attribute(&mut self, name: &str, attr: &str, value: FaceAttrValue) -> bool {
        self.ensure_face(name);
        let key = face_symbol_value(name);
        let face = self.faces.get_mut(&key).unwrap();

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
                FaceAttrValue::Text(text) => face.family = Some(text),
                FaceAttrValue::Unspecified => face.family = None,
                _ => return false,
            },
            ":foundry" => match value {
                FaceAttrValue::Text(text) => face.foundry = Some(text),
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
                FaceAttrValue::Inherit(v) => face.inherit = v,
                FaceAttrValue::Unspecified => face.inherit = None,
                _ => return false,
            },
            _ => return false,
        }
        true
    }

    /// Look up a face by name.
    pub fn get(&self, name: &str) -> Option<&Face> {
        self.faces.get(&face_symbol_value(name))
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

        let key = face_symbol_value(name);
        let Some(face) = self.faces.get(&key) else {
            return Face::new(name);
        };

        let mut result = face.clone();

        // Apply inheritance. Mirrors GNU `merge_face_vectors` (xfaces.c:2310)
        // which calls `merge_face_ref(from[LFACE_INHERIT_INDEX], to, ...)` —
        // the raw face_ref value is resolved recursively by shape
        // (symbol / list of face_refs / plist of attributes).
        if let Some(inherit_ref) = face.inherit {
            let parent = self.resolve_face_ref(inherit_ref, depth + 1);
            // Parent provides defaults — face overrides.
            result = parent.merge(&result);
        }

        result
    }

    /// Recursively resolve a face_ref value into a `Face` by inheritance.
    ///
    /// Dispatches on the value shape, mirroring GNU `merge_face_ref`
    /// (xfaces.c:2700-3025):
    /// - `nil` / unset → empty face
    /// - symbol → named face lookup (`resolve_depth`)
    /// - list with keyword head → attribute plist (`Face::from_plist`),
    ///   plus recursive resolution of its own `:inherit`
    /// - list with non-keyword head → list of face_refs, merged left-to-right
    ///   (first takes precedence, matching xfaces.c:3005-3014)
    fn resolve_face_ref(&self, face_ref: Value, depth: usize) -> Face {
        if depth > 40 {
            return Face::default();
        }
        if face_ref.is_nil() || face_ref.is_symbol_named("nil") {
            return Face::default();
        }
        if let Some(name) = face_ref.as_symbol_name() {
            return self.resolve_depth(name, depth);
        }
        let Some(items) = crate::emacs_core::value::list_to_vec(&face_ref) else {
            return Face::default();
        };
        if items.is_empty() {
            return Face::default();
        }
        let first_is_keyword = items[0]
            .as_symbol_name()
            .is_some_and(|s| s.starts_with(':'));
        if first_is_keyword {
            // Attribute plist. Parse own attributes, then recursively
            // merge its :inherit chain as parent (parent provides
            // defaults; own attributes already take precedence).
            let own = Face::from_plist("--inline--", &items);
            let parent = match own.inherit {
                Some(inherit_ref) => self.resolve_face_ref(inherit_ref, depth + 1),
                None => Face::default(),
            };
            parent.merge(&own)
        } else {
            // List of face_refs: merge right-to-left so the head
            // (left-most entry) takes precedence — matches GNU
            // xfaces.c:3005-3014 which merges XCDR first, then XCAR.
            let mut result = Face::default();
            for item in items.iter().rev() {
                let next = self.resolve_face_ref(*item, depth + 1);
                result = result.merge(&next);
            }
            result
        }
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
            if let Some(face) = self.faces.get(&face_symbol_value(name)) {
                result = result.merge(face);
            }
        }

        result
    }

    /// Resolve a face name, consulting `face-remapping-alist`.
    ///
    /// If `name` appears in `remapping`, the remapping entries are merged
    /// together (in order) and returned instead of the original face.
    /// Cycle detection prevents infinite loops when a remapping refers
    /// back to the same face (or to another remapped face).
    pub fn resolve_with_remapping(&self, name: &str, remapping: &FaceRemapping) -> Face {
        let mut seen = HashSet::new();
        self.resolve_remapped(name, remapping, &mut seen, 0)
    }

    fn resolve_remapped(
        &self,
        name: &str,
        remapping: &FaceRemapping,
        seen: &mut HashSet<Value>,
        depth: usize,
    ) -> Face {
        if depth > 20 {
            return Face::new(name);
        }

        let key = face_symbol_value(name);
        // Check face-remapping-alist — but only if we haven't already
        // visited this face (cycle detection, matching GNU's
        // push_named_merge_point).
        if !seen.contains(&key) {
            if let Some(entries) = remapping.get(name) {
                seen.insert(key);
                let base = self.resolve("default");
                let mut result = base;
                for entry in entries {
                    match entry {
                        FaceRemapEntry::RemapFace(target) => {
                            if let Some(target_name) = target.as_symbol_name() {
                                let resolved =
                                    self.resolve_remapped(target_name, remapping, seen, depth + 1);
                                result = result.merge(&resolved);
                            }
                        }
                        FaceRemapEntry::RemapAttrs(attrs) => {
                            result = result.merge(attrs);
                        }
                    }
                }
                return result;
            }
        }

        // No remapping — fall back to normal resolution.
        self.resolve_depth(name, 0)
    }

    /// Merge a list of face names, consulting `face-remapping-alist`.
    pub fn merge_faces_with_remapping(
        &self,
        face_names: &[&str],
        remapping: &FaceRemapping,
    ) -> Face {
        let default = self.resolve_with_remapping("default", remapping);
        let mut result = default;

        for name in face_names {
            let mut seen = HashSet::new();
            let resolved = self.resolve_remapped_raw(name, remapping, &mut seen, 0);
            result = result.merge(&resolved);
        }

        result
    }

    /// Like `resolve_remapped` but uses raw (non-inherited) face definitions
    /// when no remapping applies, matching `merge_faces` semantics.
    fn resolve_remapped_raw(
        &self,
        name: &str,
        remapping: &FaceRemapping,
        seen: &mut HashSet<Value>,
        depth: usize,
    ) -> Face {
        if depth > 20 {
            return Face::new(name);
        }

        let key = face_symbol_value(name);
        if !seen.contains(&key) {
            if let Some(entries) = remapping.get(name) {
                seen.insert(key);
                let mut result = Face::new(name);
                for entry in entries {
                    match entry {
                        FaceRemapEntry::RemapFace(target) => {
                            if let Some(target_name) = target.as_symbol_name() {
                                let resolved = self.resolve_remapped_raw(
                                    target_name,
                                    remapping,
                                    seen,
                                    depth + 1,
                                );
                                result = result.merge(&resolved);
                            }
                        }
                        FaceRemapEntry::RemapAttrs(attrs) => {
                            result = result.merge(attrs);
                        }
                    }
                }
                return result;
            }
        }

        // No remapping — use the raw face definition (not resolved).
        self.faces
            .get(&face_symbol_value(name))
            .cloned()
            .unwrap_or_else(|| Face::new(name))
    }

    /// List all defined face names.
    pub fn face_list(&self) -> Vec<String> {
        self.faces
            .keys()
            .filter_map(|value| value.as_symbol_name().map(str::to_string))
            .collect()
    }

    /// Number of defined faces.
    pub fn len(&self) -> usize {
        self.faces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    // pdump accessors
    pub(crate) fn dump_faces_by_sym_id(&self) -> Vec<(SymId, Face)> {
        self.faces
            .iter()
            .filter_map(|(name, face)| name.as_symbol_id().map(|id| (id, face.clone())))
            .collect()
    }

    pub(crate) fn from_dump(faces: HashMap<String, Face>) -> Self {
        Self {
            faces: faces
                .into_iter()
                .map(|(name, face)| (face_symbol_value(&name), face))
                .collect(),
        }
    }

    pub(crate) fn from_dump_sym_ids(faces: Vec<(SymId, Face)>) -> Self {
        Self {
            faces: faces
                .into_iter()
                .map(|(name, face)| (Value::from_sym_id(name), face))
                .collect(),
        }
    }
}

impl Default for FaceTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Root all cells of a face_ref value so nested conses survive GC.
fn trace_face_ref_roots(value: Value, roots: &mut Vec<Value>) {
    roots.push(value);
    if let Some(items) = crate::emacs_core::value::list_to_vec(&value) {
        for item in items {
            if item.is_cons() {
                trace_face_ref_roots(item, roots);
            } else {
                roots.push(item);
            }
        }
    }
}

impl GcTrace for FaceTable {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        roots.extend(self.faces.keys().copied());
        for face in self.faces.values() {
            if let Some(family) = face.family {
                roots.push(family);
            }
            if let Some(foundry) = face.foundry {
                roots.push(foundry);
            }
            if let Some(stipple) = face.stipple {
                roots.push(stipple);
            }
            if let Some(doc) = face.doc {
                roots.push(doc);
            }
            // `:inherit` can be an arbitrary face_ref — walk it so any
            // cons cells in list/plist forms stay rooted across GC.
            if let Some(inherit) = face.inherit {
                trace_face_ref_roots(inherit, roots);
            }
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[path = "face_test.rs"]
mod tests;
