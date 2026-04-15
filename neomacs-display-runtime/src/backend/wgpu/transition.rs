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
#[path = "transition_test.rs"]
mod tests;
