//! Basic types used throughout the display engine.

use std::ops::{Add, Sub};

/// RGBA color with f32 components (0.0 - 1.0)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self::new(r, g, b, 1.0)
    }

    pub fn from_u8(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: a as f32 / 255.0,
        }
    }

    /// Convert from Emacs pixel value (0xAARRGGBB or 0x00RRGGBB).
    /// Performs sRGB→linear conversion since Emacs colors are sRGB
    /// and the GPU surface uses an sRGB format (expects linear values).
    pub fn from_pixel(pixel: u32) -> Self {
        let a = ((pixel >> 24) & 0xFF) as u8;
        let r = ((pixel >> 16) & 0xFF) as u8;
        let g = ((pixel >> 8) & 0xFF) as u8;
        let b = (pixel & 0xFF) as u8;
        // If alpha is 0, assume fully opaque
        let a = if a == 0 { 255 } else { a };
        Self::from_u8(r, g, b, a).srgb_to_linear()
    }

    /// Convert a single sRGB component (0.0-1.0) to linear space.
    fn srgb_component_to_linear(c: f32) -> f32 {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    /// Convert this color from sRGB to linear space.
    /// Use when colors come from Emacs (sRGB) and need to be used with
    /// an sRGB surface format where the GPU expects linear values.
    pub fn srgb_to_linear(self) -> Self {
        Self {
            r: Self::srgb_component_to_linear(self.r),
            g: Self::srgb_component_to_linear(self.g),
            b: Self::srgb_component_to_linear(self.b),
            a: self.a, // alpha is always linear
        }
    }

    /// Convert a single linear component (0.0-1.0) back to sRGB.
    /// Inverse of `srgb_component_to_linear`.
    fn linear_component_to_srgb(c: f32) -> f32 {
        if c <= 0.0031308 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        }
    }

    /// Convert this color from linear space back to sRGB.
    ///
    /// Use at output boundaries that expect sRGB values, such as
    /// the TTY rasterizer which emits 24-bit ANSI color codes
    /// interpreted as sRGB by terminals. The GPU surface
    /// (`Bgra8UnormSrgb`) handles this conversion automatically,
    /// so only non-GPU outputs need it.
    ///
    /// Mirrors the architectural boundary in GNU Emacs where
    /// face pixel values (sRGB) are emitted directly to the TTY
    /// via terminfo `setab`/`setaf` with no conversion
    /// (`src/term.c::tty_defined_color`), while the X11/Cairo
    /// backend lets the server apply gamma at draw time.
    pub fn linear_to_srgb(self) -> Self {
        Self {
            r: Self::linear_component_to_srgb(self.r),
            g: Self::linear_component_to_srgb(self.g),
            b: Self::linear_component_to_srgb(self.b),
            a: self.a,
        }
    }

    // Common colors
    pub const BLACK: Self = Self::rgb(0.0, 0.0, 0.0);
    pub const WHITE: Self = Self::rgb(1.0, 1.0, 1.0);
    pub const RED: Self = Self::rgb(1.0, 0.0, 0.0);
    pub const GREEN: Self = Self::rgb(0.0, 1.0, 0.0);
    pub const BLUE: Self = Self::rgb(0.0, 0.0, 1.0);
    pub const TRANSPARENT: Self = Self::new(0.0, 0.0, 0.0, 0.0);
}

impl Default for Color {
    fn default() -> Self {
        Self::BLACK
    }
}

/// 2D point with f32 coordinates
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub const ZERO: Self = Self::new(0.0, 0.0);
}

impl Add for Point {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y)
    }
}

impl Sub for Point {
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y)
    }
}

/// 2D size with f32 dimensions
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

impl Size {
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }

    pub const ZERO: Self = Self::new(0.0, 0.0);
}

