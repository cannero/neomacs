//! Window transition state (crossfade and scroll animations).

use super::RenderApp;
use crate::core::frame_glyphs::{WindowEffectHint, WindowTransitionHint, WindowTransitionKind};
use crate::core::types::Rect;
use neomacs_display_protocol::{ScrollEasing, ScrollEffect, TransitionPolicy};
use std::collections::HashMap;

/// State for an active crossfade transition
pub(super) struct CrossfadeTransition {
    pub(super) started: std::time::Instant,
    pub(super) duration: std::time::Duration,
    pub(super) bounds: Rect,
    pub(super) effect: ScrollEffect,
    pub(super) easing: ScrollEasing,
    pub(super) old_texture: wgpu::Texture,
    pub(super) old_view: wgpu::TextureView,
    pub(super) old_bind_group: wgpu::BindGroup,
}

/// State for an active scroll slide transition
pub(super) struct ScrollTransition {
    pub(super) started: std::time::Instant,
    pub(super) duration: std::time::Duration,
    pub(super) bounds: Rect,
    pub(super) direction: i32, // +1 = scroll down (content up), -1 = scroll up
    /// Pixel distance to slide (clamped to bounds.height).
    /// For a 1-line scroll this equals char_height, not the full window.
    pub(super) scroll_distance: f32,
    pub(super) effect: ScrollEffect,
    pub(super) easing: ScrollEasing,
    pub(super) old_texture: wgpu::Texture,
    pub(super) old_view: wgpu::TextureView,
    pub(super) old_bind_group: wgpu::BindGroup,
}

/// Window transition state (crossfade and scroll animations).
///
/// Groups configuration, double-buffer textures, and active transition maps.
pub(super) struct TransitionState {
    // Configuration
    pub(super) policy: TransitionPolicy,

    // Double-buffer offscreen textures
    pub(super) offscreen_a: Option<(wgpu::Texture, wgpu::TextureView, wgpu::BindGroup)>,
    pub(super) offscreen_b: Option<(wgpu::Texture, wgpu::TextureView, wgpu::BindGroup)>,
    pub(super) current_is_a: bool,

    // Active transitions
    pub(super) crossfades: HashMap<i64, CrossfadeTransition>,
    pub(super) scroll_slides: HashMap<i64, ScrollTransition>,
}

impl Default for TransitionState {
    fn default() -> Self {
        Self {
            policy: TransitionPolicy::default(),
            offscreen_a: None,
            offscreen_b: None,
            current_is_a: true,
            crossfades: HashMap::new(),
            scroll_slides: HashMap::new(),
        }
    }
}

impl TransitionState {
    /// Check if any transitions are currently active
    pub(super) fn has_active(&self) -> bool {
        !self.crossfades.is_empty() || !self.scroll_slides.is_empty()
    }
}

impl RenderApp {
    /// Ensure offscreen textures exist (lazily created)
    pub(super) fn ensure_offscreen_textures(&mut self) {
        if self.transitions.offscreen_a.is_some() && self.transitions.offscreen_b.is_some() {
            return;
        }
        let renderer = match self.renderer.as_ref() {
            Some(r) => r,
            None => return,
        };
        let w = self.width;
        let h = self.height;

        if self.transitions.offscreen_a.is_none() {
            let (tex, view) = renderer.create_offscreen_texture(w, h);
            let bg = renderer.create_texture_bind_group(&view);
            self.transitions.offscreen_a = Some((tex, view, bg));
        }
        if self.transitions.offscreen_b.is_none() {
            let (tex, view) = renderer.create_offscreen_texture(w, h);
            let bg = renderer.create_texture_bind_group(&view);
            self.transitions.offscreen_b = Some((tex, view, bg));
        }
    }

    /// Get the "current" offscreen texture view and bind group
    pub(super) fn current_offscreen_view_and_bg(
        &self,
    ) -> Option<(&wgpu::TextureView, &wgpu::BindGroup)> {
        let (_, view, bg) = if self.transitions.current_is_a {
            self.transitions.offscreen_a.as_ref()?
        } else {
            self.transitions.offscreen_b.as_ref()?
        };
        Some((view, bg))
    }

    /// Get the "previous" offscreen texture, view, and bind group
    pub(super) fn previous_offscreen(
        &self,
    ) -> Option<(&wgpu::Texture, &wgpu::TextureView, &wgpu::BindGroup)> {
        let (tex, view, bg) = if self.transitions.current_is_a {
            self.transitions.offscreen_b.as_ref()?
        } else {
            self.transitions.offscreen_a.as_ref()?
        };
        Some((tex, view, bg))
    }

