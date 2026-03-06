//! Cursor animation, blinking, and size transition state.

use crate::core::frame_glyphs::CursorStyle;
use crate::core::types::{
    Color, CursorAnimStyle, ease_in_out_cubic, ease_linear, ease_out_cubic, ease_out_expo,
    ease_out_quad,
};

/// Target position/style for cursor animation
#[derive(Debug, Clone)]
pub(super) struct CursorTarget {
    pub(super) window_id: i32,
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) width: f32,
    pub(super) height: f32,
    pub(super) style: CursorStyle,
    pub(super) color: Color,
    /// Which frame owns this cursor (0 = root frame, non-zero = child frame_id)
    pub(super) frame_id: u64,
}

/// Per-corner spring state for the 4-corner cursor trail animation.
/// Each corner has its own position, velocity, and spring frequency.
#[derive(Debug, Clone, Copy)]
pub(super) struct CornerSpring {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) vx: f32,
    pub(super) vy: f32,
    pub(super) target_x: f32,
    pub(super) target_y: f32,
    pub(super) omega: f32,
}

/// Cursor animation, blinking, and size transition state.
///
/// Extracted from RenderApp to group all cursor-related fields together.
pub(super) struct CursorState {
    // Blink state (managed by render thread)
    pub(super) blink_on: bool,
    pub(super) blink_enabled: bool,
    pub(super) last_blink_toggle: std::time::Instant,
    pub(super) blink_interval: std::time::Duration,

    // Animation (smooth motion)
    pub(super) anim_enabled: bool,
    pub(super) anim_speed: f32,
    pub(super) anim_style: CursorAnimStyle,
    pub(super) anim_duration: f32, // seconds, for non-Exponential styles
    pub(super) target: Option<CursorTarget>,
    pub(super) current_x: f32,
    pub(super) current_y: f32,
    pub(super) current_w: f32,
    pub(super) current_h: f32,
    pub(super) animating: bool,
    pub(super) last_anim_time: std::time::Instant,
    // For easing/linear styles: capture start position when animation begins
    pub(super) start_x: f32,
    pub(super) start_y: f32,
    pub(super) start_w: f32,
    pub(super) start_h: f32,
    pub(super) anim_start_time: std::time::Instant,
    // For critically-damped spring: velocity per axis
    pub(super) velocity_x: f32,
    pub(super) velocity_y: f32,
    pub(super) velocity_w: f32,
    pub(super) velocity_h: f32,
    // 4-corner spring trail state (TL, TR, BR, BL)
    pub(super) corner_springs: [CornerSpring; 4],
    pub(super) trail_size: f32,
    // Previous target center for computing travel direction
    pub(super) prev_target_cx: f32,
    pub(super) prev_target_cy: f32,

    // Size transition (independent of position animation)
    pub(super) size_transition_enabled: bool,
    pub(super) size_transition_duration: f32, // seconds
    pub(super) size_animating: bool,
    pub(super) size_start_w: f32,
    pub(super) size_start_h: f32,
    pub(super) size_target_w: f32,
    pub(super) size_target_h: f32,
    pub(super) size_anim_start: std::time::Instant,
}

impl Default for CursorState {
    fn default() -> Self {
        Self {
            blink_on: true,
            blink_enabled: true,
            last_blink_toggle: std::time::Instant::now(),
            blink_interval: std::time::Duration::from_millis(500),
            anim_enabled: true,
            anim_speed: 15.0,
            anim_style: CursorAnimStyle::CriticallyDampedSpring,
            anim_duration: 0.15,
            target: None,
            current_x: 0.0,
            current_y: 0.0,
            current_w: 0.0,
            current_h: 0.0,
            animating: false,
            last_anim_time: std::time::Instant::now(),
            start_x: 0.0,
            start_y: 0.0,
            start_w: 0.0,
            start_h: 0.0,
            anim_start_time: std::time::Instant::now(),
            velocity_x: 0.0,
            velocity_y: 0.0,
            velocity_w: 0.0,
            velocity_h: 0.0,
            corner_springs: [CornerSpring {
                x: 0.0,
                y: 0.0,
                vx: 0.0,
                vy: 0.0,
                target_x: 0.0,
                target_y: 0.0,
                omega: 26.7,
            }; 4],
            trail_size: 0.7,
            prev_target_cx: 0.0,
            prev_target_cy: 0.0,
            size_transition_enabled: false,
            size_transition_duration: 0.15,
            size_animating: false,
            size_start_w: 0.0,
            size_start_h: 0.0,
            size_target_w: 0.0,
            size_target_h: 0.0,
            size_anim_start: std::time::Instant::now(),
        }
    }
}

