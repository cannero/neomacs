//! Buffer transition effects for smooth content changes.

use std::sync::Arc;
use std::time::{Duration, Instant};

/// Type of transition effect between buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionType {
    /// Page flip to the left (like turning a book page left).
    PageFlipLeft,
    /// Page flip to the right (like turning a book page right).
    PageFlipRight,
    /// Crossfade between old and new content.
    Fade,
    /// Slide new content in from the right.
    SlideLeft,
    /// Slide new content in from the left.
    SlideRight,
}

/// A transition between two buffer states.
#[derive(Debug)]
pub struct BufferTransition {
    /// The texture being transitioned from.
    pub from_texture: Arc<wgpu::Texture>,
    /// The texture being transitioned to.
    pub to_texture: Arc<wgpu::Texture>,
    /// The type of transition effect.
    pub transition_type: TransitionType,
    /// Duration of the transition.
    pub duration: Duration,
    /// When the transition started.
    pub started: Instant,
}

impl BufferTransition {
    /// Create a new buffer transition.
    pub fn new(
        from_texture: Arc<wgpu::Texture>,
        to_texture: Arc<wgpu::Texture>,
        transition_type: TransitionType,
        duration: Duration,
    ) -> Self {
        Self {
            from_texture,
            to_texture,
            transition_type,
            duration,
            started: Instant::now(),
        }
    }

    /// Get the current progress of the transition (0.0 to 1.0).
    pub fn progress(&self) -> f32 {
        let elapsed = self.started.elapsed();
        if self.duration.as_secs_f32() > 0.0 {
            (elapsed.as_secs_f32() / self.duration.as_secs_f32()).min(1.0)
        } else {
            1.0
        }
    }

    /// Check if the transition has completed.
    pub fn is_complete(&self) -> bool {
        self.started.elapsed() >= self.duration
    }

    /// Get the rotation angles for page flip transitions in degrees.
    ///
    /// Returns `(old_angle, new_angle)` where:
    /// - `old_angle`: rotation of the outgoing page (starts at 0, ends at -90 or 90)
    /// - `new_angle`: rotation of the incoming page (starts at 90 or -90, ends at 0)
    ///
    /// For `PageFlipLeft`, old page rotates to +90 (away from viewer on left)
    /// and new page rotates from -90 to 0 (revealing from right).
    ///
    /// For `PageFlipRight`, old page rotates to -90 (away from viewer on right)
    /// and new page rotates from +90 to 0 (revealing from left).
    pub fn page_flip_angles(&self) -> (f32, f32) {
        let progress = self.progress();

        match self.transition_type {
            TransitionType::PageFlipLeft => {
                // Old page: 0 -> 90 degrees (rotates away to the left)
                // New page: -90 -> 0 degrees (rotates in from the right)
                let old_angle = progress * 90.0;
                let new_angle = -90.0 + progress * 90.0;
                (old_angle, new_angle)
            }
            TransitionType::PageFlipRight => {
                // Old page: 0 -> -90 degrees (rotates away to the right)
                // New page: 90 -> 0 degrees (rotates in from the left)
                let old_angle = -progress * 90.0;
                let new_angle = 90.0 - progress * 90.0;
                (old_angle, new_angle)
            }
            _ => (0.0, 0.0),
        }
    }

    /// Get the opacity values for fade transitions.
    ///
    /// Returns `(old_opacity, new_opacity)` where both range from 0.0 to 1.0.
    pub fn fade_opacity(&self) -> (f32, f32) {
        let progress = self.progress();

        match self.transition_type {
            TransitionType::Fade => {
                // Old fades out, new fades in
                let old_opacity = 1.0 - progress;
                let new_opacity = progress;
                (old_opacity, new_opacity)
            }
            _ => (1.0, 1.0),
        }
    }

    /// Get the slide offsets for slide transitions.
    ///
    /// Returns `(old_offset, new_offset)` as fractions of the screen width (-1.0 to 1.0).
    /// - Negative offset means content is to the left of its normal position
    /// - Positive offset means content is to the right of its normal position
    pub fn slide_offset(&self) -> (f32, f32) {
        let progress = self.progress();

        match self.transition_type {
            TransitionType::SlideLeft => {
                // Old slides out to the left (-1.0), new slides in from the right
                // Old: 0 -> -1.0
                // New: 1.0 -> 0
                let old_offset = -progress;
                let new_offset = 1.0 - progress;
                (old_offset, new_offset)
            }
            TransitionType::SlideRight => {
                // Old slides out to the right (+1.0), new slides in from the left
                // Old: 0 -> 1.0
                // New: -1.0 -> 0
                let old_offset = progress;
                let new_offset = -1.0 + progress;
                (old_offset, new_offset)
            }
            _ => (0.0, 0.0),
        }
    }
}