    /// Snapshot the previous offscreen texture into a new dedicated texture
    pub(super) fn snapshot_prev_texture(
        &self,
    ) -> Option<(wgpu::Texture, wgpu::TextureView, wgpu::BindGroup)> {
        let renderer = self.renderer.as_ref()?;
        let (prev_tex, _, _) = self.previous_offscreen()?;

        let (snap, snap_view) = renderer.create_offscreen_texture(self.width, self.height);

        // GPU copy
        let mut encoder =
            renderer
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Snapshot Copy Encoder"),
                });
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: prev_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &snap,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        renderer.queue().submit(std::iter::once(encoder.finish()));

        let snap_bg = renderer.create_texture_bind_group(&snap_view);
        Some((snap, snap_view, snap_bg))
    }

    fn apply_transition_hint(&mut self, hint: &WindowTransitionHint, now: std::time::Instant) {
        match hint.kind {
            WindowTransitionKind::Crossfade => {
                if !self.transitions.policy.crossfade_enabled {
                    return;
                }

                self.transitions.crossfades.remove(&hint.window_id);
                self.transitions.scroll_slides.remove(&hint.window_id);

                if let Some((tex, view, bg)) = self.snapshot_prev_texture() {
                    let effect = hint
                        .effect
                        .unwrap_or(self.transitions.policy.crossfade_effect);
                    let easing = hint
                        .easing
                        .unwrap_or(self.transitions.policy.crossfade_easing);
                    tracing::debug!(
                        "Starting crossfade for window {} (effect={:?}, easing={:?})",
                        hint.window_id,
                        effect,
                        easing
                    );
                    self.transitions.crossfades.insert(
                        hint.window_id,
                        CrossfadeTransition {
                            started: now,
                            duration: self.transitions.policy.crossfade_duration(),
                            bounds: hint.bounds,
                            effect,
                            easing,
                            old_texture: tex,
                            old_view: view,
                            old_bind_group: bg,
                        },
                    );
                }
            }
            WindowTransitionKind::ScrollSlide {
                direction,
                scroll_distance,
            } => {
                if !self.transitions.policy.scroll_enabled {
                    return;
                }
                if hint.bounds.height < 50.0 {
                    return;
                }

                self.transitions.crossfades.remove(&hint.window_id);
                self.transitions.scroll_slides.remove(&hint.window_id);

                let dir = if direction >= 0 { 1 } else { -1 };
                let scroll_px = scroll_distance.max(0.0).min(hint.bounds.height);
                if let Some((tex, view, bg)) = self.snapshot_prev_texture() {
                    let effect = hint.effect.unwrap_or(self.transitions.policy.scroll_effect);
                    let easing = hint.easing.unwrap_or(self.transitions.policy.scroll_easing);
                    tracing::debug!(
                        "Starting scroll slide for window {} (dir={}, effect={:?}, easing={:?}, scroll_px={})",
                        hint.window_id,
                        dir,
                        effect,
                        easing,
                        scroll_px
                    );
                    self.transitions.scroll_slides.insert(
                        hint.window_id,
                        ScrollTransition {
                            started: now,
                            duration: self.transitions.policy.scroll_duration(),
                            bounds: hint.bounds,
                            direction: dir,
                            scroll_distance: scroll_px,
                            effect,
                            easing,
                            old_texture: tex,
                            old_view: view,
                            old_bind_group: bg,
                        },
                    );
                }
            }
        }
    }

    fn apply_effect_hint(&mut self, hint: &WindowEffectHint, now: std::time::Instant) {
        match hint {
            WindowEffectHint::TextFadeIn { window_id, bounds } => {
                if self.effects.text_fade_in.enabled {
                    if let Some(renderer) = self.renderer.as_mut() {
                        renderer.trigger_text_fade_in(*window_id, *bounds, now);
                    }
                }
            }
            WindowEffectHint::ScrollLineSpacing {
                window_id,
                bounds,
                direction,
            } => {
                if self.effects.scroll_line_spacing.enabled {
                    if let Some(renderer) = self.renderer.as_mut() {
                        renderer.trigger_scroll_line_spacing(*window_id, *bounds, *direction, now);
                    }
                }
            }
            WindowEffectHint::ScrollMomentum {
                window_id,
                bounds,
                direction,
            } => {
                if self.effects.scroll_momentum.enabled {
                    if let Some(renderer) = self.renderer.as_mut() {
                        renderer.trigger_scroll_momentum(*window_id, *bounds, *direction, now);
                    }
                }
            }
            WindowEffectHint::ScrollVelocityFade {
                window_id,
                bounds,
                delta,
            } => {
                if self.effects.scroll_velocity_fade.enabled {
                    if let Some(renderer) = self.renderer.as_mut() {
                        renderer.trigger_scroll_velocity_fade(*window_id, *bounds, *delta, now);
                    }
                }
            }
            WindowEffectHint::LineAnimation {
                bounds,
                edit_y,
                offset,
                ..
            } => {
                if self.effects.line_animation.enabled {
                    if let Some(renderer) = self.renderer.as_mut() {
                        renderer.start_line_animation(
                            *bounds,
                            *edit_y,
                            *offset,
                            self.effects.line_animation.duration_ms,
                        );
                    }
                }
            }
            WindowEffectHint::WindowSwitchFade { window_id, bounds } => {
                if self.effects.window_switch_fade.enabled {
                    if let Some(renderer) = self.renderer.as_mut() {
                        renderer.start_window_fade(*window_id, *bounds);
                        self.frame_dirty = true;
                    }
                }
            }
            WindowEffectHint::ThemeTransition { bounds } => {
                if !self.effects.theme_transition.enabled {
                    return;
                }
                if self.transitions.crossfades.contains_key(&-1) {
                    return;
                }
                if let Some((tex, view, bg_group)) = self.snapshot_prev_texture() {
                    tracing::debug!("Starting theme transition crossfade (effect hint)");
                    self.transitions.crossfades.insert(
                        -1,
                        CrossfadeTransition {
                            started: now,
                            duration: self.effects.theme_transition.duration,
                            bounds: *bounds,
                            effect: self.transitions.policy.crossfade_effect,
                            easing: self.transitions.policy.crossfade_easing,
                            old_texture: tex,
                            old_view: view,
                            old_bind_group: bg_group,
                        },
                    );
                }
            }
        }
    }

    /// Apply producer-emitted transition/effect hints.
    pub(super) fn detect_transitions(&mut self) {
        let (transition_hints, effect_hints) = match self.current_frame.as_mut() {
            Some(frame) => frame.take_runtime_hints(),
            None => return,
        };

        let now = std::time::Instant::now();

        for hint in &transition_hints {
            self.apply_transition_hint(hint, now);
        }
        for hint in &effect_hints {
            self.apply_effect_hint(hint, now);
        }
    }

    /// Render active transitions on top of the surface
    pub(super) fn render_transitions(&mut self, surface_view: &wgpu::TextureView) {
        let now = std::time::Instant::now();
        let renderer = match self.renderer.as_ref() {
            Some(r) => r,
            None => return,
        };

        // Get current offscreen bind group for "new" texture
        let current_bg = match self.current_offscreen_view_and_bg() {
            Some((_, bg)) => bg as *const wgpu::BindGroup,
            None => return,
        };

        // Render crossfades (using per-transition effect/easing)
        let mut completed_crossfades = Vec::new();
        for (&wid, transition) in &self.transitions.crossfades {
            let elapsed = now.duration_since(transition.started);
            let raw_t = (elapsed.as_secs_f32() / transition.duration.as_secs_f32()).min(1.0);
            let elapsed_secs = elapsed.as_secs_f32();

            // SAFETY: current_bg is valid for the duration of this function
            renderer.render_scroll_effect(
                surface_view,
                &transition.old_bind_group,
                unsafe { &*current_bg },
                raw_t,
                elapsed_secs,
                1, // direction: forward
                &transition.bounds,
                transition.bounds.height, // crossfade uses full bounds as slide distance
                transition.effect,
                transition.easing,
                self.width,
                self.height,
            );

            if raw_t >= 1.0 {
                completed_crossfades.push(wid);
            }
        }
        for wid in completed_crossfades {
            self.transitions.crossfades.remove(&wid);
        }

        // Render scroll slides
        let mut completed_scrolls = Vec::new();
        for (&wid, transition) in &self.transitions.scroll_slides {
            let elapsed = now.duration_since(transition.started);
            let raw_t = (elapsed.as_secs_f32() / transition.duration.as_secs_f32()).min(1.0);
            let elapsed_secs = elapsed.as_secs_f32();

            renderer.render_scroll_effect(
                surface_view,
                &transition.old_bind_group,
                unsafe { &*current_bg },
                raw_t,
                elapsed_secs,
                transition.direction,
                &transition.bounds,
                transition.scroll_distance,
                transition.effect,
                transition.easing,
                self.width,
                self.height,
            );

            if raw_t >= 1.0 {
                completed_scrolls.push(wid);
            }
        }
        for wid in completed_scrolls {
            self.transitions.scroll_slides.remove(&wid);
        }
    }
}

// ==========================================================================
// Tests
// ==========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn default_transition_state_has_expected_policy_defaults() {
        let ts = TransitionState::default();
        assert!(ts.policy.crossfade_enabled);
        assert!(ts.policy.scroll_enabled);
        assert_eq!(ts.policy.crossfade_duration(), Duration::from_millis(200));
        assert_eq!(ts.policy.scroll_duration(), Duration::from_millis(150));
        assert_eq!(ts.policy.crossfade_effect, ScrollEffect::Crossfade);
        assert_eq!(ts.policy.scroll_effect, ScrollEffect::Slide);
    }

    #[test]
    fn default_transition_state_starts_without_active_transitions() {
        let ts = TransitionState::default();
        assert!(ts.offscreen_a.is_none());
        assert!(ts.offscreen_b.is_none());
        assert!(ts.current_is_a);
        assert!(!ts.has_active());
    }
}
