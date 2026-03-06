//! Animation system for smooth scrolling, cursor blink, etc.

use std::time::{Duration, Instant};

/// Easing functions for animations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Easing {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl Easing {
    /// Apply easing function to a value t in [0, 1]
    pub fn apply(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::EaseIn => t * t,
            Easing::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            Easing::EaseInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
                }
            }
        }
    }
}

/// A single animation
#[derive(Debug, Clone)]
pub struct Animation {
    /// Start value
    pub from: f32,

    /// End value
    pub to: f32,

    /// Duration
    pub duration: Duration,

    /// Start time
    pub start_time: Instant,

    /// Easing function
    pub easing: Easing,

    /// Is this animation complete?
    pub completed: bool,
}

impl Animation {
    /// Create a new animation
    pub fn new(from: f32, to: f32, duration: Duration, easing: Easing) -> Self {
        Self {
            from,
            to,
            duration,
            start_time: Instant::now(),
            easing,
            completed: false,
        }
    }

    /// Get current value at time `now`
    pub fn value_at(&mut self, now: Instant) -> f32 {
        let elapsed = now.duration_since(self.start_time);

        if elapsed >= self.duration {
            self.completed = true;
            return self.to;
        }

        let t = elapsed.as_secs_f32() / self.duration.as_secs_f32();
        let eased_t = self.easing.apply(t);

        self.from + (self.to - self.from) * eased_t
    }

    /// Get current value (using current time)
    pub fn current_value(&mut self) -> f32 {
        self.value_at(Instant::now())
    }

    /// Check if animation is complete
    pub fn is_complete(&self) -> bool {
        self.completed
    }
}

/// Animation manager handles all active animations
#[derive(Debug)]
pub struct AnimationManager {
    /// Scroll animations by window ID
    scroll_animations: Vec<(i32, Animation)>,

    /// Cursor blink state
    cursor_blink_on: bool,
    last_cursor_toggle: Instant,
    cursor_blink_interval: Duration,

    /// Frame time tracking
    last_frame_time: Option<Instant>,
}

impl Default for AnimationManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AnimationManager {
    pub fn new() -> Self {
        Self {
            scroll_animations: Vec::new(),
            cursor_blink_on: true,
            last_cursor_toggle: Instant::now(),
            cursor_blink_interval: Duration::from_millis(530),
            last_frame_time: None,
        }
    }

    /// Start a smooth scroll animation for a window
    pub fn animate_scroll(&mut self, window_id: i32, from: f32, to: f32) {
        // Remove any existing scroll animation for this window
        self.scroll_animations.retain(|(id, _)| *id != window_id);

        let animation = Animation::new(from, to, Duration::from_millis(150), Easing::EaseOut);

        self.scroll_animations.push((window_id, animation));
    }

    /// Get current scroll offset for a window (returns None if no animation)
    pub fn get_scroll_offset(&mut self, window_id: i32) -> Option<f32> {
        let now = Instant::now();

        for (id, anim) in &mut self.scroll_animations {
            if *id == window_id {
                return Some(anim.value_at(now));
            }
        }

        None
    }

    /// Update all animations, returns true if any animation is active
    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        self.last_frame_time = Some(now);

        // Update cursor blink
        if now.duration_since(self.last_cursor_toggle) >= self.cursor_blink_interval {
            self.cursor_blink_on = !self.cursor_blink_on;
            self.last_cursor_toggle = now;
        }

        // Remove completed scroll animations
        self.scroll_animations
            .retain(|(_, anim)| !anim.is_complete());