/// Manager for buffer transitions.
#[derive(Debug, Default)]
pub struct TransitionManager {
    /// The currently active transition, if any.
    active: Option<BufferTransition>,
}

impl TransitionManager {
    /// Create a new transition manager.
    pub fn new() -> Self {
        Self { active: None }
    }

    /// Start a new transition.
    ///
    /// This will replace any currently active transition.
    pub fn start(
        &mut self,
        from_texture: Arc<wgpu::Texture>,
        to_texture: Arc<wgpu::Texture>,
        transition_type: TransitionType,
        duration: Duration,
    ) {
        self.active = Some(BufferTransition::new(
            from_texture,
            to_texture,
            transition_type,
            duration,
        ));
    }

    /// Get the currently active transition, if any.
    pub fn active(&self) -> Option<&BufferTransition> {
        self.active.as_ref()
    }

    /// Tick the transition manager, cleaning up completed transitions.
    ///
    /// Returns `true` if there was an active transition that completed.
    pub fn tick(&mut self) -> bool {
        if let Some(ref transition) = self.active {
            if transition.is_complete() {
                self.active = None;
                return true;
            }
        }
        false
    }

    /// Check if there is an active transition.
    pub fn has_transition(&self) -> bool {
        self.active.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::warn;

    // ---------------------------------------------------------------
    // Helper: create a wgpu device + dummy texture pair for testing.
    // Returns None if no GPU adapter is available (headless CI, etc.).
    // ---------------------------------------------------------------
    fn make_test_textures() -> Option<(Arc<wgpu::Texture>, Arc<wgpu::Texture>)> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        })).ok()?;
        let (device, _queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("test device"),
                ..Default::default()
            },
        ))
        .ok()?;

        let desc = wgpu::TextureDescriptor {
            label: Some("test texture"),
            size: wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        };
        let t1 = Arc::new(device.create_texture(&desc));
        let t2 = Arc::new(device.create_texture(&desc));
        Some((t1, t2))
    }

    /// Build a `BufferTransition` with a controlled `started` time so that
    /// `progress()` returns a deterministic value.
    ///
    /// `elapsed` is how much time has "already passed" since the transition
    /// started (i.e., `started = Instant::now() - elapsed`).
    fn make_transition(
        from: Arc<wgpu::Texture>,
        to: Arc<wgpu::Texture>,
        tt: TransitionType,
        duration: Duration,
        elapsed: Duration,
    ) -> BufferTransition {
        BufferTransition {
            from_texture: from,
            to_texture: to,
            transition_type: tt,
            duration,
            started: Instant::now() - elapsed,
        }
    }

    // ---------------------------------------------------------------
    // TransitionType enum tests
    // ---------------------------------------------------------------

    #[test]
    fn test_transition_type_variants() {
        let types = vec![
            TransitionType::PageFlipLeft,
            TransitionType::PageFlipRight,
            TransitionType::Fade,
            TransitionType::SlideLeft,
            TransitionType::SlideRight,
        ];
        for (i, t1) in types.iter().enumerate() {
            for (j, t2) in types.iter().enumerate() {
                if i == j {
                    assert_eq!(t1, t2);
                } else {
                    assert_ne!(t1, t2);
                }
            }
        }
    }

    #[test]
    fn test_transition_type_clone() {
        let original = TransitionType::Fade;
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    #[test]
    fn test_transition_type_copy() {
        let original = TransitionType::SlideLeft;
        let copied = original; // Copy
        // Both should still be usable (Copy trait)
        assert_eq!(original, copied);
    }

    #[test]
    fn test_transition_type_debug() {
        let formatted = format!("{:?}", TransitionType::PageFlipLeft);
        assert_eq!(formatted, "PageFlipLeft");

        let formatted = format!("{:?}", TransitionType::PageFlipRight);
        assert_eq!(formatted, "PageFlipRight");

        let formatted = format!("{:?}", TransitionType::Fade);
        assert_eq!(formatted, "Fade");

        let formatted = format!("{:?}", TransitionType::SlideLeft);
        assert_eq!(formatted, "SlideLeft");

        let formatted = format!("{:?}", TransitionType::SlideRight);
        assert_eq!(formatted, "SlideRight");
    }

    // ---------------------------------------------------------------
    // TransitionManager tests (no GPU needed for empty-state tests)
    // ---------------------------------------------------------------

    #[test]
    fn test_transition_manager_default() {
        let manager = TransitionManager::default();
        assert!(!manager.has_transition());
        assert!(manager.active().is_none());
    }

    #[test]
    fn test_transition_manager_new() {
        let manager = TransitionManager::new();
        assert!(!manager.has_transition());
        assert!(manager.active().is_none());
    }

    #[test]
    fn test_transition_manager_tick_empty() {
        let mut manager = TransitionManager::new();
        // tick on empty manager should return false
        assert!(!manager.tick());
        assert!(!manager.has_transition());
    }

    #[test]
    fn test_transition_manager_tick_empty_repeated() {
        let mut manager = TransitionManager::new();
        // Multiple ticks on empty manager always return false
        for _ in 0..10 {
            assert!(!manager.tick());
        }
    }

    #[test]
    fn test_transition_manager_debug() {
        let manager = TransitionManager::new();
        let debug_str = format!("{:?}", manager);
        assert!(debug_str.contains("TransitionManager"));
    }

    // ---------------------------------------------------------------
    // TransitionManager tests (require GPU for start/active/tick)
    // ---------------------------------------------------------------

    #[test]
    fn test_transition_manager_start_creates_active() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let mut manager = TransitionManager::new();
        manager.start(t1, t2, TransitionType::Fade, Duration::from_secs(10));
        assert!(manager.has_transition());
        assert!(manager.active().is_some());
    }

    #[test]
    fn test_transition_manager_start_replaces_existing() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let mut manager = TransitionManager::new();
        manager.start(
            t1.clone(),
            t2.clone(),
            TransitionType::Fade,
            Duration::from_secs(10),
        );
        assert!(manager.has_transition());

        // Start a new transition -- it should replace the old one
        manager.start(t1, t2, TransitionType::SlideLeft, Duration::from_secs(5));
        assert!(manager.has_transition());
        let active = manager.active().unwrap();
        assert_eq!(active.transition_type, TransitionType::SlideLeft);
    }

    #[test]
    fn test_transition_manager_tick_incomplete() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let mut manager = TransitionManager::new();
        manager.start(t1, t2, TransitionType::Fade, Duration::from_secs(60));
        // Duration is 60s -- transition cannot be complete yet
        assert!(!manager.tick());
        assert!(manager.has_transition());
    }

    #[test]
    fn test_transition_manager_tick_completed() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let mut manager = TransitionManager::new();
        // Use a zero-duration transition so it completes immediately
        manager.start(t1, t2, TransitionType::Fade, Duration::ZERO);
        // Should be complete now
        assert!(manager.tick());
        // After tick cleans it up, no more active transition
        assert!(!manager.has_transition());
        assert!(manager.active().is_none());
    }

    #[test]
    fn test_transition_manager_tick_after_completion_returns_false() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let mut manager = TransitionManager::new();
        manager.start(t1, t2, TransitionType::Fade, Duration::ZERO);
        assert!(manager.tick()); // completes and cleans up
        // Second tick: no active transition, returns false
        assert!(!manager.tick());
    }

    #[test]
    fn test_transition_manager_active_returns_correct_type() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let mut manager = TransitionManager::new();
        manager.start(t1, t2, TransitionType::PageFlipRight, Duration::from_secs(1));
        let active = manager.active().unwrap();
        assert_eq!(active.transition_type, TransitionType::PageFlipRight);
        assert_eq!(active.duration, Duration::from_secs(1));
    }

    // ---------------------------------------------------------------
    // BufferTransition::new tests
    // ---------------------------------------------------------------

    #[test]
    fn test_buffer_transition_new_sets_fields() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let before = Instant::now();
        let bt = BufferTransition::new(
            t1,
            t2,
            TransitionType::SlideRight,
            Duration::from_millis(300),
        );
        let after = Instant::now();

        assert_eq!(bt.transition_type, TransitionType::SlideRight);
        assert_eq!(bt.duration, Duration::from_millis(300));
        // started should be between before and after
        assert!(bt.started >= before);
        assert!(bt.started <= after);
    }

    // ---------------------------------------------------------------
    // BufferTransition::progress tests
    // ---------------------------------------------------------------

    #[test]
    fn test_progress_zero_elapsed() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::Fade,
            Duration::from_secs(10),
            Duration::ZERO,
        );
        let p = bt.progress();
        // Just started, progress should be very close to 0.0
        assert!(p >= 0.0 && p < 0.01, "progress was {}", p);
    }

    #[test]
    fn test_progress_halfway() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::Fade,
            Duration::from_secs(10),
            Duration::from_secs(5),
        );
        let p = bt.progress();
        // Should be approximately 0.5
        assert!(
            (p - 0.5).abs() < 0.05,
            "expected ~0.5, got {}",
            p
        );
    }

    #[test]
    fn test_progress_complete() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::Fade,
            Duration::from_secs(1),
            Duration::from_secs(2), // elapsed > duration
        );
        let p = bt.progress();
        // Should be clamped to 1.0
        assert_eq!(p, 1.0);
    }

    #[test]
    fn test_progress_clamped_at_one() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::Fade,
            Duration::from_millis(100),
            Duration::from_secs(100), // way past duration
        );
        assert_eq!(bt.progress(), 1.0);
    }

    #[test]
    fn test_progress_zero_duration_returns_one() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::Fade,
            Duration::ZERO,
            Duration::ZERO,
        );
        // Zero duration should immediately return 1.0
        assert_eq!(bt.progress(), 1.0);
    }

    #[test]
    fn test_progress_monotonically_increases() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let duration = Duration::from_secs(10);
        let mut last_progress = 0.0f32;
        for elapsed_ms in (0..=10_000).step_by(1000) {
            let bt = make_transition(
                t1.clone(),
                t2.clone(),
                TransitionType::Fade,
                duration,
                Duration::from_millis(elapsed_ms),
            );
            let p = bt.progress();
            assert!(
                p >= last_progress,
                "progress went backwards: {} -> {} at elapsed={}ms",
                last_progress,
                p,
                elapsed_ms,
            );
            last_progress = p;
        }
    }

    // ---------------------------------------------------------------
    // BufferTransition::is_complete tests
    // ---------------------------------------------------------------

    #[test]
    fn test_is_complete_false_when_not_elapsed() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::Fade,
            Duration::from_secs(60),
            Duration::ZERO,
        );
        assert!(!bt.is_complete());
    }

    #[test]
    fn test_is_complete_true_when_elapsed_exceeds_duration() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::Fade,
            Duration::from_millis(100),
            Duration::from_secs(1), // well past 100ms
        );
        assert!(bt.is_complete());
    }

    #[test]
    fn test_is_complete_true_for_zero_duration() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::Fade,
            Duration::ZERO,
            Duration::ZERO,
        );
        assert!(bt.is_complete());
    }

    // ---------------------------------------------------------------
    // page_flip_angles tests
    // ---------------------------------------------------------------

    #[test]
    fn test_page_flip_left_at_start() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        // progress ~0
        let bt = make_transition(
            t1,
            t2,
            TransitionType::PageFlipLeft,
            Duration::from_secs(10),
            Duration::ZERO,
        );
        let (old_angle, new_angle) = bt.page_flip_angles();
        // At start: old ~0, new ~-90
        assert!(old_angle.abs() < 1.0, "old_angle at start: {}", old_angle);
        assert!(
            (new_angle + 90.0).abs() < 1.0,
            "new_angle at start: {}",
            new_angle
        );
    }

    #[test]
    fn test_page_flip_left_at_end() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        // progress = 1.0
        let bt = make_transition(
            t1,
            t2,
            TransitionType::PageFlipLeft,
            Duration::from_secs(1),
            Duration::from_secs(2),
        );
        let (old_angle, new_angle) = bt.page_flip_angles();
        // At end: old = 90, new = 0
        assert!(
            (old_angle - 90.0).abs() < 0.01,
            "old_angle at end: {}",
            old_angle
        );
        assert!(
            new_angle.abs() < 0.01,
            "new_angle at end: {}",
            new_angle
        );
    }

    #[test]
    fn test_page_flip_left_at_midpoint() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        // progress ~0.5
        let bt = make_transition(
            t1,
            t2,
            TransitionType::PageFlipLeft,
            Duration::from_secs(10),
            Duration::from_secs(5),
        );
        let (old_angle, new_angle) = bt.page_flip_angles();
        // At midpoint: old ~45, new ~-45
        assert!(
            (old_angle - 45.0).abs() < 2.0,
            "old_angle at mid: {}",
            old_angle
        );
        assert!(
            (new_angle + 45.0).abs() < 2.0,
            "new_angle at mid: {}",
            new_angle
        );
    }

    #[test]
    fn test_page_flip_right_at_start() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::PageFlipRight,
            Duration::from_secs(10),
            Duration::ZERO,
        );
        let (old_angle, new_angle) = bt.page_flip_angles();
        // At start: old ~0, new ~90
        assert!(old_angle.abs() < 1.0, "old_angle at start: {}", old_angle);
        assert!(
            (new_angle - 90.0).abs() < 1.0,
            "new_angle at start: {}",
            new_angle
        );
    }

    #[test]
    fn test_page_flip_right_at_end() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::PageFlipRight,
            Duration::from_secs(1),
            Duration::from_secs(2),
        );
        let (old_angle, new_angle) = bt.page_flip_angles();
        // At end: old = -90, new = 0
        assert!(
            (old_angle + 90.0).abs() < 0.01,
            "old_angle at end: {}",
            old_angle
        );
        assert!(
            new_angle.abs() < 0.01,
            "new_angle at end: {}",
            new_angle
        );
    }

    #[test]
    fn test_page_flip_right_at_midpoint() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::PageFlipRight,
            Duration::from_secs(10),
            Duration::from_secs(5),
        );
        let (old_angle, new_angle) = bt.page_flip_angles();
        // At midpoint: old ~-45, new ~45
        assert!(
            (old_angle + 45.0).abs() < 2.0,
            "old_angle at mid: {}",
            old_angle
        );
        assert!(
            (new_angle - 45.0).abs() < 2.0,
            "new_angle at mid: {}",
            new_angle
        );
    }

    #[test]
    fn test_page_flip_angles_returns_zeros_for_non_flip_types() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        for tt in [
            TransitionType::Fade,
            TransitionType::SlideLeft,
            TransitionType::SlideRight,
        ] {
            let bt = make_transition(
                t1.clone(),
                t2.clone(),
                tt,
                Duration::from_secs(1),
                Duration::from_millis(500),
            );
            let (old_angle, new_angle) = bt.page_flip_angles();
            assert_eq!(old_angle, 0.0, "non-flip type {:?} old_angle", tt);
            assert_eq!(new_angle, 0.0, "non-flip type {:?} new_angle", tt);
        }
    }

    // ---------------------------------------------------------------
    // fade_opacity tests
    // ---------------------------------------------------------------

    #[test]
    fn test_fade_opacity_at_start() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::Fade,
            Duration::from_secs(10),
            Duration::ZERO,
        );
        let (old_op, new_op) = bt.fade_opacity();
        // At start: old ~1.0, new ~0.0
        assert!(
            (old_op - 1.0).abs() < 0.01,
            "old_opacity at start: {}",
            old_op
        );
        assert!(
            new_op.abs() < 0.01,
            "new_opacity at start: {}",
            new_op
        );
    }

    #[test]
    fn test_fade_opacity_at_end() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::Fade,
            Duration::from_secs(1),
            Duration::from_secs(2),
        );
        let (old_op, new_op) = bt.fade_opacity();
        // At end: old = 0.0, new = 1.0
        assert!(
            old_op.abs() < 0.01,
            "old_opacity at end: {}",
            old_op
        );
        assert!(
            (new_op - 1.0).abs() < 0.01,
            "new_opacity at end: {}",
            new_op
        );
    }

    #[test]
    fn test_fade_opacity_at_midpoint() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::Fade,
            Duration::from_secs(10),
            Duration::from_secs(5),
        );
        let (old_op, new_op) = bt.fade_opacity();
        // At midpoint: both ~0.5
        assert!(
            (old_op - 0.5).abs() < 0.05,
            "old_opacity at mid: {}",
            old_op
        );
        assert!(
            (new_op - 0.5).abs() < 0.05,
            "new_opacity at mid: {}",
            new_op
        );
    }

    #[test]
    fn test_fade_opacity_sum_is_one() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        // At any progress point, old_opacity + new_opacity = 1.0
        for elapsed_ms in (0..=10_000).step_by(500) {
            let bt = make_transition(
                t1.clone(),
                t2.clone(),
                TransitionType::Fade,
                Duration::from_secs(10),
                Duration::from_millis(elapsed_ms),
            );
            let (old_op, new_op) = bt.fade_opacity();
            let sum = old_op + new_op;
            assert!(
                (sum - 1.0).abs() < 0.01,
                "opacity sum={} at elapsed={}ms",
                sum,
                elapsed_ms
            );
        }
    }

    #[test]
    fn test_fade_opacity_returns_ones_for_non_fade_types() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        for tt in [
            TransitionType::PageFlipLeft,
            TransitionType::PageFlipRight,
            TransitionType::SlideLeft,
            TransitionType::SlideRight,
        ] {
            let bt = make_transition(
                t1.clone(),
                t2.clone(),
                tt,
                Duration::from_secs(1),
                Duration::from_millis(500),
            );
            let (old_op, new_op) = bt.fade_opacity();
            assert_eq!(old_op, 1.0, "non-fade type {:?} old_opacity", tt);
            assert_eq!(new_op, 1.0, "non-fade type {:?} new_opacity", tt);
        }
    }

    // ---------------------------------------------------------------
    // slide_offset tests
    // ---------------------------------------------------------------

    #[test]
    fn test_slide_left_at_start() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::SlideLeft,
            Duration::from_secs(10),
            Duration::ZERO,
        );
        let (old_off, new_off) = bt.slide_offset();
        // At start: old ~0.0, new ~1.0
        assert!(old_off.abs() < 0.01, "old_offset at start: {}", old_off);
        assert!(
            (new_off - 1.0).abs() < 0.01,
            "new_offset at start: {}",
            new_off
        );
    }

    #[test]
    fn test_slide_left_at_end() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::SlideLeft,
            Duration::from_secs(1),
            Duration::from_secs(2),
        );
        let (old_off, new_off) = bt.slide_offset();
        // At end: old = -1.0, new = 0.0
        assert!(
            (old_off + 1.0).abs() < 0.01,
            "old_offset at end: {}",
            old_off
        );
        assert!(
            new_off.abs() < 0.01,
            "new_offset at end: {}",
            new_off
        );
    }

    #[test]
    fn test_slide_left_at_midpoint() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::SlideLeft,
            Duration::from_secs(10),
            Duration::from_secs(5),
        );
        let (old_off, new_off) = bt.slide_offset();
        // At midpoint: old ~-0.5, new ~0.5
        assert!(
            (old_off + 0.5).abs() < 0.05,
            "old_offset at mid: {}",
            old_off
        );
        assert!(
            (new_off - 0.5).abs() < 0.05,
            "new_offset at mid: {}",
            new_off
        );
    }

    #[test]
    fn test_slide_right_at_start() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::SlideRight,
            Duration::from_secs(10),
            Duration::ZERO,
        );
        let (old_off, new_off) = bt.slide_offset();
        // At start: old ~0.0, new ~-1.0
        assert!(old_off.abs() < 0.01, "old_offset at start: {}", old_off);
        assert!(
            (new_off + 1.0).abs() < 0.01,
            "new_offset at start: {}",
            new_off
        );
    }

    #[test]
    fn test_slide_right_at_end() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::SlideRight,
            Duration::from_secs(1),
            Duration::from_secs(2),
        );
        let (old_off, new_off) = bt.slide_offset();
        // At end: old = 1.0, new = 0.0
        assert!(
            (old_off - 1.0).abs() < 0.01,
            "old_offset at end: {}",
            old_off
        );
        assert!(
            new_off.abs() < 0.01,
            "new_offset at end: {}",
            new_off
        );
    }

    #[test]
    fn test_slide_right_at_midpoint() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::SlideRight,
            Duration::from_secs(10),
            Duration::from_secs(5),
        );
        let (old_off, new_off) = bt.slide_offset();
        // At midpoint: old ~0.5, new ~-0.5
        assert!(
            (old_off - 0.5).abs() < 0.05,
            "old_offset at mid: {}",
            old_off
        );
        assert!(
            (new_off + 0.5).abs() < 0.05,
            "new_offset at mid: {}",
            new_off
        );
    }

    #[test]
    fn test_slide_offset_returns_zeros_for_non_slide_types() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        for tt in [
            TransitionType::PageFlipLeft,
            TransitionType::PageFlipRight,
            TransitionType::Fade,
        ] {
            let bt = make_transition(
                t1.clone(),
                t2.clone(),
                tt,
                Duration::from_secs(1),
                Duration::from_millis(500),
            );
            let (old_off, new_off) = bt.slide_offset();
            assert_eq!(old_off, 0.0, "non-slide type {:?} old_offset", tt);
            assert_eq!(new_off, 0.0, "non-slide type {:?} new_offset", tt);
        }
    }

    #[test]
    fn test_slide_left_offsets_sum_is_one() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        // For SlideLeft: old_offset + new_offset = -progress + (1 - progress) = 1 - 2*progress
        // Actually let us check the gap: new_offset - old_offset = (1-p) - (-p) = 1
        for elapsed_ms in (0..=10_000).step_by(500) {
            let bt = make_transition(
                t1.clone(),
                t2.clone(),
                TransitionType::SlideLeft,
                Duration::from_secs(10),
                Duration::from_millis(elapsed_ms),
            );
            let (old_off, new_off) = bt.slide_offset();
            let gap = new_off - old_off;
            assert!(
                (gap - 1.0).abs() < 0.01,
                "slide_left gap={} at elapsed={}ms (old={}, new={})",
                gap,
                elapsed_ms,
                old_off,
                new_off,
            );
        }
    }

    #[test]
    fn test_slide_right_offsets_gap_is_minus_one() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        // For SlideRight: new_offset - old_offset = (-1+p) - p = -1
        for elapsed_ms in (0..=10_000).step_by(500) {
            let bt = make_transition(
                t1.clone(),
                t2.clone(),
                TransitionType::SlideRight,
                Duration::from_secs(10),
                Duration::from_millis(elapsed_ms),
            );
            let (old_off, new_off) = bt.slide_offset();
            let gap = new_off - old_off;
            assert!(
                (gap + 1.0).abs() < 0.01,
                "slide_right gap={} at elapsed={}ms (old={}, new={})",
                gap,
                elapsed_ms,
                old_off,
                new_off,
            );
        }
    }

    // ---------------------------------------------------------------
    // Cross-cutting: boundary / edge-case tests
    // ---------------------------------------------------------------

    #[test]
    fn test_very_short_duration_completes_quickly() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::Fade,
            Duration::from_nanos(1),
            Duration::from_millis(1),
        );
        assert!(bt.is_complete());
        assert_eq!(bt.progress(), 1.0);
    }

    #[test]
    fn test_very_long_duration_stays_incomplete() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = make_transition(
            t1,
            t2,
            TransitionType::Fade,
            Duration::from_secs(86400), // 1 day
            Duration::ZERO,
        );
        assert!(!bt.is_complete());
        let p = bt.progress();
        assert!(p < 0.001, "progress for 1-day duration should be tiny, got {}", p);
    }

    #[test]
    fn test_all_transition_types_with_zero_duration() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        for tt in [
            TransitionType::PageFlipLeft,
            TransitionType::PageFlipRight,
            TransitionType::Fade,
            TransitionType::SlideLeft,
            TransitionType::SlideRight,
        ] {
            let bt = make_transition(
                t1.clone(),
                t2.clone(),
                tt,
                Duration::ZERO,
                Duration::ZERO,
            );
            assert_eq!(bt.progress(), 1.0, "{:?} with zero duration", tt);
            assert!(bt.is_complete(), "{:?} with zero duration", tt);
        }
    }

    #[test]
    fn test_page_flip_left_angles_range() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        // Over the whole transition, old_angle should be in [0, 90]
        // and new_angle should be in [-90, 0]
        for elapsed_ms in (0..=10_000).step_by(200) {
            let bt = make_transition(
                t1.clone(),
                t2.clone(),
                TransitionType::PageFlipLeft,
                Duration::from_secs(10),
                Duration::from_millis(elapsed_ms),
            );
            let (old_angle, new_angle) = bt.page_flip_angles();
            assert!(
                old_angle >= -0.01 && old_angle <= 90.01,
                "old_angle out of range: {} at {}ms",
                old_angle,
                elapsed_ms
            );
            assert!(
                new_angle >= -90.01 && new_angle <= 0.01,
                "new_angle out of range: {} at {}ms",
                new_angle,
                elapsed_ms
            );
        }
    }

    #[test]
    fn test_page_flip_right_angles_range() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        // old_angle in [-90, 0], new_angle in [0, 90]
        for elapsed_ms in (0..=10_000).step_by(200) {
            let bt = make_transition(
                t1.clone(),
                t2.clone(),
                TransitionType::PageFlipRight,
                Duration::from_secs(10),
                Duration::from_millis(elapsed_ms),
            );
            let (old_angle, new_angle) = bt.page_flip_angles();
            assert!(
                old_angle >= -90.01 && old_angle <= 0.01,
                "old_angle out of range: {} at {}ms",
                old_angle,
                elapsed_ms
            );
            assert!(
                new_angle >= -0.01 && new_angle <= 90.01,
                "new_angle out of range: {} at {}ms",
                new_angle,
                elapsed_ms
            );
        }
    }

    #[test]
    fn test_fade_opacity_range() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        for elapsed_ms in (0..=10_000).step_by(200) {
            let bt = make_transition(
                t1.clone(),
                t2.clone(),
                TransitionType::Fade,
                Duration::from_secs(10),
                Duration::from_millis(elapsed_ms),
            );
            let (old_op, new_op) = bt.fade_opacity();
            assert!(
                old_op >= -0.01 && old_op <= 1.01,
                "old_opacity out of range: {} at {}ms",
                old_op,
                elapsed_ms
            );
            assert!(
                new_op >= -0.01 && new_op <= 1.01,
                "new_opacity out of range: {} at {}ms",
                new_op,
                elapsed_ms
            );
        }
    }

    #[test]
    fn test_slide_left_offset_range() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        for elapsed_ms in (0..=10_000).step_by(200) {
            let bt = make_transition(
                t1.clone(),
                t2.clone(),
                TransitionType::SlideLeft,
                Duration::from_secs(10),
                Duration::from_millis(elapsed_ms),
            );
            let (old_off, new_off) = bt.slide_offset();
            // old: 0 -> -1, new: 1 -> 0
            assert!(
                old_off >= -1.01 && old_off <= 0.01,
                "old_offset out of range: {} at {}ms",
                old_off,
                elapsed_ms
            );
            assert!(
                new_off >= -0.01 && new_off <= 1.01,
                "new_offset out of range: {} at {}ms",
                new_off,
                elapsed_ms
            );
        }
    }

    #[test]
    fn test_slide_right_offset_range() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        for elapsed_ms in (0..=10_000).step_by(200) {
            let bt = make_transition(
                t1.clone(),
                t2.clone(),
                TransitionType::SlideRight,
                Duration::from_secs(10),
                Duration::from_millis(elapsed_ms),
            );
            let (old_off, new_off) = bt.slide_offset();
            // old: 0 -> 1, new: -1 -> 0
            assert!(
                old_off >= -0.01 && old_off <= 1.01,
                "old_offset out of range: {} at {}ms",
                old_off,
                elapsed_ms
            );
            assert!(
                new_off >= -1.01 && new_off <= 0.01,
                "new_offset out of range: {} at {}ms",
                new_off,
                elapsed_ms
            );
        }
    }

    // ---------------------------------------------------------------
    // TransitionManager lifecycle integration
    // ---------------------------------------------------------------

    #[test]
    fn test_manager_full_lifecycle() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let mut manager = TransitionManager::new();

        // Initially empty
        assert!(!manager.has_transition());
        assert!(!manager.tick());

        // Start a transition with zero duration (completes immediately)
        manager.start(
            t1.clone(),
            t2.clone(),
            TransitionType::Fade,
            Duration::ZERO,
        );
        assert!(manager.has_transition());

        // Tick should complete it
        assert!(manager.tick());
        assert!(!manager.has_transition());

        // Start a long transition
        manager.start(
            t1.clone(),
            t2.clone(),
            TransitionType::SlideLeft,
            Duration::from_secs(60),
        );
        assert!(manager.has_transition());
        assert!(!manager.tick()); // Not complete yet

        // Replace with another zero-duration transition
        manager.start(t1, t2, TransitionType::PageFlipLeft, Duration::ZERO);
        assert!(manager.has_transition());
        let active = manager.active().unwrap();
        assert_eq!(active.transition_type, TransitionType::PageFlipLeft);

        // Tick completes it
        assert!(manager.tick());
        assert!(!manager.has_transition());
    }

    #[test]
    fn test_buffer_transition_debug_impl() {
        let Some((t1, t2)) = make_test_textures() else {
            warn!("Skipping: no GPU adapter available");
            return;
        };
        let bt = BufferTransition::new(
            t1,
            t2,
            TransitionType::Fade,
            Duration::from_millis(200),
        );
        let debug_str = format!("{:?}", bt);
        assert!(debug_str.contains("BufferTransition"));
        assert!(debug_str.contains("Fade"));
    }
}
