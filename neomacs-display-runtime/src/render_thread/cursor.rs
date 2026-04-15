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
#[path = "cursor_test.rs"]
mod tests;