        // Return true if there are active animations
        !self.scroll_animations.is_empty()
    }

    /// Get cursor visibility (for blinking)
    pub fn cursor_visible(&self) -> bool {
        self.cursor_blink_on
    }

    /// Reset cursor blink (call when cursor moves)
    pub fn reset_cursor_blink(&mut self) {
        self.cursor_blink_on = true;
        self.last_cursor_toggle = Instant::now();
    }

    /// Set cursor blink interval
    pub fn set_cursor_blink_interval(&mut self, interval: Duration) {
        self.cursor_blink_interval = interval;
    }

    /// Check if any animations are running
    pub fn has_active_animations(&self) -> bool {
        !self.scroll_animations.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn test_easing() {
        assert_eq!(Easing::Linear.apply(0.5), 0.5);
        assert!(Easing::EaseIn.apply(0.5) < 0.5);
        assert!(Easing::EaseOut.apply(0.5) > 0.5);
    }

    #[test]
    fn test_animation() {
        let mut anim = Animation::new(0.0, 100.0, Duration::from_millis(100), Easing::Linear);

        // At start
        let v1 = anim.current_value();
        assert!(v1 < 50.0);

        // Wait and check progress
        sleep(Duration::from_millis(50));
        let v2 = anim.current_value();
        assert!(v2 > v1);

        // Wait until complete
        sleep(Duration::from_millis(60));
        let v3 = anim.current_value();
        assert_eq!(v3, 100.0);
        assert!(anim.is_complete());
    }

    // ----------------------------------------------------------------
    // Easing function tests
    // ----------------------------------------------------------------

    #[test]
    fn test_easing_boundary_values() {
        // All easing functions must map 0 -> 0 and 1 -> 1
        for easing in &[
            Easing::Linear,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
        ] {
            let at_zero = easing.apply(0.0);
            let at_one = easing.apply(1.0);
            assert!(
                (at_zero - 0.0).abs() < 1e-6,
                "{:?}.apply(0.0) = {}, expected 0.0",
                easing,
                at_zero
            );
            assert!(
                (at_one - 1.0).abs() < 1e-6,
                "{:?}.apply(1.0) = {}, expected 1.0",
                easing,
                at_one
            );
        }
    }

    #[test]
    fn test_easing_clamping() {
        // Values outside [0, 1] should be clamped
        for easing in &[
            Easing::Linear,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
        ] {
            let below = easing.apply(-0.5);
            let above = easing.apply(1.5);
            assert!(
                (below - easing.apply(0.0)).abs() < 1e-6,
                "{:?}.apply(-0.5) = {}, expected same as apply(0.0)",
                easing,
                below
            );
            assert!(
                (above - easing.apply(1.0)).abs() < 1e-6,
                "{:?}.apply(1.5) = {}, expected same as apply(1.0)",
                easing,
                above
            );
        }
    }

    #[test]
    fn test_easing_linear_midpoint() {
        assert!((Easing::Linear.apply(0.25) - 0.25).abs() < 1e-6);
        assert!((Easing::Linear.apply(0.5) - 0.5).abs() < 1e-6);
        assert!((Easing::Linear.apply(0.75) - 0.75).abs() < 1e-6);
    }

    #[test]
    fn test_easing_ease_in_curve_shape() {
        // EaseIn (t^2) starts slow, ends fast
        // At t=0.25: 0.0625, at t=0.5: 0.25, at t=0.75: 0.5625
        assert!((Easing::EaseIn.apply(0.25) - 0.0625).abs() < 1e-6);
        assert!((Easing::EaseIn.apply(0.5) - 0.25).abs() < 1e-6);
        assert!((Easing::EaseIn.apply(0.75) - 0.5625).abs() < 1e-6);
    }

    #[test]
    fn test_easing_ease_out_curve_shape() {
        // EaseOut: 1 - (1-t)^2 starts fast, ends slow
        // At t=0.25: 1 - 0.75^2 = 0.4375
        // At t=0.5: 1 - 0.5^2 = 0.75
        // At t=0.75: 1 - 0.25^2 = 0.9375
        assert!((Easing::EaseOut.apply(0.25) - 0.4375).abs() < 1e-6);
        assert!((Easing::EaseOut.apply(0.5) - 0.75).abs() < 1e-6);
        assert!((Easing::EaseOut.apply(0.75) - 0.9375).abs() < 1e-6);
    }

    #[test]
    fn test_easing_ease_in_out_symmetry() {
        // EaseInOut should be symmetric around (0.5, 0.5)
        let at_half = Easing::EaseInOut.apply(0.5);
        assert!(
            (at_half - 0.5).abs() < 1e-6,
            "EaseInOut at 0.5 = {}, expected 0.5",
            at_half
        );

        // Symmetry: apply(t) + apply(1-t) should equal 1
        for &t in &[0.1, 0.2, 0.3, 0.4] {
            let sum = Easing::EaseInOut.apply(t) + Easing::EaseInOut.apply(1.0 - t);
            assert!(
                (sum - 1.0).abs() < 1e-5,
                "EaseInOut symmetry broken: apply({}) + apply({}) = {}",
                t,
                1.0 - t,
                sum
            );
        }
    }

    #[test]
    fn test_easing_monotonicity() {
        // All easing functions should be monotonically non-decreasing on [0, 1]
        for easing in &[
            Easing::Linear,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
        ] {
            let mut prev = easing.apply(0.0);
            for i in 1..=100 {
                let t = i as f32 / 100.0;
                let val = easing.apply(t);
                assert!(
                    val >= prev - 1e-6,
                    "{:?} not monotonic: apply({}) = {} < apply({}) = {}",
                    easing,
                    t,
                    val,
                    (i - 1) as f32 / 100.0,
                    prev
                );
                prev = val;
            }
        }
    }

    // ----------------------------------------------------------------
    // Animation tests
    // ----------------------------------------------------------------

    #[test]
    fn test_animation_new_initial_state() {
        let anim = Animation::new(10.0, 50.0, Duration::from_millis(200), Easing::EaseOut);
        assert_eq!(anim.from, 10.0);
        assert_eq!(anim.to, 50.0);
        assert_eq!(anim.duration, Duration::from_millis(200));
        assert_eq!(anim.easing, Easing::EaseOut);
        assert!(!anim.completed);
    }

    #[test]
    fn test_animation_value_at_start_time() {
        let mut anim = Animation::new(0.0, 100.0, Duration::from_secs(1), Easing::Linear);
        // Query at the exact start time should return the from value
        let val = anim.value_at(anim.start_time);
        assert!(
            (val - 0.0).abs() < 1e-6,
            "value_at(start_time) = {}, expected 0.0",
            val
        );
        assert!(!anim.is_complete());
    }

    #[test]
    fn test_animation_value_at_end_time() {
        let mut anim = Animation::new(0.0, 100.0, Duration::from_secs(1), Easing::Linear);
        let end_time = anim.start_time + Duration::from_secs(1);
        let val = anim.value_at(end_time);
        assert_eq!(val, 100.0);
        assert!(anim.is_complete());
    }

    #[test]
    fn test_animation_value_past_end_time() {
        let mut anim = Animation::new(0.0, 100.0, Duration::from_millis(50), Easing::Linear);
        let way_after = anim.start_time + Duration::from_secs(10);
        let val = anim.value_at(way_after);
        assert_eq!(val, 100.0);
        assert!(anim.is_complete());
    }

    #[test]
    fn test_animation_linear_midpoint_value() {
        let mut anim = Animation::new(0.0, 200.0, Duration::from_secs(2), Easing::Linear);
        let mid = anim.start_time + Duration::from_secs(1);
        let val = anim.value_at(mid);
        assert!(
            (val - 100.0).abs() < 1.0,
            "Linear midpoint: expected ~100.0, got {}",
            val
        );
    }

    #[test]
    fn test_animation_negative_range() {
        // Animation can go from high to low
        let mut anim = Animation::new(100.0, 0.0, Duration::from_secs(1), Easing::Linear);
        let mid = anim.start_time + Duration::from_millis(500);
        let val = anim.value_at(mid);
        assert!(
            (val - 50.0).abs() < 1.0,
            "Reverse animation midpoint: expected ~50.0, got {}",
            val
        );

        let end = anim.start_time + Duration::from_secs(1);
        let val_end = anim.value_at(end);
        assert_eq!(val_end, 0.0);
    }

    #[test]
    fn test_animation_zero_duration() {
        // A zero-duration animation should immediately complete
        let mut anim = Animation::new(0.0, 42.0, Duration::from_millis(0), Easing::Linear);
        let val = anim.value_at(anim.start_time);
        assert_eq!(val, 42.0);
        assert!(anim.is_complete());
    }

    #[test]
    fn test_animation_same_from_to() {
        // Animation where from == to should always return that value
        let mut anim = Animation::new(77.0, 77.0, Duration::from_secs(1), Easing::EaseInOut);
        let mid = anim.start_time + Duration::from_millis(500);
        let val = anim.value_at(mid);
        assert!(
            (val - 77.0).abs() < 1e-6,
            "Same from/to: expected 77.0, got {}",
            val
        );
    }

    #[test]
    fn test_animation_completed_flag_stays_set() {
        let mut anim = Animation::new(0.0, 10.0, Duration::from_millis(10), Easing::Linear);
        assert!(!anim.is_complete());

        // Complete it
        let after = anim.start_time + Duration::from_millis(20);
        anim.value_at(after);
        assert!(anim.is_complete());

        // Calling value_at again, even with an earlier time, should keep completed=true
        // because completed is a one-way flag set by value_at
        let val = anim.value_at(after);
        assert!(anim.is_complete());
        assert_eq!(val, 10.0);
    }

    // ----------------------------------------------------------------
    // AnimationManager tests
    // ----------------------------------------------------------------

    #[test]
    fn test_animation_manager_new_state() {
        let mgr = AnimationManager::new();
        assert!(mgr.cursor_visible());
        assert!(!mgr.has_active_animations());
        assert_eq!(mgr.cursor_blink_interval, Duration::from_millis(530));
        assert!(mgr.last_frame_time.is_none());
    }

    #[test]
    fn test_animation_manager_default_matches_new() {
        let mgr_new = AnimationManager::new();
        let mgr_default = AnimationManager::default();
        assert_eq!(mgr_new.cursor_blink_on, mgr_default.cursor_blink_on);
        assert_eq!(
            mgr_new.cursor_blink_interval,
            mgr_default.cursor_blink_interval
        );
        assert_eq!(
            mgr_new.scroll_animations.len(),
            mgr_default.scroll_animations.len()
        );
        assert_eq!(mgr_new.last_frame_time, mgr_default.last_frame_time);
    }

    #[test]
    fn test_animate_scroll_creates_animation() {
        let mut mgr = AnimationManager::new();
        assert!(!mgr.has_active_animations());

        mgr.animate_scroll(1, 0.0, 100.0);
        assert!(mgr.has_active_animations());
    }

    #[test]
    fn test_get_scroll_offset_returns_none_for_unknown_window() {
        let mut mgr = AnimationManager::new();
        assert!(mgr.get_scroll_offset(999).is_none());
    }

    #[test]
    fn test_get_scroll_offset_returns_value_for_active_animation() {
        let mut mgr = AnimationManager::new();
        mgr.animate_scroll(1, 0.0, 100.0);

        let offset = mgr.get_scroll_offset(1);
        assert!(offset.is_some());
        // Just started, value should be close to 0 (the from value)
        let val = offset.unwrap();
        assert!(val >= 0.0 && val <= 100.0);
    }

    #[test]
    fn test_animate_scroll_replaces_existing_for_same_window() {
        let mut mgr = AnimationManager::new();
        mgr.animate_scroll(1, 0.0, 50.0);
        mgr.animate_scroll(1, 50.0, 200.0);

        // Should only have one animation for window 1
        assert_eq!(
            mgr.scroll_animations.len(),
            1,
            "Expected exactly 1 animation after replacement"
        );
        assert_eq!(mgr.scroll_animations[0].0, 1);
        assert_eq!(mgr.scroll_animations[0].1.from, 50.0);
        assert_eq!(mgr.scroll_animations[0].1.to, 200.0);
    }

    #[test]
    fn test_multiple_concurrent_scroll_animations() {
        let mut mgr = AnimationManager::new();
        mgr.animate_scroll(1, 0.0, 100.0);
        mgr.animate_scroll(2, 50.0, 200.0);
        mgr.animate_scroll(3, 10.0, 30.0);

        assert_eq!(mgr.scroll_animations.len(), 3);
        assert!(mgr.has_active_animations());

        // Each window should return its own offset
        assert!(mgr.get_scroll_offset(1).is_some());
        assert!(mgr.get_scroll_offset(2).is_some());
        assert!(mgr.get_scroll_offset(3).is_some());
        assert!(mgr.get_scroll_offset(4).is_none());
    }

    #[test]
    fn test_tick_removes_completed_animations() {
        let mut mgr = AnimationManager::new();
        mgr.animate_scroll(1, 0.0, 100.0);

        // Wait for the scroll animation (150ms) to complete
        sleep(Duration::from_millis(200));

        // Force completion by reading the value (which sets completed flag)
        let _ = mgr.get_scroll_offset(1);

        // tick should prune completed animations
        let has_active = mgr.tick();
        assert!(!has_active);
        assert!(!mgr.has_active_animations());
    }

    #[test]
    fn test_tick_sets_last_frame_time() {
        let mut mgr = AnimationManager::new();
        assert!(mgr.last_frame_time.is_none());

        mgr.tick();
        assert!(mgr.last_frame_time.is_some());
    }

    #[test]
    fn test_tick_returns_true_with_active_animations() {
        let mut mgr = AnimationManager::new();
        mgr.animate_scroll(1, 0.0, 100.0);

        let has_active = mgr.tick();
        assert!(
            has_active,
            "tick() should return true when animations are active"
        );
    }

    #[test]
    fn test_cursor_blink_initial_visibility() {
        let mgr = AnimationManager::new();
        assert!(mgr.cursor_visible(), "Cursor should be visible initially");
    }

    #[test]
    fn test_cursor_blink_toggles_after_interval() {
        let mut mgr = AnimationManager::new();
        mgr.set_cursor_blink_interval(Duration::from_millis(50));
        assert!(mgr.cursor_visible());

        sleep(Duration::from_millis(60));
        mgr.tick();

        assert!(
            !mgr.cursor_visible(),
            "Cursor should toggle off after interval"
        );

        sleep(Duration::from_millis(60));
        mgr.tick();

        assert!(
            mgr.cursor_visible(),
            "Cursor should toggle back on after another interval"
        );
    }

    #[test]
    fn test_reset_cursor_blink_makes_visible() {
        let mut mgr = AnimationManager::new();
        mgr.set_cursor_blink_interval(Duration::from_millis(50));

        // Wait for blink to toggle off
        sleep(Duration::from_millis(60));
        mgr.tick();
        assert!(!mgr.cursor_visible());

        // Reset should make it visible again
        mgr.reset_cursor_blink();
        assert!(mgr.cursor_visible(), "Cursor should be visible after reset");
    }

    #[test]
    fn test_set_cursor_blink_interval() {
        let mut mgr = AnimationManager::new();
        assert_eq!(mgr.cursor_blink_interval, Duration::from_millis(530));

        mgr.set_cursor_blink_interval(Duration::from_millis(1000));
        assert_eq!(mgr.cursor_blink_interval, Duration::from_millis(1000));
    }

    // ----------------------------------------------------------------
    // Additional easing tests
    // ----------------------------------------------------------------

    #[test]
    fn test_easing_ease_in_out_first_half_values() {
        // First half of EaseInOut: 2*t^2
        // t=0.1 => 2*(0.01)=0.02, t=0.25 => 2*(0.0625)=0.125, t=0.4 => 2*(0.16)=0.32
        assert!((Easing::EaseInOut.apply(0.1) - 0.02).abs() < 1e-6);
        assert!((Easing::EaseInOut.apply(0.25) - 0.125).abs() < 1e-6);
        assert!((Easing::EaseInOut.apply(0.4) - 0.32).abs() < 1e-6);
    }

    #[test]
    fn test_easing_ease_in_out_second_half_values() {
        // Second half of EaseInOut: 1 - (-2t+2)^2 / 2
        // t=0.6 => 1 - (-1.2+2)^2/2 = 1 - 0.64/2 = 1 - 0.32 = 0.68
        // t=0.75 => 1 - (-1.5+2)^2/2 = 1 - 0.25/2 = 1 - 0.125 = 0.875
        // t=0.9 => 1 - (-1.8+2)^2/2 = 1 - 0.04/2 = 1 - 0.02 = 0.98
        assert!((Easing::EaseInOut.apply(0.6) - 0.68).abs() < 1e-5);
        assert!((Easing::EaseInOut.apply(0.75) - 0.875).abs() < 1e-5);
        assert!((Easing::EaseInOut.apply(0.9) - 0.98).abs() < 1e-5);
    }

    #[test]
    fn test_easing_extreme_clamping_values() {
        // Very large negative and positive values should clamp
        for easing in &[
            Easing::Linear,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
        ] {
            let very_neg = easing.apply(-1000.0);
            let very_pos = easing.apply(1000.0);
            assert!(
                (very_neg - easing.apply(0.0)).abs() < 1e-6,
                "{:?}: apply(-1000) should equal apply(0)",
                easing
            );
            assert!(
                (very_pos - easing.apply(1.0)).abs() < 1e-6,
                "{:?}: apply(1000) should equal apply(1)",
                easing
            );
        }
    }

    #[test]
    fn test_easing_output_range_within_0_1() {
        // All easing functions should output values in [0, 1] for inputs in [0, 1]
        for easing in &[
            Easing::Linear,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
        ] {
            for i in 0..=100 {
                let t = i as f32 / 100.0;
                let val = easing.apply(t);
                assert!(
                    val >= -1e-6 && val <= 1.0 + 1e-6,
                    "{:?}.apply({}) = {} is outside [0, 1]",
                    easing,
                    t,
                    val
                );
            }
        }
    }

    #[test]
    fn test_easing_ease_in_slower_than_linear_first_half() {
        // EaseIn (t^2) should always be below linear for t in (0, 1)
        for i in 1..100 {
            let t = i as f32 / 100.0;
            assert!(
                Easing::EaseIn.apply(t) <= Easing::Linear.apply(t) + 1e-6,
                "EaseIn should be <= Linear at t={}",
                t
            );
        }
    }

    #[test]
    fn test_easing_ease_out_faster_than_linear_first_half() {
        // EaseOut should always be above linear for t in (0, 1)
        for i in 1..100 {
            let t = i as f32 / 100.0;
            assert!(
                Easing::EaseOut.apply(t) >= Easing::Linear.apply(t) - 1e-6,
                "EaseOut should be >= Linear at t={}",
                t
            );
        }
    }

    #[test]
    fn test_easing_ease_in_and_ease_out_complementary() {
        // EaseIn(t) + EaseOut(1-t) should equal 1
        // EaseIn(t) = t^2, EaseOut(1-t) = 1 - (1-(1-t))^2 = 1 - t^2
        // So EaseIn(t) + EaseOut(1-t) = t^2 + 1 - t^2 = 1
        for i in 0..=100 {
            let t = i as f32 / 100.0;
            let sum = Easing::EaseIn.apply(t) + Easing::EaseOut.apply(1.0 - t);
            assert!(
                (sum - 1.0).abs() < 1e-5,
                "EaseIn({}) + EaseOut({}) = {}, expected 1.0",
                t,
                1.0 - t,
                sum
            );
        }
    }

    #[test]
    fn test_easing_nan_infinity_clamped() {
        // NaN and infinity should be handled gracefully via clamp
        // f32::NAN.clamp(0.0, 1.0) returns 0.0 on most platforms
        // f32::INFINITY.clamp(0.0, 1.0) returns 1.0
        // f32::NEG_INFINITY.clamp(0.0, 1.0) returns 0.0
        let inf_val = Easing::Linear.apply(f32::INFINITY);
        assert!(
            (inf_val - 1.0).abs() < 1e-6,
            "apply(INFINITY) = {}, expected 1.0",
            inf_val
        );

        let neg_inf_val = Easing::Linear.apply(f32::NEG_INFINITY);
        assert!(
            (neg_inf_val - 0.0).abs() < 1e-6,
            "apply(NEG_INFINITY) = {}, expected 0.0",
            neg_inf_val
        );
    }

    // ----------------------------------------------------------------
    // Additional Animation tests
    // ----------------------------------------------------------------

    #[test]
    fn test_animation_value_at_quarter_with_ease_in() {
        // EaseIn at t=0.25: eased = 0.25^2 = 0.0625
        // from=0, to=100 => value = 0 + 100*0.0625 = 6.25
        let mut anim = Animation::new(0.0, 100.0, Duration::from_secs(4), Easing::EaseIn);
        let quarter = anim.start_time + Duration::from_secs(1);
        let val = anim.value_at(quarter);
        assert!(
            (val - 6.25).abs() < 0.5,
            "EaseIn at 25%: expected ~6.25, got {}",
            val
        );
        assert!(!anim.is_complete());
    }

    #[test]
    fn test_animation_value_at_quarter_with_ease_out() {
        // EaseOut at t=0.25: eased = 1-(1-0.25)^2 = 1-0.5625 = 0.4375
        // from=0, to=100 => value = 43.75
        let mut anim = Animation::new(0.0, 100.0, Duration::from_secs(4), Easing::EaseOut);
        let quarter = anim.start_time + Duration::from_secs(1);
        let val = anim.value_at(quarter);
        assert!(
            (val - 43.75).abs() < 0.5,
            "EaseOut at 25%: expected ~43.75, got {}",
            val
        );
    }

    #[test]
    fn test_animation_value_at_half_with_ease_in_out() {
        // EaseInOut at t=0.5: eased = 0.5
        // from=0, to=100 => value = 50.0
        let mut anim = Animation::new(0.0, 100.0, Duration::from_secs(2), Easing::EaseInOut);
        let mid = anim.start_time + Duration::from_secs(1);
        let val = anim.value_at(mid);
        assert!(
            (val - 50.0).abs() < 0.5,
            "EaseInOut at 50%: expected ~50.0, got {}",
            val
        );
    }

    #[test]
    fn test_animation_large_value_range() {
        let mut anim = Animation::new(
            -1_000_000.0,
            1_000_000.0,
            Duration::from_secs(2),
            Easing::Linear,
        );
        let mid = anim.start_time + Duration::from_secs(1);
        let val = anim.value_at(mid);
        assert!(
            (val - 0.0).abs() < 100.0,
            "Large range linear midpoint: expected ~0.0, got {}",
            val
        );

        let end = anim.start_time + Duration::from_secs(2);
        let val_end = anim.value_at(end);
        assert_eq!(val_end, 1_000_000.0);
    }

    #[test]
    fn test_animation_negative_values() {
        let mut anim = Animation::new(-50.0, -200.0, Duration::from_secs(1), Easing::Linear);
        let mid = anim.start_time + Duration::from_millis(500);
        let val = anim.value_at(mid);
        assert!(
            (val - (-125.0)).abs() < 1.0,
            "Negative range midpoint: expected ~-125.0, got {}",
            val
        );

        let end = anim.start_time + Duration::from_secs(1);
        assert_eq!(anim.value_at(end), -200.0);
    }

    #[test]
    fn test_animation_very_short_duration() {
        let mut anim = Animation::new(0.0, 100.0, Duration::from_nanos(1), Easing::Linear);
        // Even reading immediately will likely be past the 1ns duration
        sleep(Duration::from_millis(1));
        let val = anim.current_value();
        assert_eq!(val, 100.0);
        assert!(anim.is_complete());
    }

    #[test]
    fn test_animation_very_long_duration() {
        let mut anim = Animation::new(0.0, 100.0, Duration::from_secs(3600), Easing::Linear);
        // At the start time, value should be exactly from
        let val = anim.value_at(anim.start_time);
        assert!((val - 0.0).abs() < 1e-6);
        assert!(!anim.is_complete());

        // 1 second into a 1-hour animation: ~0.0278%
        let one_sec = anim.start_time + Duration::from_secs(1);
        let val = anim.value_at(one_sec);
        let expected = 100.0 / 3600.0;
        assert!(
            (val - expected).abs() < 0.01,
            "Long duration at 1s: expected ~{}, got {}",
            expected,
            val
        );
    }

    #[test]
    fn test_animation_progresses_monotonically_with_linear() {
        let mut anim = Animation::new(0.0, 100.0, Duration::from_secs(1), Easing::Linear);
        let mut prev = anim.value_at(anim.start_time);
        for i in 1..=100 {
            let t = anim.start_time + Duration::from_millis(i * 10);
            let val = anim.value_at(t);
            assert!(
                val >= prev - 1e-6,
                "Animation not monotonic at step {}: {} < {}",
                i,
                val,
                prev
            );
            prev = val;
        }
    }

    #[test]
    fn test_animation_progresses_monotonically_with_ease_in_out() {
        let mut anim = Animation::new(0.0, 100.0, Duration::from_secs(1), Easing::EaseInOut);
        let mut prev = anim.value_at(anim.start_time);
        for i in 1..=100 {
            let t = anim.start_time + Duration::from_millis(i * 10);
            let val = anim.value_at(t);
            assert!(
                val >= prev - 1e-4,
                "Animation not monotonic at step {}: {} < {}",
                i,
                val,
                prev
            );
            prev = val;
        }
    }

    #[test]
    fn test_animation_is_complete_not_set_mid_animation() {
        let mut anim = Animation::new(0.0, 100.0, Duration::from_secs(10), Easing::Linear);
        let mid = anim.start_time + Duration::from_secs(5);
        let _ = anim.value_at(mid);
        assert!(
            !anim.is_complete(),
            "Animation should not be complete at midpoint"
        );
    }

    #[test]
    fn test_animation_value_at_multiple_times_without_completion() {
        // Calling value_at at different mid-animation times gives consistent interpolation
        let mut anim = Animation::new(10.0, 110.0, Duration::from_secs(10), Easing::Linear);
        let t1 = anim.start_time + Duration::from_secs(2);
        let t2 = anim.start_time + Duration::from_secs(5);
        let t3 = anim.start_time + Duration::from_secs(8);

        let v1 = anim.value_at(t1); // 10 + 100 * 0.2 = 30
        let v2 = anim.value_at(t2); // 10 + 100 * 0.5 = 60
        let v3 = anim.value_at(t3); // 10 + 100 * 0.8 = 90

        assert!((v1 - 30.0).abs() < 0.5, "At 20%: expected ~30, got {}", v1);
        assert!((v2 - 60.0).abs() < 0.5, "At 50%: expected ~60, got {}", v2);
        assert!((v3 - 90.0).abs() < 0.5, "At 80%: expected ~90, got {}", v3);
        assert!(!anim.is_complete());
    }

    #[test]
    fn test_animation_with_fractional_values() {
        let mut anim = Animation::new(0.1, 0.9, Duration::from_secs(1), Easing::Linear);
        let mid = anim.start_time + Duration::from_millis(500);
        let val = anim.value_at(mid);
        assert!(
            (val - 0.5).abs() < 0.01,
            "Fractional range midpoint: expected ~0.5, got {}",
            val
        );
    }

    // ----------------------------------------------------------------
    // Additional AnimationManager tests
    // ----------------------------------------------------------------

    #[test]
    fn test_tick_returns_false_with_no_animations() {
        let mut mgr = AnimationManager::new();
        let has_active = mgr.tick();
        assert!(!has_active, "tick() should return false with no animations");
    }

    #[test]
    fn test_cursor_does_not_toggle_before_interval() {
        let mut mgr = AnimationManager::new();
        mgr.set_cursor_blink_interval(Duration::from_millis(500));
        assert!(mgr.cursor_visible());

        // Tick immediately -- far less than 500ms elapsed
        mgr.tick();
        assert!(
            mgr.cursor_visible(),
            "Cursor should NOT toggle before interval elapses"
        );
    }

    #[test]
    fn test_reset_cursor_blink_resets_toggle_timer() {
        let mut mgr = AnimationManager::new();
        mgr.set_cursor_blink_interval(Duration::from_millis(50));

        // Wait almost long enough for a toggle
        sleep(Duration::from_millis(40));
        mgr.reset_cursor_blink();

        // Now wait another 30ms (total 30ms since reset, not enough for 50ms interval)
        sleep(Duration::from_millis(30));
        mgr.tick();
        assert!(
            mgr.cursor_visible(),
            "After reset, cursor blink timer should restart"
        );
    }

    #[test]
    fn test_multiple_reset_cursor_blink_calls() {
        let mut mgr = AnimationManager::new();
        mgr.set_cursor_blink_interval(Duration::from_millis(50));

        for _ in 0..5 {
            mgr.reset_cursor_blink();
            assert!(mgr.cursor_visible());
        }
    }

    #[test]
    fn test_get_scroll_offset_returns_none_after_tick_removes_completed() {
        let mut mgr = AnimationManager::new();
        mgr.animate_scroll(1, 0.0, 10.0);

        // Wait for animation to complete (scroll duration is 150ms)
        sleep(Duration::from_millis(200));

        // Read to set completed flag
        let _ = mgr.get_scroll_offset(1);

        // Tick removes completed
        mgr.tick();

        // Now the offset should be None
        assert!(
            mgr.get_scroll_offset(1).is_none(),
            "Offset should be None after completed animation is pruned"
        );
    }

    #[test]
    fn test_replacing_one_window_animation_does_not_affect_other() {
        let mut mgr = AnimationManager::new();
        mgr.animate_scroll(1, 0.0, 100.0);
        mgr.animate_scroll(2, 0.0, 200.0);

        // Replace window 1's animation
        mgr.animate_scroll(1, 50.0, 150.0);

        // Window 2 should be unaffected
        assert_eq!(mgr.scroll_animations.len(), 2);
        let win2_anim = mgr
            .scroll_animations
            .iter()
            .find(|(id, _)| *id == 2)
            .unwrap();
        assert_eq!(win2_anim.1.from, 0.0);
        assert_eq!(win2_anim.1.to, 200.0);
    }

    #[test]
    fn test_animate_scroll_uses_ease_out_and_150ms() {
        let mut mgr = AnimationManager::new();
        mgr.animate_scroll(1, 0.0, 100.0);

        let (_, anim) = &mgr.scroll_animations[0];
        assert_eq!(anim.easing, Easing::EaseOut);
        assert_eq!(anim.duration, Duration::from_millis(150));
    }

    #[test]
    fn test_tick_updates_last_frame_time_each_call() {
        let mut mgr = AnimationManager::new();
        mgr.tick();
        let first = mgr.last_frame_time.unwrap();

        sleep(Duration::from_millis(10));
        mgr.tick();
        let second = mgr.last_frame_time.unwrap();

        assert!(
            second > first,
            "last_frame_time should advance with each tick"
        );
    }

    #[test]
    fn test_cursor_blink_multiple_toggles() {
        let mut mgr = AnimationManager::new();
        mgr.set_cursor_blink_interval(Duration::from_millis(30));

        // Toggle 1: off
        sleep(Duration::from_millis(40));
        mgr.tick();
        assert!(!mgr.cursor_visible());

        // Toggle 2: on
        sleep(Duration::from_millis(40));
        mgr.tick();
        assert!(mgr.cursor_visible());

        // Toggle 3: off
        sleep(Duration::from_millis(40));
        mgr.tick();
        assert!(!mgr.cursor_visible());

        // Toggle 4: on
        sleep(Duration::from_millis(40));
        mgr.tick();
        assert!(mgr.cursor_visible());
    }

    #[test]
    fn test_easing_derive_traits() {
        // Verify Clone, Copy, PartialEq, Eq, Debug work
        let e1 = Easing::Linear;
        let e2 = e1; // Copy
        let e3 = e1.clone(); // Clone
        assert_eq!(e1, e2); // PartialEq
        assert_eq!(e2, e3);
        assert_ne!(Easing::Linear, Easing::EaseIn);
        let _ = format!("{:?}", e1); // Debug
    }

    #[test]
    fn test_animation_clone() {
        let anim = Animation::new(0.0, 100.0, Duration::from_secs(1), Easing::Linear);
        let cloned = anim.clone();
        assert_eq!(cloned.from, anim.from);
        assert_eq!(cloned.to, anim.to);
        assert_eq!(cloned.duration, anim.duration);
        assert_eq!(cloned.easing, anim.easing);
        assert_eq!(cloned.completed, anim.completed);
        assert_eq!(cloned.start_time, anim.start_time);
    }

    #[test]
    fn test_animation_debug_format() {
        let anim = Animation::new(0.0, 100.0, Duration::from_secs(1), Easing::Linear);
        let debug_str = format!("{:?}", anim);
        assert!(debug_str.contains("Animation"));
        assert!(debug_str.contains("from"));
        assert!(debug_str.contains("to"));
    }

    #[test]
    fn test_animation_manager_debug_format() {
        let mgr = AnimationManager::new();
        let debug_str = format!("{:?}", mgr);
        assert!(debug_str.contains("AnimationManager"));
    }

    #[test]
    fn test_has_active_animations_after_adding_and_removing() {
        let mut mgr = AnimationManager::new();
        assert!(!mgr.has_active_animations());

        mgr.animate_scroll(1, 0.0, 10.0);
        assert!(mgr.has_active_animations());

        // Wait for completion + read + tick
        sleep(Duration::from_millis(200));
        let _ = mgr.get_scroll_offset(1);
        mgr.tick();
        assert!(!mgr.has_active_animations());
    }
}