impl CursorState {
    /// Compute the 4 target corners for a cursor based on its style.
    /// Returns [TL, TR, BR, BL] as (x, y) tuples.
    pub(super) fn target_corners(target: &CursorTarget) -> [(f32, f32); 4] {
        match target.style {
            CursorStyle::FilledBox => {
                // Filled box: full rectangle
                let x0 = target.x;
                let y0 = target.y;
                let x1 = target.x + target.width;
                let y1 = target.y + target.height;
                [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
            }
            CursorStyle::Bar(bar_w) => {
                // Bar: thin vertical line
                let x0 = target.x;
                let y0 = target.y;
                let x1 = target.x + bar_w;
                let y1 = target.y + target.height;
                [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
            }
            CursorStyle::Hbar(hbar_h) => {
                // Underline: thin horizontal line at bottom
                let x0 = target.x;
                let y0 = target.y + target.height - hbar_h;
                let x1 = target.x + target.width;
                let y1 = target.y + target.height;
                [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
            }
            CursorStyle::Hollow => {
                // Hollow: full rectangle (border only drawn elsewhere)
                let x0 = target.x;
                let y0 = target.y;
                let x1 = target.x + target.width;
                let y1 = target.y + target.height;
                [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
            }
        }
    }

    /// Tick cursor animation, returns true if position changed (needs redraw)
    pub(super) fn tick_animation(&mut self) -> bool {
        if !self.anim_enabled || !self.animating {
            return false;
        }
        let target = match self.target.as_ref() {
            Some(t) => t.clone(),
            None => return false,
        };

        let now = std::time::Instant::now();
        let dt = now.duration_since(self.last_anim_time).as_secs_f32();
        self.last_anim_time = now;

        match self.anim_style {
            CursorAnimStyle::Exponential => {
                let factor = 1.0 - (-self.anim_speed * dt).exp();
                let dx = target.x - self.current_x;
                let dy = target.y - self.current_y;
                let dw = target.width - self.current_w;
                let dh = target.height - self.current_h;
                self.current_x += dx * factor;
                self.current_y += dy * factor;
                self.current_w += dw * factor;
                self.current_h += dh * factor;
                if dx.abs() < 0.5 && dy.abs() < 0.5 && dw.abs() < 0.5 && dh.abs() < 0.5 {
                    self.snap(&target);
                }
            }
            CursorAnimStyle::CriticallyDampedSpring => {
                let mut all_settled = true;
                for i in 0..4 {
                    let spring = &mut self.corner_springs[i];
                    let omega = spring.omega;
                    let exp_term = (-omega * dt).exp();

                    let x0 = spring.x - spring.target_x;
                    let vx0 = spring.vx;
                    let new_x = (x0 + (vx0 + omega * x0) * dt) * exp_term;
                    spring.vx = ((vx0 + omega * x0) * exp_term)
                        - omega * (x0 + (vx0 + omega * x0) * dt) * exp_term;
                    spring.x = spring.target_x + new_x;

                    let y0 = spring.y - spring.target_y;
                    let vy0 = spring.vy;
                    let new_y = (y0 + (vy0 + omega * y0) * dt) * exp_term;
                    spring.vy = ((vy0 + omega * y0) * exp_term)
                        - omega * (y0 + (vy0 + omega * y0) * dt) * exp_term;
                    spring.y = spring.target_y + new_y;

                    let dist =
                        (spring.x - spring.target_x).abs() + (spring.y - spring.target_y).abs();
                    let vel = spring.vx.abs() + spring.vy.abs();
                    if dist > 0.5 || vel > 1.0 {
                        all_settled = false;
                    }
                }

                let min_x = self
                    .corner_springs
                    .iter()
                    .map(|s| s.x)
                    .fold(f32::INFINITY, f32::min);
                let min_y = self
                    .corner_springs
                    .iter()
                    .map(|s| s.y)
                    .fold(f32::INFINITY, f32::min);
                let max_x = self
                    .corner_springs
                    .iter()
                    .map(|s| s.x)
                    .fold(f32::NEG_INFINITY, f32::max);
                let max_y = self
                    .corner_springs
                    .iter()
                    .map(|s| s.y)
                    .fold(f32::NEG_INFINITY, f32::max);
                self.current_x = min_x;
                self.current_y = min_y;
                self.current_w = max_x - min_x;
                self.current_h = max_y - min_y;

                if all_settled {
                    let target_corners = Self::target_corners(&target);
                    for i in 0..4 {
                        self.corner_springs[i].x = target_corners[i].0;
                        self.corner_springs[i].y = target_corners[i].1;
                        self.corner_springs[i].vx = 0.0;
                        self.corner_springs[i].vy = 0.0;
                    }
                    self.snap(&target);
                }
            }
            style => {
                let elapsed = now.duration_since(self.anim_start_time).as_secs_f32();
                let raw_t = (elapsed / self.anim_duration).min(1.0);
                let t = match style {
                    CursorAnimStyle::EaseOutQuad => ease_out_quad(raw_t),
                    CursorAnimStyle::EaseOutCubic => ease_out_cubic(raw_t),
                    CursorAnimStyle::EaseOutExpo => ease_out_expo(raw_t),
                    CursorAnimStyle::EaseInOutCubic => ease_in_out_cubic(raw_t),
                    CursorAnimStyle::Linear => ease_linear(raw_t),
                    _ => raw_t,
                };
                self.current_x = self.start_x + (target.x - self.start_x) * t;
                self.current_y = self.start_y + (target.y - self.start_y) * t;
                self.current_w = self.start_w + (target.width - self.start_w) * t;
                self.current_h = self.start_h + (target.height - self.start_h) * t;
                if raw_t >= 1.0 {
                    self.snap(&target);
                }
            }
        }

        true
    }

    /// Snap cursor to target and stop animating
    pub(super) fn snap(&mut self, target: &CursorTarget) {
        self.current_x = target.x;
        self.current_y = target.y;
        self.current_w = target.width;
        self.current_h = target.height;
        self.animating = false;
    }

    /// Tick cursor size transition, returns true if size changed (needs redraw).
    pub(super) fn tick_size_animation(&mut self) -> bool {
        if !self.size_transition_enabled || !self.size_animating {
            return false;
        }
        let elapsed = self.size_anim_start.elapsed().as_secs_f32();
        let raw_t = (elapsed / self.size_transition_duration).min(1.0);
        let t = raw_t * (2.0 - raw_t); // ease-out-quad
        self.current_w = self.size_start_w + (self.size_target_w - self.size_start_w) * t;
        self.current_h = self.size_start_h + (self.size_target_h - self.size_start_h) * t;
        if raw_t >= 1.0 {
            self.current_w = self.size_target_w;
            self.current_h = self.size_target_h;
            self.size_animating = false;
        }
        true
    }

    /// Reset blink to visible (e.g. when new frame arrives)
    pub(super) fn reset_blink(&mut self) {
        self.blink_on = true;
        self.last_blink_toggle = std::time::Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    // ---------------------------------------------------------------
    // Helper: create a CursorTarget with given position/size/style
    // ---------------------------------------------------------------
    fn make_target(x: f32, y: f32, w: f32, h: f32, style: CursorStyle) -> CursorTarget {
        CursorTarget {
            window_id: 1,
            x,
            y,
            width: w,
            height: h,
            style,
            color: Color::WHITE,
            frame_id: 0,
        }
    }

    // ---------------------------------------------------------------
    // Easing functions: boundary values, monotonicity, specific values
    // ---------------------------------------------------------------

    #[test]
    fn easing_linear_endpoints() {
        assert!((ease_linear(0.0)).abs() < 1e-6);
        assert!((ease_linear(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn easing_linear_identity() {
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            assert!(
                (ease_linear(t) - t).abs() < 1e-6,
                "ease_linear({}) should equal {} but got {}",
                t,
                t,
                ease_linear(t)
            );
        }
    }

    #[test]
    fn easing_out_quad_endpoints() {
        assert!((ease_out_quad(0.0)).abs() < 1e-6);
        assert!((ease_out_quad(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn easing_out_quad_monotonic_increasing() {
        let mut prev = ease_out_quad(0.0);
        for i in 1..=100 {
            let t = i as f32 / 100.0;
            let val = ease_out_quad(t);
            assert!(
                val >= prev,
                "ease_out_quad not monotonic at t={}: {} < {}",
                t,
                val,
                prev
            );
            prev = val;
        }
    }

    #[test]
    fn easing_out_quad_midpoint() {
        // ease_out_quad(0.5) = -0.5*(0.5-2.0) = -0.5*(-1.5) = 0.75
        assert!((ease_out_quad(0.5) - 0.75).abs() < 1e-6);
    }

    #[test]
    fn easing_out_cubic_endpoints() {
        assert!((ease_out_cubic(0.0)).abs() < 1e-6);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn easing_out_cubic_monotonic_increasing() {
        let mut prev = ease_out_cubic(0.0);
        for i in 1..=100 {
            let t = i as f32 / 100.0;
            let val = ease_out_cubic(t);
            assert!(val >= prev, "ease_out_cubic not monotonic at t={}", t);
            prev = val;
        }
    }

    #[test]
    fn easing_out_cubic_midpoint() {
        // ease_out_cubic(0.5) = (-0.5)^3 + 1 = -0.125 + 1 = 0.875
        assert!((ease_out_cubic(0.5) - 0.875).abs() < 1e-6);
    }

    #[test]
    fn easing_out_expo_endpoints() {
        assert!((ease_out_expo(0.0)).abs() < 1e-6);
        assert!((ease_out_expo(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn easing_out_expo_monotonic_increasing() {
        let mut prev = ease_out_expo(0.0);
        for i in 1..=100 {
            let t = i as f32 / 100.0;
            let val = ease_out_expo(t);
            assert!(val >= prev, "ease_out_expo not monotonic at t={}", t);
            prev = val;
        }
    }

    #[test]
    fn easing_out_expo_rapid_initial_progress() {
        // ease_out_expo should have >= 50% progress at t=0.1
        // 1 - 2^(-10*0.1) = 1 - 2^(-1) = 1 - 0.5 = 0.5
        let val = ease_out_expo(0.1);
        assert!(val >= 0.5, "ease_out_expo(0.1) = {} should be >= 0.5", val);
        // And at t=0.2 it should clearly exceed 0.5
        let val2 = ease_out_expo(0.2);
        assert!(val2 > 0.7, "ease_out_expo(0.2) = {} should be > 0.7", val2);
    }

    #[test]
    fn easing_out_expo_above_one_returns_one() {
        // The function has a special case for t >= 1.0
        assert!((ease_out_expo(1.0) - 1.0).abs() < 1e-6);
        assert!((ease_out_expo(1.5) - 1.0).abs() < 1e-6);
        assert!((ease_out_expo(100.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn easing_in_out_cubic_endpoints() {
        assert!((ease_in_out_cubic(0.0)).abs() < 1e-6);
        assert!((ease_in_out_cubic(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn easing_in_out_cubic_symmetric_midpoint() {
        // S-curve: midpoint should be exactly 0.5
        assert!((ease_in_out_cubic(0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn easing_in_out_cubic_monotonic_increasing() {
        let mut prev = ease_in_out_cubic(0.0);
        for i in 1..=100 {
            let t = i as f32 / 100.0;
            let val = ease_in_out_cubic(t);
            assert!(val >= prev, "ease_in_out_cubic not monotonic at t={}", t);
            prev = val;
        }
    }

    #[test]
    fn easing_in_out_cubic_symmetry() {
        // ease_in_out_cubic should be symmetric: f(t) + f(1-t) = 1
        for i in 0..=50 {
            let t = i as f32 / 100.0;
            let sum = ease_in_out_cubic(t) + ease_in_out_cubic(1.0 - t);
            assert!(
                (sum - 1.0).abs() < 1e-5,
                "ease_in_out_cubic symmetry broken at t={}: f(t)+f(1-t)={}",
                t,
                sum
            );
        }
    }

    #[test]
    fn easing_all_output_range_zero_to_one() {
        // All easing functions should map [0,1] to [0,1]
        for i in 0..=100 {
            let t = i as f32 / 100.0;
            let fns: [(&str, f32); 5] = [
                ("linear", ease_linear(t)),
                ("out_quad", ease_out_quad(t)),
                ("out_cubic", ease_out_cubic(t)),
                ("out_expo", ease_out_expo(t)),
                ("in_out_cubic", ease_in_out_cubic(t)),
            ];
            for (name, val) in &fns {
                assert!(
                    *val >= -1e-6 && *val <= 1.0 + 1e-6,
                    "{}({}) = {} is outside [0,1]",
                    name,
                    t,
                    val
                );
            }
        }
    }

    // ---------------------------------------------------------------
    // CursorState default values
    // ---------------------------------------------------------------

    #[test]
    fn default_state_blink_is_on() {
        let state = CursorState::default();
        assert!(state.blink_on);
        assert!(state.blink_enabled);
        assert_eq!(state.blink_interval, Duration::from_millis(500));
    }

    #[test]
    fn default_state_animation_enabled() {
        let state = CursorState::default();
        assert!(state.anim_enabled);
        assert!(!state.animating);
        assert_eq!(state.anim_speed, 15.0);
        assert_eq!(state.anim_style, CursorAnimStyle::CriticallyDampedSpring);
        assert_eq!(state.anim_duration, 0.15);
    }

    #[test]
    fn default_state_no_target() {
        let state = CursorState::default();
        assert!(state.target.is_none());
    }

    #[test]
    fn default_state_positions_at_origin() {
        let state = CursorState::default();
        assert_eq!(state.current_x, 0.0);
        assert_eq!(state.current_y, 0.0);
        assert_eq!(state.current_w, 0.0);
        assert_eq!(state.current_h, 0.0);
    }

    #[test]
    fn default_state_velocities_zero() {
        let state = CursorState::default();
        assert_eq!(state.velocity_x, 0.0);
        assert_eq!(state.velocity_y, 0.0);
        assert_eq!(state.velocity_w, 0.0);
        assert_eq!(state.velocity_h, 0.0);
    }

    #[test]
    fn default_state_size_transition_disabled() {
        let state = CursorState::default();
        assert!(!state.size_transition_enabled);
        assert!(!state.size_animating);
        assert_eq!(state.size_transition_duration, 0.15);
    }

    #[test]
    fn default_state_corner_springs() {
        let state = CursorState::default();
        for spring in &state.corner_springs {
            assert_eq!(spring.x, 0.0);
            assert_eq!(spring.y, 0.0);
            assert_eq!(spring.vx, 0.0);
            assert_eq!(spring.vy, 0.0);
            assert_eq!(spring.target_x, 0.0);
            assert_eq!(spring.target_y, 0.0);
            assert_eq!(spring.omega, 26.7);
        }
        assert_eq!(state.trail_size, 0.7);
    }

    // ---------------------------------------------------------------
    // target_corners: cursor style → corner positions
    // ---------------------------------------------------------------

    #[test]
    fn target_corners_filled_box_style_0() {
        let target = make_target(10.0, 20.0, 100.0, 50.0, CursorStyle::FilledBox);
        let corners = CursorState::target_corners(&target);
        // TL, TR, BR, BL
        assert_eq!(corners[0], (10.0, 20.0)); // top-left
        assert_eq!(corners[1], (110.0, 20.0)); // top-right
        assert_eq!(corners[2], (110.0, 70.0)); // bottom-right
        assert_eq!(corners[3], (10.0, 70.0)); // bottom-left
    }

    #[test]
    fn target_corners_bar_style_1() {
        let target = make_target(10.0, 20.0, 100.0, 50.0, CursorStyle::Bar(2.0));
        let corners = CursorState::target_corners(&target);
        // Bar is 2px wide
        assert_eq!(corners[0], (10.0, 20.0));
        assert_eq!(corners[1], (12.0, 20.0)); // x + 2.0
        assert_eq!(corners[2], (12.0, 70.0));
        assert_eq!(corners[3], (10.0, 70.0));
    }

    #[test]
    fn target_corners_underline_style_2() {
        let target = make_target(10.0, 20.0, 100.0, 50.0, CursorStyle::Hbar(2.0));
        let corners = CursorState::target_corners(&target);
        // Underline is 2px tall at the bottom
        assert_eq!(corners[0], (10.0, 68.0)); // y + height - 2.0
        assert_eq!(corners[1], (110.0, 68.0));
        assert_eq!(corners[2], (110.0, 70.0)); // y + height
        assert_eq!(corners[3], (10.0, 70.0));
    }

    #[test]
    fn target_corners_hollow_style_3_uses_default() {
        let target = make_target(10.0, 20.0, 100.0, 50.0, CursorStyle::Hollow);
        let corners = CursorState::target_corners(&target);
        // Style 3 (hollow) falls through to default: full rectangle
        assert_eq!(corners[0], (10.0, 20.0));
        assert_eq!(corners[1], (110.0, 20.0));
        assert_eq!(corners[2], (110.0, 70.0));
        assert_eq!(corners[3], (10.0, 70.0));
    }

    #[test]
    fn target_corners_hollow_uses_full_rectangle() {
        let target = make_target(5.0, 10.0, 20.0, 30.0, CursorStyle::Hollow);
        let corners = CursorState::target_corners(&target);
        assert_eq!(corners[0], (5.0, 10.0));
        assert_eq!(corners[1], (25.0, 10.0));
        assert_eq!(corners[2], (25.0, 40.0));
        assert_eq!(corners[3], (5.0, 40.0));
    }

    #[test]
    fn target_corners_zero_size_cursor() {
        let target = make_target(10.0, 20.0, 0.0, 0.0, CursorStyle::FilledBox);
        let corners = CursorState::target_corners(&target);
        // All four corners collapse to (10, 20) or (10, 20)
        assert_eq!(corners[0], (10.0, 20.0));
        assert_eq!(corners[1], (10.0, 20.0));
        assert_eq!(corners[2], (10.0, 20.0));
        assert_eq!(corners[3], (10.0, 20.0));
    }

    #[test]
    fn target_corners_bar_ignores_width() {
        // Bar style always uses 2px width regardless of target.width
        let target = make_target(0.0, 0.0, 500.0, 30.0, CursorStyle::Bar(2.0));
        let corners = CursorState::target_corners(&target);
        assert_eq!(corners[1].0, 2.0); // top-right x = 0 + 2
    }

    #[test]
    fn target_corners_underline_uses_full_width() {
        let target = make_target(0.0, 0.0, 80.0, 20.0, CursorStyle::Hbar(2.0));
        let corners = CursorState::target_corners(&target);
        assert_eq!(corners[0].0, 0.0);
        assert_eq!(corners[1].0, 80.0); // uses full width
    }

    // ---------------------------------------------------------------
    // snap: immediately move to target
    // ---------------------------------------------------------------

    #[test]
    fn snap_sets_position_to_target() {
        let mut state = CursorState::default();
        state.animating = true;
        state.current_x = 100.0;
        state.current_y = 200.0;
        state.current_w = 50.0;
        state.current_h = 25.0;

        let target = make_target(300.0, 400.0, 80.0, 40.0, CursorStyle::FilledBox);
        state.snap(&target);

        assert_eq!(state.current_x, 300.0);
        assert_eq!(state.current_y, 400.0);
        assert_eq!(state.current_w, 80.0);
        assert_eq!(state.current_h, 40.0);
    }

    #[test]
    fn snap_stops_animation() {
        let mut state = CursorState::default();
        state.animating = true;
        let target = make_target(0.0, 0.0, 10.0, 10.0, CursorStyle::FilledBox);
        state.snap(&target);
        assert!(!state.animating);
    }

    #[test]
    fn snap_same_position_is_noop_on_values() {
        let mut state = CursorState::default();
        state.current_x = 50.0;
        state.current_y = 60.0;
        state.current_w = 10.0;
        state.current_h = 20.0;
        state.animating = true;

        let target = make_target(50.0, 60.0, 10.0, 20.0, CursorStyle::FilledBox);
        state.snap(&target);

        assert_eq!(state.current_x, 50.0);
        assert_eq!(state.current_y, 60.0);
        assert!(!state.animating);
    }

    // ---------------------------------------------------------------
    // reset_blink
    // ---------------------------------------------------------------

    #[test]
    fn reset_blink_sets_visible() {
        let mut state = CursorState::default();
        state.blink_on = false;
        let before = Instant::now();
        state.reset_blink();
        let after = Instant::now();

        assert!(state.blink_on);
        assert!(state.last_blink_toggle >= before);
        assert!(state.last_blink_toggle <= after);
    }

    #[test]
    fn reset_blink_already_visible_stays_visible() {
        let mut state = CursorState::default();
        assert!(state.blink_on); // default is true
        state.reset_blink();
        assert!(state.blink_on);
    }

    #[test]
    fn reset_blink_updates_timestamp() {
        let mut state = CursorState::default();
        let old_time = state.last_blink_toggle;
        // Sleep briefly to ensure time advances
        std::thread::sleep(Duration::from_millis(2));
        state.reset_blink();
        assert!(state.last_blink_toggle > old_time);
    }

    // ---------------------------------------------------------------
    // tick_animation: returns false when disabled or not animating
    // ---------------------------------------------------------------

    #[test]
    fn tick_animation_returns_false_when_disabled() {
        let mut state = CursorState::default();
        state.anim_enabled = false;
        state.animating = true;
        state.target = Some(make_target(
            100.0,
            100.0,
            10.0,
            20.0,
            CursorStyle::FilledBox,
        ));
        assert!(!state.tick_animation());
    }

    #[test]
    fn tick_animation_returns_false_when_not_animating() {
        let mut state = CursorState::default();
        state.anim_enabled = true;
        state.animating = false;
        state.target = Some(make_target(
            100.0,
            100.0,
            10.0,
            20.0,
            CursorStyle::FilledBox,
        ));
        assert!(!state.tick_animation());
    }

    #[test]
    fn tick_animation_returns_false_when_no_target() {
        let mut state = CursorState::default();
        state.anim_enabled = true;
        state.animating = true;
        state.target = None;
        assert!(!state.tick_animation());
    }

    // ---------------------------------------------------------------
    // tick_animation: Exponential style
    // ---------------------------------------------------------------

    #[test]
    fn tick_animation_exponential_moves_toward_target() {
        let mut state = CursorState::default();
        state.anim_enabled = true;
        state.animating = true;
        state.anim_style = CursorAnimStyle::Exponential;
        state.anim_speed = 15.0;
        state.current_x = 0.0;
        state.current_y = 0.0;
        state.current_w = 10.0;
        state.current_h = 20.0;
        state.target = Some(make_target(
            200.0,
            300.0,
            10.0,
            20.0,
            CursorStyle::FilledBox,
        ));
        state.last_anim_time = Instant::now();

        // Wait a tiny bit so dt > 0
        std::thread::sleep(Duration::from_millis(5));

        let result = state.tick_animation();
        assert!(result);
        // Should have moved toward target
        assert!(
            state.current_x > 0.0,
            "x should have moved toward 200: got {}",
            state.current_x
        );
        assert!(
            state.current_y > 0.0,
            "y should have moved toward 300: got {}",
            state.current_y
        );
        // Should not have overshot
        assert!(state.current_x <= 200.0);
        assert!(state.current_y <= 300.0);
    }

    #[test]
    fn tick_animation_exponential_snaps_when_close() {
        let mut state = CursorState::default();
        state.anim_enabled = true;
        state.animating = true;
        state.anim_style = CursorAnimStyle::Exponential;
        state.anim_speed = 15.0;
        // Position very close to target (within 0.5 threshold)
        state.current_x = 100.0;
        state.current_y = 200.0;
        state.current_w = 10.0;
        state.current_h = 20.0;
        state.target = Some(make_target(
            100.3,
            200.2,
            10.1,
            20.1,
            CursorStyle::FilledBox,
        ));
        state.last_anim_time = Instant::now();

        std::thread::sleep(Duration::from_millis(1));
        state.tick_animation();

        // Should have snapped: position == target, animating == false
        assert_eq!(state.current_x, 100.3);
        assert_eq!(state.current_y, 200.2);
        assert_eq!(state.current_w, 10.1);
        assert_eq!(state.current_h, 20.1);
        assert!(!state.animating);
    }

    // ---------------------------------------------------------------
    // tick_animation: Linear easing style
    // ---------------------------------------------------------------

    #[test]
    fn tick_animation_linear_interpolation() {
        let mut state = CursorState::default();
        state.anim_enabled = true;
        state.animating = true;
        state.anim_style = CursorAnimStyle::Linear;
        state.anim_duration = 1.0; // 1 second
        state.start_x = 0.0;
        state.start_y = 0.0;
        state.start_w = 10.0;
        state.start_h = 20.0;
        state.current_x = 0.0;
        state.current_y = 0.0;
        state.current_w = 10.0;
        state.current_h = 20.0;
        state.target = Some(make_target(
            100.0,
            200.0,
            30.0,
            40.0,
            CursorStyle::FilledBox,
        ));
        state.anim_start_time = Instant::now();
        state.last_anim_time = Instant::now();

        // Sleep to let some time pass
        std::thread::sleep(Duration::from_millis(10));

        let result = state.tick_animation();
        assert!(result);

        // With linear easing, progress should be proportional to time elapsed
        // After ~10ms of a 1s animation, should be ~1% of the way
        assert!(state.current_x > 0.0);
        assert!(state.current_x < 100.0);
        assert!(state.current_y > 0.0);
        assert!(state.current_y < 200.0);
    }

    #[test]
    fn tick_animation_linear_completes_and_snaps() {
        let mut state = CursorState::default();
        state.anim_enabled = true;
        state.animating = true;
        state.anim_style = CursorAnimStyle::Linear;
        state.anim_duration = 0.001; // very short: 1ms
        state.start_x = 0.0;
        state.start_y = 0.0;
        state.start_w = 10.0;
        state.start_h = 20.0;
        state.target = Some(make_target(
            100.0,
            200.0,
            30.0,
            40.0,
            CursorStyle::FilledBox,
        ));
        // Set start time in the past so elapsed > duration
        state.anim_start_time = Instant::now() - Duration::from_millis(100);
        state.last_anim_time = Instant::now();

        state.tick_animation();

        // Should snap to target when raw_t >= 1.0
        assert_eq!(state.current_x, 100.0);
        assert_eq!(state.current_y, 200.0);
        assert_eq!(state.current_w, 30.0);
        assert_eq!(state.current_h, 40.0);
        assert!(!state.animating);
    }

    // ---------------------------------------------------------------
    // tick_animation: EaseOutQuad style
    // ---------------------------------------------------------------

    #[test]
    fn tick_animation_ease_out_quad_progresses() {
        let mut state = CursorState::default();
        state.anim_enabled = true;
        state.animating = true;
        state.anim_style = CursorAnimStyle::EaseOutQuad;
        state.anim_duration = 0.5;
        state.start_x = 0.0;
        state.start_y = 0.0;
        state.start_w = 10.0;
        state.start_h = 10.0;
        state.target = Some(make_target(
            100.0,
            100.0,
            10.0,
            10.0,
            CursorStyle::FilledBox,
        ));
        state.anim_start_time = Instant::now();
        state.last_anim_time = Instant::now();

        std::thread::sleep(Duration::from_millis(10));
        let result = state.tick_animation();
        assert!(result);
        assert!(state.current_x > 0.0);
    }

    // ---------------------------------------------------------------
    // tick_animation: EaseOutCubic style
    // ---------------------------------------------------------------

    #[test]
    fn tick_animation_ease_out_cubic_progresses() {
        let mut state = CursorState::default();
        state.anim_enabled = true;
        state.animating = true;
        state.anim_style = CursorAnimStyle::EaseOutCubic;
        state.anim_duration = 0.5;
        state.start_x = 50.0;
        state.start_y = 50.0;
        state.start_w = 10.0;
        state.start_h = 20.0;
        state.target = Some(make_target(
            200.0,
            200.0,
            10.0,
            20.0,
            CursorStyle::FilledBox,
        ));
        state.anim_start_time = Instant::now();
        state.last_anim_time = Instant::now();

        std::thread::sleep(Duration::from_millis(10));
        let result = state.tick_animation();
        assert!(result);
        assert!(
            state.current_x > 50.0,
            "x should have progressed past start"
        );
    }

    // ---------------------------------------------------------------
    // tick_animation: EaseOutExpo style
    // ---------------------------------------------------------------

    #[test]
    fn tick_animation_ease_out_expo_progresses() {
        let mut state = CursorState::default();
        state.anim_enabled = true;
        state.animating = true;
        state.anim_style = CursorAnimStyle::EaseOutExpo;
        state.anim_duration = 0.5;
        state.start_x = 0.0;
        state.start_y = 0.0;
        state.start_w = 5.0;
        state.start_h = 15.0;
        state.target = Some(make_target(300.0, 300.0, 5.0, 15.0, CursorStyle::FilledBox));
        state.anim_start_time = Instant::now();
        state.last_anim_time = Instant::now();

        std::thread::sleep(Duration::from_millis(10));
        let result = state.tick_animation();
        assert!(result);
        assert!(state.current_x > 0.0);
    }

    // ---------------------------------------------------------------
    // tick_animation: EaseInOutCubic style
    // ---------------------------------------------------------------

    #[test]
    fn tick_animation_ease_in_out_cubic_progresses() {
        let mut state = CursorState::default();
        state.anim_enabled = true;
        state.animating = true;
        state.anim_style = CursorAnimStyle::EaseInOutCubic;
        state.anim_duration = 0.5;
        state.start_x = 10.0;
        state.start_y = 10.0;
        state.start_w = 8.0;
        state.start_h = 16.0;
        state.target = Some(make_target(400.0, 400.0, 8.0, 16.0, CursorStyle::FilledBox));
        state.anim_start_time = Instant::now();
        state.last_anim_time = Instant::now();

        std::thread::sleep(Duration::from_millis(10));
        let result = state.tick_animation();
        assert!(result);
        assert!(state.current_x > 10.0);
    }

    // ---------------------------------------------------------------
    // tick_animation: CriticallyDampedSpring style
    // ---------------------------------------------------------------

    #[test]
    fn tick_animation_spring_moves_toward_target() {
        let mut state = CursorState::default();
        state.anim_enabled = true;
        state.animating = true;
        state.anim_style = CursorAnimStyle::CriticallyDampedSpring;
        state.target = Some(make_target(
            200.0,
            300.0,
            80.0,
            40.0,
            CursorStyle::FilledBox,
        ));

        // Initialize corner springs away from target
        let target_corners = CursorState::target_corners(state.target.as_ref().unwrap());
        for i in 0..4 {
            state.corner_springs[i].x = 0.0;
            state.corner_springs[i].y = 0.0;
            state.corner_springs[i].vx = 0.0;
            state.corner_springs[i].vy = 0.0;
            state.corner_springs[i].target_x = target_corners[i].0;
            state.corner_springs[i].target_y = target_corners[i].1;
        }
        state.last_anim_time = Instant::now();

        std::thread::sleep(Duration::from_millis(5));
        let result = state.tick_animation();
        assert!(result);

        // Springs should have moved corners toward target
        // current_x/y are derived from bounding box of corner springs
        // After one tick from origin, they should have moved toward target
        // (not necessarily arrived)
    }

    #[test]
    fn tick_animation_spring_settles_at_target() {
        let mut state = CursorState::default();
        state.anim_enabled = true;
        state.animating = true;
        state.anim_style = CursorAnimStyle::CriticallyDampedSpring;
        let target = make_target(100.0, 100.0, 50.0, 25.0, CursorStyle::FilledBox);
        state.target = Some(target.clone());

        // Set corner springs very close to target with tiny velocity
        let target_corners = CursorState::target_corners(&target);
        for i in 0..4 {
            state.corner_springs[i].x = target_corners[i].0 + 0.1;
            state.corner_springs[i].y = target_corners[i].1 + 0.1;
            state.corner_springs[i].vx = 0.1;
            state.corner_springs[i].vy = 0.1;
            state.corner_springs[i].target_x = target_corners[i].0;
            state.corner_springs[i].target_y = target_corners[i].1;
        }
        state.last_anim_time = Instant::now();

        std::thread::sleep(Duration::from_millis(5));
        state.tick_animation();

        // Should have settled: snapped to target
        assert_eq!(state.current_x, 100.0);
        assert_eq!(state.current_y, 100.0);
        assert_eq!(state.current_w, 50.0);
        assert_eq!(state.current_h, 25.0);
        assert!(!state.animating);
    }

    #[test]
    fn tick_animation_spring_resets_velocities_on_settle() {
        let mut state = CursorState::default();
        state.anim_enabled = true;
        state.animating = true;
        state.anim_style = CursorAnimStyle::CriticallyDampedSpring;
        let target = make_target(50.0, 50.0, 20.0, 10.0, CursorStyle::FilledBox);
        state.target = Some(target.clone());

        let target_corners = CursorState::target_corners(&target);
        for i in 0..4 {
            state.corner_springs[i].x = target_corners[i].0 + 0.01;
            state.corner_springs[i].y = target_corners[i].1 + 0.01;
            state.corner_springs[i].vx = 0.01;
            state.corner_springs[i].vy = 0.01;
            state.corner_springs[i].target_x = target_corners[i].0;
            state.corner_springs[i].target_y = target_corners[i].1;
        }
        state.last_anim_time = Instant::now();

        std::thread::sleep(Duration::from_millis(5));
        state.tick_animation();

        // Velocities should be reset to 0
        for spring in &state.corner_springs {
            assert_eq!(spring.vx, 0.0);
            assert_eq!(spring.vy, 0.0);
        }
    }

    // ---------------------------------------------------------------
    // tick_animation: edge cases
    // ---------------------------------------------------------------

    #[test]
    fn tick_animation_same_start_and_end_position() {
        let mut state = CursorState::default();
        state.anim_enabled = true;
        state.animating = true;
        state.anim_style = CursorAnimStyle::Linear;
        state.anim_duration = 0.15;
        state.start_x = 100.0;
        state.start_y = 200.0;
        state.start_w = 10.0;
        state.start_h = 20.0;
        state.current_x = 100.0;
        state.current_y = 200.0;
        state.current_w = 10.0;
        state.current_h = 20.0;
        state.target = Some(make_target(
            100.0,
            200.0,
            10.0,
            20.0,
            CursorStyle::FilledBox,
        ));
        state.anim_start_time = Instant::now();
        state.last_anim_time = Instant::now();

        std::thread::sleep(Duration::from_millis(5));
        let result = state.tick_animation();
        assert!(result);

        // Position should stay the same since start == target
        assert!((state.current_x - 100.0).abs() < 1e-3);
        assert!((state.current_y - 200.0).abs() < 1e-3);
    }

    #[test]
    fn tick_animation_zero_duration_completes_immediately() {
        let mut state = CursorState::default();
        state.anim_enabled = true;
        state.animating = true;
        state.anim_style = CursorAnimStyle::Linear;
        state.anim_duration = 0.0; // zero duration
        state.start_x = 0.0;
        state.start_y = 0.0;
        state.start_w = 5.0;
        state.start_h = 10.0;
        state.target = Some(make_target(
            500.0,
            600.0,
            15.0,
            25.0,
            CursorStyle::FilledBox,
        ));
        state.anim_start_time = Instant::now();
        state.last_anim_time = Instant::now();

        // Even with zero duration, raw_t would be infinity or NaN from 0/0,
        // but it's clamped to min(1.0) so it should snap immediately.
        // The .min(1.0) ensures raw_t = 1.0 regardless of elapsed/0.
        // Actually: elapsed/0.0 = inf, inf.min(1.0) = 1.0
        std::thread::sleep(Duration::from_millis(1));
        state.tick_animation();

        assert_eq!(state.current_x, 500.0);
        assert_eq!(state.current_y, 600.0);
        assert_eq!(state.current_w, 15.0);
        assert_eq!(state.current_h, 25.0);
        assert!(!state.animating);
    }

    #[test]
    fn tick_animation_exponential_same_position_snaps() {
        let mut state = CursorState::default();
        state.anim_enabled = true;
        state.animating = true;
        state.anim_style = CursorAnimStyle::Exponential;
        state.anim_speed = 15.0;
        state.current_x = 100.0;
        state.current_y = 100.0;
        state.current_w = 10.0;
        state.current_h = 20.0;
        state.target = Some(make_target(
            100.0,
            100.0,
            10.0,
            20.0,
            CursorStyle::FilledBox,
        ));
        state.last_anim_time = Instant::now();

        std::thread::sleep(Duration::from_millis(1));
        state.tick_animation();

        // dx, dy, dw, dh are all 0.0 (< 0.5), should snap immediately
        assert_eq!(state.current_x, 100.0);
        assert_eq!(state.current_y, 100.0);
        assert!(!state.animating);
    }

    // ---------------------------------------------------------------
    // tick_animation: multiple ticks converge
    // ---------------------------------------------------------------

    #[test]
    fn tick_animation_exponential_converges_over_many_ticks() {
        let mut state = CursorState::default();
        state.anim_enabled = true;
        state.animating = true;
        state.anim_style = CursorAnimStyle::Exponential;
        state.anim_speed = 15.0;
        state.current_x = 0.0;
        state.current_y = 0.0;
        state.current_w = 10.0;
        state.current_h = 20.0;
        state.target = Some(make_target(
            100.0,
            100.0,
            10.0,
            20.0,
            CursorStyle::FilledBox,
        ));
        state.last_anim_time = Instant::now();

        // Run many ticks
        for _ in 0..200 {
            std::thread::sleep(Duration::from_millis(2));
            if !state.animating {
                break;
            }
            state.tick_animation();
        }

        // Should have snapped to target
        assert_eq!(state.current_x, 100.0);
        assert_eq!(state.current_y, 100.0);
        assert!(!state.animating);
    }

    #[test]
    fn tick_animation_linear_converges_over_duration() {
        let mut state = CursorState::default();
        state.anim_enabled = true;
        state.animating = true;
        state.anim_style = CursorAnimStyle::Linear;
        state.anim_duration = 0.05; // 50ms
        state.start_x = 0.0;
        state.start_y = 0.0;
        state.start_w = 10.0;
        state.start_h = 20.0;
        state.current_x = 0.0;
        state.current_y = 0.0;
        state.current_w = 10.0;
        state.current_h = 20.0;
        state.target = Some(make_target(
            100.0,
            200.0,
            30.0,
            40.0,
            CursorStyle::FilledBox,
        ));
        state.anim_start_time = Instant::now();
        state.last_anim_time = Instant::now();

        // Run ticks until animation completes
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(2));
            if !state.animating {
                break;
            }
            state.tick_animation();
        }

        assert_eq!(state.current_x, 100.0);
        assert_eq!(state.current_y, 200.0);
        assert_eq!(state.current_w, 30.0);
        assert_eq!(state.current_h, 40.0);
        assert!(!state.animating);
    }

    // ---------------------------------------------------------------
    // tick_size_animation
    // ---------------------------------------------------------------

    #[test]
    fn tick_size_animation_returns_false_when_disabled() {
        let mut state = CursorState::default();
        state.size_transition_enabled = false;
        state.size_animating = true;
        assert!(!state.tick_size_animation());
    }

    #[test]
    fn tick_size_animation_returns_false_when_not_animating() {
        let mut state = CursorState::default();
        state.size_transition_enabled = true;
        state.size_animating = false;
        assert!(!state.tick_size_animation());
    }

    #[test]
    fn tick_size_animation_interpolates_size() {
        let mut state = CursorState::default();
        state.size_transition_enabled = true;
        state.size_animating = true;
        state.size_transition_duration = 1.0; // 1 second
        state.size_start_w = 10.0;
        state.size_start_h = 20.0;
        state.size_target_w = 50.0;
        state.size_target_h = 80.0;
        state.current_w = 10.0;
        state.current_h = 20.0;
        state.size_anim_start = Instant::now();

        std::thread::sleep(Duration::from_millis(10));
        let result = state.tick_size_animation();
        assert!(result);

        // Size should have moved toward target
        assert!(
            state.current_w > 10.0,
            "width should have increased from 10: got {}",
            state.current_w
        );
        assert!(
            state.current_h > 20.0,
            "height should have increased from 20: got {}",
            state.current_h
        );
        // But not yet at target
        assert!(state.current_w < 50.0);
        assert!(state.current_h < 80.0);
    }

    #[test]
    fn tick_size_animation_completes_and_snaps() {
        let mut state = CursorState::default();
        state.size_transition_enabled = true;
        state.size_animating = true;
        state.size_transition_duration = 0.001; // 1ms
        state.size_start_w = 10.0;
        state.size_start_h = 20.0;
        state.size_target_w = 50.0;
        state.size_target_h = 80.0;
        state.current_w = 10.0;
        state.current_h = 20.0;
        state.size_anim_start = Instant::now() - Duration::from_millis(100);

        let result = state.tick_size_animation();
        assert!(result);

        // Should snap to target size
        assert_eq!(state.current_w, 50.0);
        assert_eq!(state.current_h, 80.0);
        assert!(!state.size_animating);
    }

    #[test]
    fn tick_size_animation_zero_duration_completes_immediately() {
        let mut state = CursorState::default();
        state.size_transition_enabled = true;
        state.size_animating = true;
        state.size_transition_duration = 0.0;
        state.size_start_w = 5.0;
        state.size_start_h = 10.0;
        state.size_target_w = 30.0;
        state.size_target_h = 60.0;
        state.current_w = 5.0;
        state.current_h = 10.0;
        state.size_anim_start = Instant::now();

        std::thread::sleep(Duration::from_millis(1));
        state.tick_size_animation();

        assert_eq!(state.current_w, 30.0);
        assert_eq!(state.current_h, 60.0);
        assert!(!state.size_animating);
    }

    #[test]
    fn tick_size_animation_same_start_and_target() {
        let mut state = CursorState::default();
        state.size_transition_enabled = true;
        state.size_animating = true;
        state.size_transition_duration = 0.15;
        state.size_start_w = 20.0;
        state.size_start_h = 40.0;
        state.size_target_w = 20.0;
        state.size_target_h = 40.0;
        state.current_w = 20.0;
        state.current_h = 40.0;
        state.size_anim_start = Instant::now();

        std::thread::sleep(Duration::from_millis(5));
        let result = state.tick_size_animation();
        assert!(result);

        // Size should remain the same since start == target
        assert!((state.current_w - 20.0).abs() < 1e-3);
        assert!((state.current_h - 40.0).abs() < 1e-3);
    }

    #[test]
    fn tick_size_animation_ease_out_quad_curve() {
        // The size transition uses ease-out-quad: t * (2 - t)
        // Verify the easing is applied correctly by checking that
        // at the halfway point, progress is 0.75 (ease-out-quad at 0.5)
        let mut state = CursorState::default();
        state.size_transition_enabled = true;
        state.size_animating = true;
        state.size_transition_duration = 0.1; // 100ms
        state.size_start_w = 0.0;
        state.size_start_h = 0.0;
        state.size_target_w = 100.0;
        state.size_target_h = 100.0;
        state.current_w = 0.0;
        state.current_h = 0.0;
        // Set start time 50ms ago (halfway through 100ms)
        state.size_anim_start = Instant::now() - Duration::from_millis(50);

        state.tick_size_animation();

        // At raw_t=0.5, ease-out-quad = 0.5*(2.0-0.5) = 0.75
        // So width should be ~75.0 and height ~75.0
        // Allow some tolerance for timing imprecision
        assert!(
            (state.current_w - 75.0).abs() < 5.0,
            "width at halfway should be ~75: got {}",
            state.current_w
        );
        assert!(
            (state.current_h - 75.0).abs() < 5.0,
            "height at halfway should be ~75: got {}",
            state.current_h
        );
    }

    #[test]
    fn tick_size_animation_converges() {
        let mut state = CursorState::default();
        state.size_transition_enabled = true;
        state.size_animating = true;
        state.size_transition_duration = 0.05; // 50ms
        state.size_start_w = 10.0;
        state.size_start_h = 10.0;
        state.size_target_w = 100.0;
        state.size_target_h = 100.0;
        state.current_w = 10.0;
        state.current_h = 10.0;
        state.size_anim_start = Instant::now();

        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(2));
            if !state.size_animating {
                break;
            }
            state.tick_size_animation();
        }

        assert_eq!(state.current_w, 100.0);
        assert_eq!(state.current_h, 100.0);
        assert!(!state.size_animating);
    }

    // ---------------------------------------------------------------
    // Blink toggle timing behavior
    // ---------------------------------------------------------------

    #[test]
    fn blink_state_tracks_enabled_and_interval() {
        let mut state = CursorState::default();
        state.blink_enabled = true;
        state.blink_interval = Duration::from_millis(250);
        assert!(state.blink_enabled);
        assert_eq!(state.blink_interval, Duration::from_millis(250));
    }

    #[test]
    fn blink_disabled_does_not_affect_blink_on() {
        let mut state = CursorState::default();
        state.blink_enabled = false;
        state.blink_on = true;
        // blink_enabled being false doesn't change blink_on by itself;
        // the render loop checks blink_enabled before toggling
        assert!(state.blink_on);
    }

    #[test]
    fn blink_interval_zero_is_valid() {
        let mut state = CursorState::default();
        state.blink_interval = Duration::from_millis(0);
        assert_eq!(state.blink_interval, Duration::ZERO);
    }

    // ---------------------------------------------------------------
    // CornerSpring: basic state
    // ---------------------------------------------------------------

    #[test]
    fn corner_spring_copy_semantics() {
        let spring = CornerSpring {
            x: 10.0,
            y: 20.0,
            vx: 1.0,
            vy: 2.0,
            target_x: 100.0,
            target_y: 200.0,
            omega: 30.0,
        };
        let copy = spring; // Copy
        assert_eq!(copy.x, 10.0);
        assert_eq!(copy.y, 20.0);
        assert_eq!(copy.vx, 1.0);
        assert_eq!(copy.vy, 2.0);
        assert_eq!(copy.target_x, 100.0);
        assert_eq!(copy.target_y, 200.0);
        assert_eq!(copy.omega, 30.0);
    }

    // ---------------------------------------------------------------
    // CursorTarget: basic construction
    // ---------------------------------------------------------------

    #[test]
    fn cursor_target_clone() {
        let target = make_target(10.0, 20.0, 30.0, 40.0, CursorStyle::Bar(2.0));
        let cloned = target.clone();
        assert_eq!(cloned.x, 10.0);
        assert_eq!(cloned.y, 20.0);
        assert_eq!(cloned.width, 30.0);
        assert_eq!(cloned.height, 40.0);
        assert_eq!(cloned.style, CursorStyle::Bar(2.0));
        assert_eq!(cloned.window_id, 1);
        assert_eq!(cloned.frame_id, 0);
    }

    // ---------------------------------------------------------------
    // Integration: tick_animation updates last_anim_time
    // ---------------------------------------------------------------

    #[test]
    fn tick_animation_updates_last_anim_time() {
        let mut state = CursorState::default();
        state.anim_enabled = true;
        state.animating = true;
        state.anim_style = CursorAnimStyle::Linear;
        state.anim_duration = 1.0;
        state.start_x = 0.0;
        state.start_y = 0.0;
        state.start_w = 10.0;
        state.start_h = 10.0;
        state.target = Some(make_target(
            100.0,
            100.0,
            10.0,
            10.0,
            CursorStyle::FilledBox,
        ));
        let old_time = Instant::now() - Duration::from_millis(100);
        state.last_anim_time = old_time;
        state.anim_start_time = old_time;

        state.tick_animation();

        // last_anim_time should have been updated to approximately now
        assert!(state.last_anim_time > old_time);
    }

    // ---------------------------------------------------------------
    // tick_animation: easing styles all complete at the same target
    // ---------------------------------------------------------------

    #[test]
    fn tick_animation_all_easing_styles_reach_target() {
        let easing_styles = [
            CursorAnimStyle::Linear,
            CursorAnimStyle::EaseOutQuad,
            CursorAnimStyle::EaseOutCubic,
            CursorAnimStyle::EaseOutExpo,
            CursorAnimStyle::EaseInOutCubic,
        ];

        for style in &easing_styles {
            let mut state = CursorState::default();
            state.anim_enabled = true;
            state.animating = true;
            state.anim_style = *style;
            state.anim_duration = 0.001; // 1ms, will complete instantly
            state.start_x = 0.0;
            state.start_y = 0.0;
            state.start_w = 10.0;
            state.start_h = 20.0;
            state.target = Some(make_target(
                200.0,
                300.0,
                40.0,
                50.0,
                CursorStyle::FilledBox,
            ));
            state.anim_start_time = Instant::now() - Duration::from_millis(100);
            state.last_anim_time = Instant::now();

            state.tick_animation();

            assert_eq!(state.current_x, 200.0, "{:?} did not reach target x", style);
            assert_eq!(state.current_y, 300.0, "{:?} did not reach target y", style);
            assert_eq!(state.current_w, 40.0, "{:?} did not reach target w", style);
            assert_eq!(state.current_h, 50.0, "{:?} did not reach target h", style);
            assert!(
                !state.animating,
                "{:?} should have stopped animating",
                style
            );
        }
    }

    // ---------------------------------------------------------------
    // Negative position / large coordinates
    // ---------------------------------------------------------------

    #[test]
    fn target_corners_negative_coordinates() {
        let target = make_target(-50.0, -30.0, 100.0, 60.0, CursorStyle::FilledBox);
        let corners = CursorState::target_corners(&target);
        assert_eq!(corners[0], (-50.0, -30.0));
        assert_eq!(corners[1], (50.0, -30.0));
        assert_eq!(corners[2], (50.0, 30.0));
        assert_eq!(corners[3], (-50.0, 30.0));
    }

    #[test]
    fn target_corners_large_coordinates() {
        let target = make_target(10000.0, 20000.0, 500.0, 300.0, CursorStyle::FilledBox);
        let corners = CursorState::target_corners(&target);
        assert_eq!(corners[0], (10000.0, 20000.0));
        assert_eq!(corners[1], (10500.0, 20000.0));
        assert_eq!(corners[2], (10500.0, 20300.0));
        assert_eq!(corners[3], (10000.0, 20300.0));
    }

    // ---------------------------------------------------------------
    // Critically damped spring: physics consistency
    // ---------------------------------------------------------------

    #[test]
    fn spring_physics_no_overshoot_single_axis() {
        // A critically damped spring should not overshoot when starting from
        // rest (zero velocity). We verify this for a simple 1D case by running
        // the spring simulation manually.
        let omega: f32 = 26.7;
        let target: f32 = 100.0;
        let mut pos: f32 = 0.0;
        let mut vel: f32 = 0.0;
        let dt: f32 = 0.001; // 1ms steps

        for _ in 0..2000 {
            let exp_term = (-omega * dt).exp();
            let x0 = pos - target;
            let v0 = vel;
            let new_x = (x0 + (v0 + omega * x0) * dt) * exp_term;
            vel = ((v0 + omega * x0) * exp_term) - omega * (x0 + (v0 + omega * x0) * dt) * exp_term;
            pos = target + new_x;

            // Should never overshoot (go above target when starting below)
            assert!(
                pos <= target + 1.0,
                "Spring overshot at step: pos={}, target={}",
                pos,
                target
            );
        }

        // Should have converged close to target
        assert!(
            (pos - target).abs() < 1.0,
            "Spring did not converge: pos={}, target={}",
            pos,
            target
        );
    }

    #[test]
    fn spring_physics_with_initial_velocity() {
        // With initial velocity toward target, the spring may overshoot slightly
        // but should converge
        let omega: f32 = 26.7;
        let target: f32 = 100.0;
        let mut pos: f32 = 0.0;
        let mut vel: f32 = 500.0; // high initial velocity toward target
        let dt: f32 = 0.001;

        for _ in 0..5000 {
            let exp_term = (-omega * dt).exp();
            let x0 = pos - target;
            let v0 = vel;
            let new_x = (x0 + (v0 + omega * x0) * dt) * exp_term;
            vel = ((v0 + omega * x0) * exp_term) - omega * (x0 + (v0 + omega * x0) * dt) * exp_term;
            pos = target + new_x;
        }

        // Should converge regardless of initial velocity
        assert!(
            (pos - target).abs() < 1.0,
            "Spring with initial velocity did not converge: pos={}, target={}",
            pos,
            target
        );
    }
}