/// Rectangle with position and size
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn from_point_size(point: Point, size: Size) -> Self {
        Self::new(point.x, point.y, size.width, size.height)
    }

    pub fn origin(&self) -> Point {
        Point::new(self.x, self.y)
    }

    pub fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }

    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    pub fn contains(&self, point: Point) -> bool {
        point.x >= self.x && point.x < self.right() && point.y >= self.y && point.y < self.bottom()
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.right()
            && self.right() > other.x
            && self.y < other.bottom()
            && self.bottom() > other.y
    }

    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0, 0.0);
}

/// Animated cursor position override for smooth cursor motion.
///
/// When cursor animation is enabled, the render thread interpolates the cursor
/// position and passes this struct to the renderer instead of using the raw
/// frame glyph coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimatedCursor {
    pub window_id: i32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// When Some, draw cursor as a quad from these 4 corner positions (spring trail).
    /// Order: [top-left, top-right, bottom-right, bottom-left].
    pub corners: Option<[(f32, f32); 4]>,
    /// Which frame owns this cursor (0 = root frame, non-zero = child frame_id)
    pub frame_id: u64,
}

/// Cursor animation style.
///
/// Controls how the smooth cursor interpolates between positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CursorAnimStyle {
    /// Exponential decay (current default). No fixed duration; `speed` controls rate.
    Exponential = 0,
    /// Critically-damped spring (Neovide-style). Physics-based, natural feel.
    CriticallyDampedSpring = 1,
    /// Ease-out quadratic: gentle deceleration.
    EaseOutQuad = 2,
    /// Ease-out cubic: stronger deceleration.
    EaseOutCubic = 3,
    /// Ease-out exponential: sharp initial speed, rapid deceleration.
    EaseOutExpo = 4,
    /// Ease-in-out cubic: smooth S-curve acceleration + deceleration.
    EaseInOutCubic = 5,
    /// Linear: constant speed.
    Linear = 6,
}

impl CursorAnimStyle {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::CriticallyDampedSpring,
            2 => Self::EaseOutQuad,
            3 => Self::EaseOutCubic,
            4 => Self::EaseOutExpo,
            5 => Self::EaseInOutCubic,
            6 => Self::Linear,
            _ => Self::Exponential,
        }
    }
}

// ---------------------------------------------------------------------------
// Easing functions (t in 0.0..=1.0, returns 0.0..=1.0)
// ---------------------------------------------------------------------------

/// Ease-out quadratic: `−t(t−2)` — gentle deceleration.
pub fn ease_out_quad(t: f32) -> f32 {
    -t * (t - 2.0)
}

/// Ease-out cubic: `(t−1)³ + 1` — stronger deceleration.
pub fn ease_out_cubic(t: f32) -> f32 {
    let n = t - 1.0;
    n * n * n + 1.0
}

/// Ease-out exponential: `1 − 2^(−10t)` — sharp deceleration.
pub fn ease_out_expo(t: f32) -> f32 {
    if t >= 1.0 {
        1.0
    } else {
        1.0 - 2f32.powf(-10.0 * t)
    }
}

/// Ease-in-out cubic: smooth S-curve.
pub fn ease_in_out_cubic(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let n = -2.0 * t + 2.0;
        1.0 - n * n * n / 2.0
    }
}

/// Linear easing (identity).
pub fn ease_linear(t: f32) -> f32 {
    t
}

/// 2D transform matrix
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    /// 2D affine transform: [a, b, c, d, tx, ty]
    /// | a  b  0 |
    /// | c  d  0 |
    /// | tx ty 1 |
    pub matrix: [f32; 6],
}

impl Transform {
    pub const IDENTITY: Self = Self {
        matrix: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
    };

    pub fn translate(tx: f32, ty: f32) -> Self {
        Self {
            matrix: [1.0, 0.0, 0.0, 1.0, tx, ty],
        }
    }

    pub fn scale(sx: f32, sy: f32) -> Self {
        Self {
            matrix: [sx, 0.0, 0.0, sy, 0.0, 0.0],
        }
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
