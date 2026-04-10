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
mod tests {
    use super::*;

    #[test]
    fn test_color_from_pixel() {
        // from_pixel converts sRGB to linear
        let color = Color::from_pixel(0x00FF8040);
        // sRGB 1.0 → linear 1.0
        assert!((color.r - 1.0).abs() < 0.01);
        // sRGB 0.502 → linear ~0.214
        assert!((color.g - 0.214).abs() < 0.02);
        // sRGB 0.251 → linear ~0.051
        assert!((color.b - 0.051).abs() < 0.02);
        assert!((color.a - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_rect_contains() {
        let rect = Rect::new(10.0, 10.0, 100.0, 50.0);
        assert!(rect.contains(Point::new(50.0, 30.0)));
        assert!(!rect.contains(Point::new(5.0, 30.0)));
    }

    // ---------------------------------------------------------------
    // Color tests
    // ---------------------------------------------------------------

    #[test]
    fn test_color_new_and_components() {
        let c = Color::new(0.1, 0.2, 0.3, 0.4);
        assert_eq!(c.r, 0.1);
        assert_eq!(c.g, 0.2);
        assert_eq!(c.b, 0.3);
        assert_eq!(c.a, 0.4);
    }

    #[test]
    fn test_color_rgb_sets_alpha_to_one() {
        let c = Color::rgb(0.5, 0.6, 0.7);
        assert_eq!(c.a, 1.0);
        assert_eq!(c.r, 0.5);
        assert_eq!(c.g, 0.6);
        assert_eq!(c.b, 0.7);
    }

    #[test]
    fn test_color_from_u8() {
        let c = Color::from_u8(0, 128, 255, 255);
        assert!((c.r - 0.0).abs() < 1e-5);
        assert!((c.g - 128.0 / 255.0).abs() < 1e-5);
        assert!((c.b - 1.0).abs() < 1e-5);
        assert!((c.a - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_color_from_u8_zero_alpha() {
        let c = Color::from_u8(255, 255, 255, 0);
        assert!((c.a - 0.0).abs() < 1e-5);
    }

    #[test]
    fn test_color_named_constants() {
        assert_eq!(Color::BLACK, Color::rgb(0.0, 0.0, 0.0));
        assert_eq!(Color::WHITE, Color::rgb(1.0, 1.0, 1.0));
        assert_eq!(Color::RED, Color::rgb(1.0, 0.0, 0.0));
        assert_eq!(Color::GREEN, Color::rgb(0.0, 1.0, 0.0));
        assert_eq!(Color::BLUE, Color::rgb(0.0, 0.0, 1.0));
        assert_eq!(Color::TRANSPARENT, Color::new(0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn test_color_default_is_black() {
        assert_eq!(Color::default(), Color::BLACK);
    }

    #[test]
    fn test_srgb_to_linear_black_and_white() {
        // sRGB 0.0 -> linear 0.0
        let black_lin = Color::rgb(0.0, 0.0, 0.0).srgb_to_linear();
        assert!((black_lin.r).abs() < 1e-6);
        assert!((black_lin.g).abs() < 1e-6);
        assert!((black_lin.b).abs() < 1e-6);

        // sRGB 1.0 -> linear 1.0
        let white_lin = Color::rgb(1.0, 1.0, 1.0).srgb_to_linear();
        assert!((white_lin.r - 1.0).abs() < 1e-6);
        assert!((white_lin.g - 1.0).abs() < 1e-6);
        assert!((white_lin.b - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_srgb_to_linear_mid_gray() {
        // sRGB 0.5 -> linear ~0.214
        let gray = Color::rgb(0.5, 0.5, 0.5).srgb_to_linear();
        assert!((gray.r - 0.214).abs() < 0.01);
        assert!((gray.g - 0.214).abs() < 0.01);
        assert!((gray.b - 0.214).abs() < 0.01);
    }

    #[test]
    fn test_srgb_to_linear_preserves_alpha() {
        let c = Color::new(0.5, 0.5, 0.5, 0.75).srgb_to_linear();
        assert_eq!(c.a, 0.75);
    }

    #[test]
    fn test_srgb_to_linear_low_value_branch() {
        // Values <= 0.04045 use the linear branch (c / 12.92)
        let c = Color::rgb(0.04, 0.04, 0.04).srgb_to_linear();
        let expected = 0.04 / 12.92;
        assert!((c.r - expected).abs() < 1e-6);
    }

    #[test]
    fn test_from_pixel_opaque_red() {
        // 0x00FF0000 = opaque red (alpha 0 treated as 255)
        let c = Color::from_pixel(0x00FF0000);
        // red=1.0 sRGB -> 1.0 linear
        assert!((c.r - 1.0).abs() < 0.01);
        assert!((c.g).abs() < 0.01);
        assert!((c.b).abs() < 0.01);
        assert!((c.a - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_from_pixel_with_alpha() {
        // 0x80804020 = alpha=0x80, r=0x80, g=0x40, b=0x20
        let c = Color::from_pixel(0x80804020);
        // alpha 0x80 = 128/255 ~ 0.502
        assert!((c.a - 128.0 / 255.0).abs() < 0.01);
        // r: sRGB 128/255 ~ 0.502, linear ~ 0.214
        assert!((c.r - 0.214).abs() < 0.02);
    }

    #[test]
    fn test_from_pixel_pure_white() {
        let c = Color::from_pixel(0x00FFFFFF);
        assert!((c.r - 1.0).abs() < 0.01);
        assert!((c.g - 1.0).abs() < 0.01);
        assert!((c.b - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_from_pixel_pure_black() {
        let c = Color::from_pixel(0x00000000);
        // alpha 0 -> treated as 255, so a=1.0
        assert!((c.a - 1.0).abs() < 0.01);
        assert!((c.r).abs() < 0.01);
        assert!((c.g).abs() < 0.01);
        assert!((c.b).abs() < 0.01);
    }

    // ---------------------------------------------------------------
    // Point tests
    // ---------------------------------------------------------------

    #[test]
    fn test_point_new_and_zero() {
        let p = Point::new(3.0, 4.0);
        assert_eq!(p.x, 3.0);
        assert_eq!(p.y, 4.0);
        assert_eq!(Point::ZERO, Point::new(0.0, 0.0));
    }

    #[test]
    fn test_point_add() {
        let a = Point::new(1.0, 2.0);
        let b = Point::new(3.0, 4.0);
        let sum = a + b;
        assert_eq!(sum, Point::new(4.0, 6.0));
    }

    #[test]
    fn test_point_sub() {
        let a = Point::new(5.0, 7.0);
        let b = Point::new(2.0, 3.0);
        let diff = a - b;
        assert_eq!(diff, Point::new(3.0, 4.0));
    }

    #[test]
    fn test_point_add_sub_inverse() {
        let a = Point::new(10.0, 20.0);
        let b = Point::new(3.0, 7.0);
        let result = (a + b) - b;
        assert!((result.x - a.x).abs() < 1e-6);
        assert!((result.y - a.y).abs() < 1e-6);
    }

    #[test]
    fn test_point_default_is_zero() {
        assert_eq!(Point::default(), Point::ZERO);
    }

    // ---------------------------------------------------------------
    // Size tests
    // ---------------------------------------------------------------

    #[test]
    fn test_size_new_and_zero() {
        let s = Size::new(100.0, 200.0);
        assert_eq!(s.width, 100.0);
        assert_eq!(s.height, 200.0);
        assert_eq!(Size::ZERO, Size::new(0.0, 0.0));
    }

    #[test]
    fn test_size_default_is_zero() {
        assert_eq!(Size::default(), Size::ZERO);
    }

    // ---------------------------------------------------------------
    // Rect tests
    // ---------------------------------------------------------------

    #[test]
    fn test_rect_new_and_accessors() {
        let r = Rect::new(10.0, 20.0, 100.0, 50.0);
        assert_eq!(r.x, 10.0);
        assert_eq!(r.y, 20.0);
        assert_eq!(r.width, 100.0);
        assert_eq!(r.height, 50.0);
        assert_eq!(r.right(), 110.0);
        assert_eq!(r.bottom(), 70.0);
    }

    #[test]
    fn test_rect_from_point_size() {
        let p = Point::new(5.0, 10.0);
        let s = Size::new(50.0, 25.0);
        let r = Rect::from_point_size(p, s);
        assert_eq!(r.x, 5.0);
        assert_eq!(r.y, 10.0);
        assert_eq!(r.width, 50.0);
        assert_eq!(r.height, 25.0);
    }

    #[test]
    fn test_rect_origin_and_size() {
        let r = Rect::new(3.0, 4.0, 30.0, 40.0);
        assert_eq!(r.origin(), Point::new(3.0, 4.0));
        assert_eq!(r.size(), Size::new(30.0, 40.0));
    }

    #[test]
    fn test_rect_contains_edge_cases() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        // Top-left corner (inclusive)
        assert!(r.contains(Point::new(0.0, 0.0)));
        // Just inside bottom-right
        assert!(r.contains(Point::new(9.99, 9.99)));
        // Exactly on right edge (exclusive)
        assert!(!r.contains(Point::new(10.0, 5.0)));
        // Exactly on bottom edge (exclusive)
        assert!(!r.contains(Point::new(5.0, 10.0)));
        // Bottom-right corner (exclusive)
        assert!(!r.contains(Point::new(10.0, 10.0)));
    }

    #[test]
    fn test_rect_contains_negative_coords() {
        let r = Rect::new(-10.0, -10.0, 20.0, 20.0);
        assert!(r.contains(Point::new(0.0, 0.0)));
        assert!(r.contains(Point::new(-10.0, -10.0)));
        assert!(r.contains(Point::new(9.0, 9.0)));
        assert!(!r.contains(Point::new(10.0, 10.0)));
    }

    #[test]
    fn test_rect_intersects_overlapping() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 5.0, 10.0, 10.0);
        assert!(a.intersects(&b));
        assert!(b.intersects(&a));
    }

    #[test]
    fn test_rect_intersects_non_overlapping() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(20.0, 20.0, 10.0, 10.0);
        assert!(!a.intersects(&b));
        assert!(!b.intersects(&a));
    }

    #[test]
    fn test_rect_intersects_touching_edges() {
        // Touching at the edge means no overlap (right of a == left of b)
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(10.0, 0.0, 10.0, 10.0);
        assert!(!a.intersects(&b));
    }

    #[test]
    fn test_rect_intersects_one_inside_other() {
        let outer = Rect::new(0.0, 0.0, 100.0, 100.0);
        let inner = Rect::new(10.0, 10.0, 5.0, 5.0);
        assert!(outer.intersects(&inner));
        assert!(inner.intersects(&outer));
    }

    #[test]
    fn test_rect_zero_size_contains_nothing() {
        let r = Rect::new(5.0, 5.0, 0.0, 0.0);
        // Zero-size rect contains nothing (right() == x, bottom() == y, so < check fails)
        assert!(!r.contains(Point::new(5.0, 5.0)));
        assert!(!r.contains(Point::new(4.0, 4.0)));
    }

    #[test]
    fn test_rect_zero_size_intersects_behavior() {
        // A zero-width rect at origin inside a larger rect:
        // intersects uses strict inequality, so zero-width rect
        // at (5,5) with w=0,h=0 still satisfies all four conditions
        // against (0,0,100,100) because right()=5>0 and 5<100.
        let zero = Rect::new(5.0, 5.0, 0.0, 0.0);
        let large = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert!(zero.intersects(&large));

        // But two zero-size rects at the same point don't intersect
        // because right() == x means self.x < other.right() is false.
        let z2 = Rect::new(5.0, 5.0, 0.0, 0.0);
        assert!(!zero.intersects(&z2));

        // Zero-size rect outside the other rect doesn't intersect.
        let outside = Rect::new(200.0, 200.0, 0.0, 0.0);
        assert!(!outside.intersects(&large));
    }

    #[test]
    fn test_rect_zero_constant() {
        let r = Rect::ZERO;
        assert_eq!(r.x, 0.0);
        assert_eq!(r.y, 0.0);
        assert_eq!(r.width, 0.0);
        assert_eq!(r.height, 0.0);
    }

    #[test]
    fn test_rect_default_is_zero() {
        assert_eq!(Rect::default(), Rect::ZERO);
    }

    // ---------------------------------------------------------------
    // Transform tests
    // ---------------------------------------------------------------

    #[test]
    fn test_transform_identity() {
        let t = Transform::IDENTITY;
        assert_eq!(t.matrix, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn test_transform_default_is_identity() {
        assert_eq!(Transform::default(), Transform::IDENTITY);
    }

    #[test]
    fn test_transform_translate() {
        let t = Transform::translate(10.0, 20.0);
        assert_eq!(t.matrix[0], 1.0); // a (scale x)
        assert_eq!(t.matrix[3], 1.0); // d (scale y)
        assert_eq!(t.matrix[4], 10.0); // tx
        assert_eq!(t.matrix[5], 20.0); // ty
    }

    #[test]
    fn test_transform_scale() {
        let t = Transform::scale(2.0, 3.0);
        assert_eq!(t.matrix[0], 2.0); // a (scale x)
        assert_eq!(t.matrix[3], 3.0); // d (scale y)
        assert_eq!(t.matrix[4], 0.0); // tx
        assert_eq!(t.matrix[5], 0.0); // ty
    }

    #[test]
    fn test_transform_scale_identity() {
        let t = Transform::scale(1.0, 1.0);
        assert_eq!(t, Transform::IDENTITY);
    }

    #[test]
    fn test_transform_translate_zero() {
        let t = Transform::translate(0.0, 0.0);
        assert_eq!(t, Transform::IDENTITY);
    }

    // ---------------------------------------------------------------
    // CursorAnimStyle tests
    // ---------------------------------------------------------------

    #[test]
    fn test_cursor_anim_style_from_u8() {
        assert_eq!(CursorAnimStyle::from_u8(0), CursorAnimStyle::Exponential);
        assert_eq!(
            CursorAnimStyle::from_u8(1),
            CursorAnimStyle::CriticallyDampedSpring
        );
        assert_eq!(CursorAnimStyle::from_u8(2), CursorAnimStyle::EaseOutQuad);
        assert_eq!(CursorAnimStyle::from_u8(3), CursorAnimStyle::EaseOutCubic);
        assert_eq!(CursorAnimStyle::from_u8(4), CursorAnimStyle::EaseOutExpo);
        assert_eq!(CursorAnimStyle::from_u8(5), CursorAnimStyle::EaseInOutCubic);
        assert_eq!(CursorAnimStyle::from_u8(6), CursorAnimStyle::Linear);
    }

    #[test]
    fn test_cursor_anim_style_unknown_defaults_to_exponential() {
        assert_eq!(CursorAnimStyle::from_u8(7), CursorAnimStyle::Exponential);
        assert_eq!(CursorAnimStyle::from_u8(255), CursorAnimStyle::Exponential);
    }

    // ---------------------------------------------------------------
    // Easing function tests
    // ---------------------------------------------------------------

    #[test]
    fn test_ease_out_quad_endpoints() {
        assert!((ease_out_quad(0.0)).abs() < 1e-6);
        assert!((ease_out_quad(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_ease_out_quad_monotonic() {
        let mut prev = ease_out_quad(0.0);
        for i in 1..=100 {
            let t = i as f32 / 100.0;
            let val = ease_out_quad(t);
            assert!(val >= prev, "ease_out_quad not monotonic at t={}", t);
            prev = val;
        }
    }

    #[test]
    fn test_ease_out_cubic_endpoints() {
        assert!((ease_out_cubic(0.0)).abs() < 1e-6);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_ease_out_cubic_monotonic() {
        let mut prev = ease_out_cubic(0.0);
        for i in 1..=100 {
            let t = i as f32 / 100.0;
            let val = ease_out_cubic(t);
            assert!(val >= prev, "ease_out_cubic not monotonic at t={}", t);
            prev = val;
        }
    }

    #[test]
    fn test_ease_out_expo_endpoints() {
        assert!((ease_out_expo(0.0)).abs() < 1e-6);
        assert!((ease_out_expo(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_ease_out_expo_monotonic() {
        let mut prev = ease_out_expo(0.0);
        for i in 1..=100 {
            let t = i as f32 / 100.0;
            let val = ease_out_expo(t);
            assert!(val >= prev, "ease_out_expo not monotonic at t={}", t);
            prev = val;
        }
    }

    #[test]
    fn test_ease_in_out_cubic_endpoints() {
        assert!((ease_in_out_cubic(0.0)).abs() < 1e-6);
        assert!((ease_in_out_cubic(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_ease_in_out_cubic_midpoint() {
        // At t=0.5, should be 0.5 (symmetric S-curve)
        assert!((ease_in_out_cubic(0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_ease_in_out_cubic_monotonic() {
        let mut prev = ease_in_out_cubic(0.0);
        for i in 1..=100 {
            let t = i as f32 / 100.0;
            let val = ease_in_out_cubic(t);
            assert!(val >= prev, "ease_in_out_cubic not monotonic at t={}", t);
            prev = val;
        }
    }

    #[test]
    fn test_ease_linear_is_identity() {
        for i in 0..=100 {
            let t = i as f32 / 100.0;
            assert!((ease_linear(t) - t).abs() < 1e-6);
        }
    }

    // ---------------------------------------------------------------
    // Additional Color edge-case tests
    // ---------------------------------------------------------------

    #[test]
    fn test_color_from_u8_all_zeros() {
        let c = Color::from_u8(0, 0, 0, 0);
        assert_eq!(c.r, 0.0);
        assert_eq!(c.g, 0.0);
        assert_eq!(c.b, 0.0);
        assert_eq!(c.a, 0.0);
    }

    #[test]
    fn test_color_from_u8_all_max() {
        let c = Color::from_u8(255, 255, 255, 255);
        assert!((c.r - 1.0).abs() < 1e-6);
        assert!((c.g - 1.0).abs() < 1e-6);
        assert!((c.b - 1.0).abs() < 1e-6);
        assert!((c.a - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_color_from_u8_individual_channels() {
        // Only red
        let r = Color::from_u8(200, 0, 0, 255);
        assert!(r.r > 0.0);
        assert_eq!(r.g, 0.0);
        assert_eq!(r.b, 0.0);

        // Only green
        let g = Color::from_u8(0, 200, 0, 255);
        assert_eq!(g.r, 0.0);
        assert!(g.g > 0.0);
        assert_eq!(g.b, 0.0);

        // Only blue
        let b = Color::from_u8(0, 0, 200, 255);
        assert_eq!(b.r, 0.0);
        assert_eq!(b.g, 0.0);
        assert!(b.b > 0.0);
    }

    #[test]
    fn test_srgb_to_linear_at_threshold_boundary() {
        // Exactly at the threshold (0.04045) should use the linear branch
        let at_threshold = Color::srgb_component_to_linear(0.04045);
        let expected = 0.04045 / 12.92;
        assert!(
            (at_threshold - expected).abs() < 1e-6,
            "at threshold: got {}, expected {}",
            at_threshold,
            expected
        );
    }

    #[test]
    fn test_srgb_to_linear_just_above_threshold() {
        // Just above 0.04045 should use the power-curve branch
        let above = Color::srgb_component_to_linear(0.04046);
        let expected = ((0.04046 + 0.055) / 1.055_f32).powf(2.4);
        assert!(
            (above - expected).abs() < 1e-6,
            "just above threshold: got {}, expected {}",
            above,
            expected
        );
    }

    #[test]
    fn test_srgb_to_linear_known_reference_values() {
        // Known reference: sRGB 0.2 -> linear ~0.0331
        let val_02 = Color::srgb_component_to_linear(0.2);
        assert!(
            (val_02 - 0.0331).abs() < 0.001,
            "sRGB 0.2 -> linear: got {}, expected ~0.0331",
            val_02
        );

        // Known reference: sRGB 0.8 -> linear ~0.604
        let val_08 = Color::srgb_component_to_linear(0.8);
        assert!(
            (val_08 - 0.604).abs() < 0.001,
            "sRGB 0.8 -> linear: got {}, expected ~0.604",
            val_08
        );

        // Known reference: sRGB 0.5 -> linear ~0.2140
        let val_05 = Color::srgb_component_to_linear(0.5);
        assert!(
            (val_05 - 0.2140).abs() < 0.001,
            "sRGB 0.5 -> linear: got {}, expected ~0.2140",
            val_05
        );
    }

    #[test]
    fn test_srgb_to_linear_monotonic() {
        // sRGB to linear should be monotonically increasing
        let mut prev = Color::srgb_component_to_linear(0.0);
        for i in 1..=1000 {
            let srgb = i as f32 / 1000.0;
            let lin = Color::srgb_component_to_linear(srgb);
            assert!(
                lin >= prev,
                "sRGB to linear not monotonic at sRGB={}: prev={}, current={}",
                srgb,
                prev,
                lin
            );
            prev = lin;
        }
    }

    #[test]
    fn test_srgb_to_linear_output_range() {
        // All sRGB values [0,1] should map to [0,1] in linear
        for i in 0..=1000 {
            let srgb = i as f32 / 1000.0;
            let lin = Color::srgb_component_to_linear(srgb);
            assert!(
                lin >= 0.0 && lin <= 1.0,
                "sRGB {} mapped to linear {} which is outside [0,1]",
                srgb,
                lin
            );
        }
    }

    #[test]
    fn test_from_pixel_channel_extraction() {
        // Verify each channel is correctly extracted from the packed u32
        // 0xAARRGGBB format

        // Pure channels with explicit alpha
        let red = Color::from_pixel(0xFFFF0000);
        assert!((red.r - 1.0).abs() < 0.01, "red channel from 0xFFFF0000");
        assert!(red.g.abs() < 0.01, "green should be 0 in 0xFFFF0000");
        assert!(red.b.abs() < 0.01, "blue should be 0 in 0xFFFF0000");
        assert!((red.a - 1.0).abs() < 0.01, "alpha should be 1.0 for 0xFF");

        let green = Color::from_pixel(0xFF00FF00);
        assert!(green.r.abs() < 0.01, "red should be 0 in 0xFF00FF00");
        assert!(
            (green.g - 1.0).abs() < 0.01,
            "green channel from 0xFF00FF00"
        );
        assert!(green.b.abs() < 0.01, "blue should be 0 in 0xFF00FF00");

        let blue = Color::from_pixel(0xFF0000FF);
        assert!(blue.r.abs() < 0.01, "red should be 0 in 0xFF0000FF");
        assert!(blue.g.abs() < 0.01, "green should be 0 in 0xFF0000FF");
        assert!((blue.b - 1.0).abs() < 0.01, "blue channel from 0xFF0000FF");
    }

    #[test]
    fn test_from_pixel_alpha_zero_treated_as_opaque() {
        // When alpha byte is 0x00, from_pixel treats it as fully opaque (255)
        let c = Color::from_pixel(0x00804020);
        assert!((c.a - 1.0).abs() < 0.01, "alpha 0x00 should become 1.0");
    }

    #[test]
    fn test_from_pixel_alpha_nonzero_preserved() {
        // When alpha is non-zero, it should be preserved as-is
        let c = Color::from_pixel(0x40804020);
        let expected_a = 0x40 as f32 / 255.0;
        assert!(
            (c.a - expected_a).abs() < 0.01,
            "alpha 0x40: got {}, expected {}",
            c.a,
            expected_a
        );
    }

    #[test]
    fn test_from_pixel_full_alpha_ff() {
        let c = Color::from_pixel(0xFF808080);
        assert!((c.a - 1.0).abs() < 0.01, "alpha 0xFF should be 1.0");
    }

    #[test]
    fn test_color_clone_and_copy() {
        let original = Color::new(0.1, 0.2, 0.3, 0.4);
        let cloned = original.clone();
        let copied = original; // Copy trait
        assert_eq!(original, cloned);
        assert_eq!(original, copied);
    }

    #[test]
    fn test_color_partial_eq() {
        let a = Color::new(0.1, 0.2, 0.3, 0.4);
        let b = Color::new(0.1, 0.2, 0.3, 0.4);
        let c = Color::new(0.1, 0.2, 0.3, 0.5);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn test_color_debug_format() {
        let c = Color::new(1.0, 0.0, 0.5, 1.0);
        let debug_str = format!("{:?}", c);
        assert!(debug_str.contains("Color"));
        assert!(debug_str.contains("1.0"));
        assert!(debug_str.contains("0.5"));
    }

    #[test]
    fn test_srgb_to_linear_idempotent_at_extremes() {
        // 0.0 -> linear 0.0, and applying again still 0.0
        let zero = Color::rgb(0.0, 0.0, 0.0).srgb_to_linear().srgb_to_linear();
        assert!(zero.r.abs() < 1e-6);

        // 1.0 -> linear 1.0, and applying again still 1.0
        let one = Color::rgb(1.0, 1.0, 1.0).srgb_to_linear().srgb_to_linear();
        assert!((one.r - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_srgb_to_linear_reduces_mid_values() {
        // For sRGB values in (0, 1) exclusive, linear should always be less
        // than or equal to sRGB (the sRGB curve is above the linear diagonal)
        for i in 1..1000 {
            let srgb = i as f32 / 1000.0;
            let lin = Color::srgb_component_to_linear(srgb);
            assert!(
                lin <= srgb + 1e-6,
                "linear {} should be <= sRGB {} for mid values",
                lin,
                srgb
            );
        }
    }

    #[test]
    fn test_color_from_u8_precise_values() {
        // Verify precise conversion for specific u8 values
        let c = Color::from_u8(1, 127, 254, 128);
        assert!((c.r - 1.0 / 255.0).abs() < 1e-6);
        assert!((c.g - 127.0 / 255.0).abs() < 1e-6);
        assert!((c.b - 254.0 / 255.0).abs() < 1e-6);
        assert!((c.a - 128.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn test_color_rgb_vs_new_with_alpha_one() {
        // rgb(r, g, b) should be identical to new(r, g, b, 1.0)
        let rgb = Color::rgb(0.3, 0.6, 0.9);
        let new = Color::new(0.3, 0.6, 0.9, 1.0);
        assert_eq!(rgb, new);
    }

    #[test]
    fn test_color_transparent_components() {
        let t = Color::TRANSPARENT;
        assert_eq!(t.r, 0.0);
        assert_eq!(t.g, 0.0);
        assert_eq!(t.b, 0.0);
        assert_eq!(t.a, 0.0);
    }

    #[test]
    fn test_from_pixel_emacs_typical_colors() {
        // Emacs default foreground: #FFFFFF (white)
        let white = Color::from_pixel(0x00FFFFFF);
        assert!((white.r - 1.0).abs() < 0.01);
        assert!((white.g - 1.0).abs() < 0.01);
        assert!((white.b - 1.0).abs() < 0.01);

        // Emacs default background: #000000 (black)
        let black = Color::from_pixel(0x00000000);
        assert!(black.r.abs() < 0.01);
        assert!(black.g.abs() < 0.01);
        assert!(black.b.abs() < 0.01);

        // A typical Emacs comment color: #7F7F7F (gray)
        let gray = Color::from_pixel(0x007F7F7F);
        // sRGB 127/255 ~ 0.498, linear ~ 0.212
        assert!((gray.r - 0.212).abs() < 0.02);
        assert_eq!(gray.r, gray.g);
        assert_eq!(gray.g, gray.b);
    }

    #[test]
    fn test_srgb_component_to_linear_continuity_at_threshold() {
        // The two branches should produce very close values at the threshold
        // to ensure continuity of the conversion function
        let below = Color::srgb_component_to_linear(0.04045);
        let above = Color::srgb_component_to_linear(0.04046);
        assert!(
            (above - below).abs() < 0.001,
            "discontinuity at threshold: below={}, above={}",
            below,
            above
        );
    }
}
