//! Shared transition policy config for crossfade/scroll animations.

use crate::scroll_animation::{ScrollEasing, ScrollEffect};
use std::time::Duration;

/// Animation policy for per-window transitions.
///
/// This is the authoritative transition config shared across crates; render
/// code consumes this policy instead of owning separate config fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionPolicy {
    /// Enable crossfade transitions on buffer/window changes.
    pub crossfade_enabled: bool,
    /// Crossfade duration in milliseconds.
    pub crossfade_duration_ms: u32,
    /// Crossfade visual effect.
    pub crossfade_effect: ScrollEffect,
    /// Crossfade timing function.
    pub crossfade_easing: ScrollEasing,

    /// Enable scroll-slide transitions.
    pub scroll_enabled: bool,
    /// Scroll-slide duration in milliseconds.
    pub scroll_duration_ms: u32,
    /// Scroll visual effect.
    pub scroll_effect: ScrollEffect,
    /// Scroll timing function.
    pub scroll_easing: ScrollEasing,
}

impl TransitionPolicy {
    /// Build policy from wire-level effect/easing indices used by C->Rust FFI.
    pub fn from_indices(
        crossfade_enabled: bool,
        crossfade_duration_ms: u32,
        crossfade_effect: u32,
        crossfade_easing: u32,
        scroll_enabled: bool,
        scroll_duration_ms: u32,
        scroll_effect: u32,
        scroll_easing: u32,
    ) -> Self {
        let cf_effect = ScrollEffect::ALL
            .get(crossfade_effect as usize)
            .copied()
            .unwrap_or(ScrollEffect::Crossfade);
        let sc_effect = ScrollEffect::ALL
            .get(scroll_effect as usize)
            .copied()
            .unwrap_or(ScrollEffect::Slide);

        Self {
            crossfade_enabled,
            crossfade_duration_ms: if crossfade_duration_ms > 0 {
                crossfade_duration_ms
            } else {
                200
            },
            crossfade_effect: cf_effect,
            crossfade_easing: easing_from_index(crossfade_easing),
            scroll_enabled,
            scroll_duration_ms: if scroll_duration_ms > 0 {
                scroll_duration_ms
            } else {
                150
            },
            scroll_effect: sc_effect,
            scroll_easing: easing_from_index(scroll_easing),
        }
    }

    /// True when at least one transition path needs offscreen snapshots.
    pub fn needs_offscreen(&self) -> bool {
        self.crossfade_enabled || self.scroll_enabled
    }

    /// Crossfade duration as `Duration`.
    pub fn crossfade_duration(&self) -> Duration {
        Duration::from_millis(self.crossfade_duration_ms as u64)
    }

    /// Scroll duration as `Duration`.
    pub fn scroll_duration(&self) -> Duration {
        Duration::from_millis(self.scroll_duration_ms as u64)
    }
}

impl Default for TransitionPolicy {
    fn default() -> Self {
        Self {
            crossfade_enabled: true,
            crossfade_duration_ms: 200,
            crossfade_effect: ScrollEffect::Crossfade,
            crossfade_easing: ScrollEasing::EaseOutQuad,
            scroll_enabled: true,
            scroll_duration_ms: 150,
            scroll_effect: ScrollEffect::default(),
            scroll_easing: ScrollEasing::default(),
        }
    }
}

fn easing_from_index(idx: u32) -> ScrollEasing {
    match idx {
        0 => ScrollEasing::EaseOutQuad,
        1 => ScrollEasing::EaseOutCubic,
        2 => ScrollEasing::Spring,
        3 => ScrollEasing::Linear,
        4 => ScrollEasing::EaseInOutCubic,
        _ => ScrollEasing::EaseOutQuad,
    }
}
