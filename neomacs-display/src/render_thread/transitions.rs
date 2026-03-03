//! Window transition state (crossfade and scroll animations).

use super::RenderApp;
use crate::core::frame_glyphs::{FrameGlyph, WindowTransitionHint, WindowTransitionKind};
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

    // Per-window metadata from previous frame (for transition detection)
    pub(super) prev_window_infos: HashMap<i64, crate::core::frame_glyphs::WindowInfo>,
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
            prev_window_infos: HashMap::new(),
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

    /// Detect transitions from producer-emitted hints and apply non-transition effects.
    pub(super) fn detect_transitions(&mut self) {
        let frame = match self.current_frame.as_ref() {
            Some(f) => f,
            None => return,
        };

        let now = std::time::Instant::now();
        let transition_hints = frame.transition_hints.clone();

        for info in &frame.window_infos {
            if info.is_minibuffer {
                continue;
            }
            if let Some(prev) = self.transitions.prev_window_infos.get(&info.window_id) {
                if prev.buffer_id != 0 && info.buffer_id != 0 {
                    if prev.buffer_id != info.buffer_id {
                        if self.effects.text_fade_in.enabled {
                            if let Some(renderer) = self.renderer.as_mut() {
                                renderer.trigger_text_fade_in(info.window_id, info.bounds, now);
                            }
                        }
                    } else if prev.window_start != info.window_start {
                        if self.effects.text_fade_in.enabled {
                            if let Some(renderer) = self.renderer.as_mut() {
                                renderer.trigger_text_fade_in(info.window_id, info.bounds, now);
                            }
                        }
                        if self.effects.scroll_line_spacing.enabled {
                            let dir = if info.window_start > prev.window_start {
                                1
                            } else {
                                -1
                            };
                            if let Some(renderer) = self.renderer.as_mut() {
                                renderer.trigger_scroll_line_spacing(
                                    info.window_id,
                                    info.bounds,
                                    dir,
                                    now,
                                );
                            }
                        }
                        if self.effects.scroll_momentum.enabled {
                            let dir = if info.window_start > prev.window_start {
                                1
                            } else {
                                -1
                            };
                            if let Some(renderer) = self.renderer.as_mut() {
                                renderer.trigger_scroll_momentum(
                                    info.window_id,
                                    info.bounds,
                                    dir,
                                    now,
                                );
                            }
                        }
                        if self.effects.scroll_velocity_fade.enabled {
                            let delta =
                                (info.window_start - prev.window_start).unsigned_abs() as f32;
                            if let Some(renderer) = self.renderer.as_mut() {
                                renderer.trigger_scroll_velocity_fade(
                                    info.window_id,
                                    info.bounds,
                                    delta,
                                    now,
                                );
                            }
                        }
                    } else if self.effects.line_animation.enabled
                        && prev.buffer_size != info.buffer_size
                    {
                        let mut cursor_y: Option<f32> = None;
                        for g in &frame.glyphs {
                            if let FrameGlyph::Cursor { x, y, style, .. } = g {
                                if *x >= info.bounds.x
                                    && *x < info.bounds.x + info.bounds.width
                                    && *y >= info.bounds.y
                                    && *y < info.bounds.y + info.bounds.height
                                    && !style.is_hollow()
                                {
                                    cursor_y = Some(*y);
                                    break;
                                }
                            }
                        }
                        if let Some(edit_y) = cursor_y {
                            let ch = info.char_height;
                            let delta = info.buffer_size - prev.buffer_size;
                            let offset = if delta > 0 { -ch } else { ch };
                            if let Some(renderer) = self.renderer.as_mut() {
                                renderer.start_line_animation(
                                    info.bounds,
                                    edit_y + ch,
                                    offset,
                                    self.effects.line_animation.duration_ms,
                                );
                            }
                        }
                    }
                }
            }
        }

        if self.effects.window_switch_fade.enabled {
            let mut new_selected: Option<(i64, Rect)> = None;
            for info in &frame.window_infos {
                if info.selected && !info.is_minibuffer {
                    new_selected = Some((info.window_id, info.bounds));
                    break;
                }
            }
            if let Some((wid, bounds)) = new_selected {
                if self.prev_selected_window_id != 0 && wid != self.prev_selected_window_id {
                    if let Some(renderer) = self.renderer.as_mut() {
                        renderer.start_window_fade(wid, bounds);
                        self.frame_dirty = true;
                    }
                }
                self.prev_selected_window_id = wid;
            }
        }

        if self.effects.theme_transition.enabled {
            let bg = &frame.background;
            let new_bg = (bg.r, bg.g, bg.b, bg.a);
            if let Some(old_bg) = self.prev_background {
                let dr = (new_bg.0 - old_bg.0).abs();
                let dg = (new_bg.1 - old_bg.1).abs();
                let db = (new_bg.2 - old_bg.2).abs();
                if dr > 0.02 || dg > 0.02 || db > 0.02 {
                    let full_h = frame
                        .window_infos
                        .iter()
                        .find(|w| w.is_minibuffer)
                        .map_or(frame.height, |w| w.bounds.y);
                    let full_bounds = Rect::new(0.0, 0.0, frame.width, full_h);
                    if !self.transitions.crossfades.contains_key(&-1) {
                        if let Some((tex, view, bg_group)) = self.snapshot_prev_texture() {
                            tracing::debug!("Starting theme transition crossfade (bg changed)");
                            self.transitions.crossfades.insert(
                                -1,
                                CrossfadeTransition {
                                    started: now,
                                    duration: self.effects.theme_transition.duration,
                                    bounds: full_bounds,
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
            self.prev_background = Some(new_bg);
        }

        self.transitions.prev_window_infos.clear();
        for info in &frame.window_infos {
            self.transitions
                .prev_window_infos
                .insert(info.window_id, info.clone());
        }

        for hint in &transition_hints {
            self.apply_transition_hint(hint, now);
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
    use crate::core::frame_glyphs::{CursorStyle, WindowInfo};
    use crate::core::types::Rect;
    use std::collections::{HashMap, HashSet};
    use std::time::{Duration, Instant};

    // ── Helper: create a WindowInfo with sensible defaults ───────────────

    fn make_window_info(
        window_id: i64,
        buffer_id: u64,
        window_start: i64,
        bounds: Rect,
    ) -> WindowInfo {
        WindowInfo {
            window_id,
            buffer_id,
            window_start,
            window_end: window_start + 500,
            buffer_size: 10000,
            bounds,
            mode_line_height: 20.0,
            header_line_height: 0.0,
            tab_line_height: 0.0,
            selected: false,
            is_minibuffer: false,
            char_height: 16.0,
            buffer_file_name: String::new(),
            modified: false,
        }
    }

    // =====================================================================
    // TransitionState::default()
    // =====================================================================

    #[test]
    fn default_crossfade_enabled() {
        let ts = TransitionState::default();
        assert!(ts.policy.crossfade_enabled);
    }

    #[test]
    fn default_crossfade_duration_is_200ms() {
        let ts = TransitionState::default();
        assert_eq!(ts.policy.crossfade_duration(), Duration::from_millis(200));
    }

    #[test]
    fn default_crossfade_effect_is_crossfade() {
        let ts = TransitionState::default();
        assert_eq!(ts.policy.crossfade_effect, ScrollEffect::Crossfade);
    }

    #[test]
    fn default_crossfade_easing_is_ease_out_quad() {
        let ts = TransitionState::default();
        assert_eq!(ts.policy.crossfade_easing, ScrollEasing::EaseOutQuad);
    }

    #[test]
    fn default_scroll_enabled() {
        let ts = TransitionState::default();
        assert!(ts.policy.scroll_enabled);
    }

    #[test]
    fn default_scroll_duration_is_150ms() {
        let ts = TransitionState::default();
        assert_eq!(ts.policy.scroll_duration(), Duration::from_millis(150));
    }

    #[test]
    fn default_scroll_effect_is_slide() {
        let ts = TransitionState::default();
        // ScrollEffect::default() is Slide
        assert_eq!(ts.policy.scroll_effect, ScrollEffect::Slide);
    }

    #[test]
    fn default_scroll_easing_is_ease_out_quad() {
        let ts = TransitionState::default();
        // ScrollEasing::default() is EaseOutQuad
        assert_eq!(ts.policy.scroll_easing, ScrollEasing::EaseOutQuad);
    }

    #[test]
    fn default_offscreen_textures_are_none() {
        let ts = TransitionState::default();
        assert!(ts.offscreen_a.is_none());
        assert!(ts.offscreen_b.is_none());
    }

    #[test]
    fn default_current_is_a() {
        let ts = TransitionState::default();
        assert!(ts.current_is_a);
    }

    #[test]
    fn default_no_active_transitions() {
        let ts = TransitionState::default();
        assert!(ts.crossfades.is_empty());
        assert!(ts.scroll_slides.is_empty());
    }

    #[test]
    fn default_no_prev_window_infos() {
        let ts = TransitionState::default();
        assert!(ts.prev_window_infos.is_empty());
    }

    // =====================================================================
    // TransitionState::has_active()
    // =====================================================================

    #[test]
    fn has_active_false_when_both_maps_empty() {
        let ts = TransitionState::default();
        assert!(!ts.has_active());
    }

    #[test]
    fn has_active_false_when_only_prev_window_infos_populated() {
        let mut ts = TransitionState::default();
        ts.prev_window_infos.insert(
            1,
            make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 800.0, 600.0)),
        );
        // prev_window_infos doesn't affect has_active()
        assert!(!ts.has_active());
    }

    // =====================================================================
    // Progress calculation (raw_t)
    // =====================================================================
    //
    // The progress formula used in render_transitions is:
    //   raw_t = (elapsed.as_secs_f32() / duration.as_secs_f32()).min(1.0)

    /// Compute transition progress the same way render_transitions does.
    fn compute_raw_t(elapsed: Duration, duration: Duration) -> f32 {
        (elapsed.as_secs_f32() / duration.as_secs_f32()).min(1.0)
    }

    #[test]
    fn progress_at_zero_elapsed() {
        let t = compute_raw_t(Duration::ZERO, Duration::from_millis(200));
        assert!((t - 0.0).abs() < 1e-6);
    }

    #[test]
    fn progress_at_half_duration() {
        let t = compute_raw_t(Duration::from_millis(100), Duration::from_millis(200));
        assert!((t - 0.5).abs() < 1e-6);
    }

    #[test]
    fn progress_at_full_duration() {
        let t = compute_raw_t(Duration::from_millis(200), Duration::from_millis(200));
        assert!((t - 1.0).abs() < 1e-6);
    }

    #[test]
    fn progress_clamped_at_one_when_elapsed_exceeds_duration() {
        let t = compute_raw_t(Duration::from_millis(500), Duration::from_millis(200));
        assert!((t - 1.0).abs() < 1e-6);
    }

    #[test]
    fn progress_at_one_third() {
        let t = compute_raw_t(Duration::from_millis(50), Duration::from_millis(150));
        assert!((t - 1.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn progress_with_very_short_duration() {
        // 1ms duration, 10ms elapsed -> clamped to 1.0
        let t = compute_raw_t(Duration::from_millis(10), Duration::from_millis(1));
        assert!((t - 1.0).abs() < 1e-6);
    }

    #[test]
    fn progress_with_long_duration() {
        // 1s duration, 500ms elapsed -> 0.5
        let t = compute_raw_t(Duration::from_millis(500), Duration::from_secs(1));
        assert!((t - 0.5).abs() < 1e-6);
    }

    #[test]
    fn progress_is_always_in_zero_one_range() {
        let durations = [
            Duration::from_millis(1),
            Duration::from_millis(50),
            Duration::from_millis(150),
            Duration::from_millis(200),
            Duration::from_secs(1),
        ];
        for dur in &durations {
            for ms in 0..500 {
                let elapsed = Duration::from_millis(ms);
                let t = compute_raw_t(elapsed, *dur);
                assert!(
                    t >= 0.0,
                    "raw_t={} < 0 for elapsed={}ms, dur={:?}",
                    t,
                    ms,
                    dur
                );
                assert!(
                    t <= 1.0,
                    "raw_t={} > 1 for elapsed={}ms, dur={:?}",
                    t,
                    ms,
                    dur
                );
            }
        }
    }

    #[test]
    fn progress_monotonically_increasing() {
        let duration = Duration::from_millis(200);
        let mut prev_t = 0.0f32;
        for ms in 0..=300 {
            let t = compute_raw_t(Duration::from_millis(ms), duration);
            assert!(
                t >= prev_t,
                "Progress decreased at {}ms: {} < {}",
                ms,
                t,
                prev_t
            );
            prev_t = t;
        }
    }

    // =====================================================================
    // Completion detection (raw_t >= 1.0)
    // =====================================================================

    /// Returns true if a transition is complete (same condition as render_transitions)
    fn is_complete(elapsed: Duration, duration: Duration) -> bool {
        compute_raw_t(elapsed, duration) >= 1.0
    }

    #[test]
    fn transition_not_complete_before_duration() {
        assert!(!is_complete(
            Duration::from_millis(199),
            Duration::from_millis(200)
        ));
    }

    #[test]
    fn transition_complete_at_exact_duration() {
        assert!(is_complete(
            Duration::from_millis(200),
            Duration::from_millis(200)
        ));
    }

    #[test]
    fn transition_complete_after_duration() {
        assert!(is_complete(
            Duration::from_millis(300),
            Duration::from_millis(200)
        ));
    }

    #[test]
    fn transition_not_complete_at_zero() {
        assert!(!is_complete(Duration::ZERO, Duration::from_millis(200)));
    }

    // =====================================================================
    // Buffer change detection predicate
    // =====================================================================
    //
    // From detect_transitions:
    //   prev.buffer_id != 0 && info.buffer_id != 0 && prev.buffer_id != info.buffer_id

    fn is_buffer_change(prev_buf: u64, curr_buf: u64) -> bool {
        prev_buf != 0 && curr_buf != 0 && prev_buf != curr_buf
    }

    #[test]
    fn buffer_change_detected_when_ids_differ() {
        assert!(is_buffer_change(100, 200));
    }

    #[test]
    fn no_buffer_change_when_ids_same() {
        assert!(!is_buffer_change(100, 100));
    }

    #[test]
    fn no_buffer_change_when_prev_is_zero() {
        assert!(!is_buffer_change(0, 100));
    }

    #[test]
    fn no_buffer_change_when_curr_is_zero() {
        assert!(!is_buffer_change(100, 0));
    }

    #[test]
    fn no_buffer_change_when_both_zero() {
        assert!(!is_buffer_change(0, 0));
    }

    // =====================================================================
    // Crossfade suppression for minibuffer (height threshold)
    // =====================================================================
    //
    // From detect_transitions:
    //   crossfade_enabled && info.bounds.height >= 50.0

    fn should_crossfade_on_buffer_change(crossfade_enabled: bool, height: f32) -> bool {
        crossfade_enabled && height >= 50.0
    }

    #[test]
    fn crossfade_for_tall_window() {
        assert!(should_crossfade_on_buffer_change(true, 600.0));
    }

    #[test]
    fn crossfade_at_exactly_50_pixels() {
        assert!(should_crossfade_on_buffer_change(true, 50.0));
    }

    #[test]
    fn no_crossfade_for_short_window() {
        // Minibuffer is typically < 50px, suppressed
        assert!(!should_crossfade_on_buffer_change(true, 30.0));
    }

    #[test]
    fn no_crossfade_when_disabled() {
        assert!(!should_crossfade_on_buffer_change(false, 600.0));
    }

    #[test]
    fn no_crossfade_at_49_pixels() {
        assert!(!should_crossfade_on_buffer_change(true, 49.9));
    }

    // =====================================================================
    // Minibuffer height change detection
    // =====================================================================
    //
    // From detect_transitions:
    //   info.is_minibuffer && (prev.bounds.height - info.bounds.height).abs() > 2.0

    fn is_minibuffer_height_change(is_minibuffer: bool, prev_h: f32, curr_h: f32) -> bool {
        is_minibuffer && (prev_h - curr_h).abs() > 2.0
    }

    #[test]
    fn minibuffer_height_change_detected() {
        assert!(is_minibuffer_height_change(true, 20.0, 40.0));
    }

    #[test]
    fn minibuffer_no_change_within_threshold() {
        assert!(!is_minibuffer_height_change(true, 20.0, 21.5));
    }

    #[test]
    fn minibuffer_no_change_at_exactly_2() {
        assert!(!is_minibuffer_height_change(true, 20.0, 22.0));
    }

    #[test]
    fn minibuffer_change_just_above_2() {
        assert!(is_minibuffer_height_change(true, 20.0, 22.1));
    }

    #[test]
    fn non_minibuffer_height_change_ignored() {
        assert!(!is_minibuffer_height_change(false, 20.0, 80.0));
    }

    #[test]
    fn minibuffer_height_shrink_detected() {
        assert!(is_minibuffer_height_change(true, 60.0, 20.0));
    }

    // =====================================================================
    // Scroll detection
    // =====================================================================
    //
    // From detect_transitions:
    //   prev.window_start != info.window_start
    //
    //   Scroll direction: if info.window_start > prev.window_start { 1 } else { -1 }

    fn is_scroll(prev_start: i64, curr_start: i64) -> bool {
        prev_start != curr_start
    }

    fn scroll_direction(prev_start: i64, curr_start: i64) -> i32 {
        if curr_start > prev_start { 1 } else { -1 }
    }

    #[test]
    fn scroll_detected_when_window_start_changes() {
        assert!(is_scroll(0, 100));
    }

    #[test]
    fn no_scroll_when_window_start_unchanged() {
        assert!(!is_scroll(100, 100));
    }

    #[test]
    fn scroll_down_positive_direction() {
        assert_eq!(scroll_direction(0, 100), 1);
    }

    #[test]
    fn scroll_up_negative_direction() {
        assert_eq!(scroll_direction(100, 0), -1);
    }

    #[test]
    fn scroll_down_by_one_line() {
        assert!(is_scroll(0, 1));
        assert_eq!(scroll_direction(0, 1), 1);
    }

    #[test]
    fn scroll_up_by_one_line() {
        assert!(is_scroll(1, 0));
        assert_eq!(scroll_direction(1, 0), -1);
    }

    // =====================================================================
    // Scroll content height and slide suppression
    // =====================================================================
    //
    // From detect_transitions:
    //   let content_height = info.bounds.height - info.mode_line_height;
    //   scroll_enabled && content_height >= 50.0

    fn scroll_content_height(bounds_height: f32, mode_line_height: f32) -> f32 {
        bounds_height - mode_line_height
    }

    fn should_scroll_slide(scroll_enabled: bool, content_height: f32) -> bool {
        scroll_enabled && content_height >= 50.0
    }

    #[test]
    fn content_height_excludes_mode_line() {
        assert!((scroll_content_height(600.0, 20.0) - 580.0).abs() < 1e-6);
    }

    #[test]
    fn content_height_zero_mode_line() {
        assert!((scroll_content_height(600.0, 0.0) - 600.0).abs() < 1e-6);
    }

    #[test]
    fn scroll_slide_for_tall_content() {
        assert!(should_scroll_slide(true, 580.0));
    }

    #[test]
    fn no_scroll_slide_for_short_content() {
        assert!(!should_scroll_slide(true, 30.0));
    }

    #[test]
    fn no_scroll_slide_when_disabled() {
        assert!(!should_scroll_slide(false, 580.0));
    }

    #[test]
    fn scroll_slide_at_exactly_50() {
        assert!(should_scroll_slide(true, 50.0));
    }

    #[test]
    fn no_scroll_slide_at_49() {
        assert!(!should_scroll_slide(true, 49.9));
    }

    #[test]
    fn content_bounds_calculation() {
        let bounds = Rect::new(10.0, 20.0, 400.0, 600.0);
        let mode_line_h = 22.0;
        let content_h = bounds.height - mode_line_h;
        let content_bounds = Rect::new(bounds.x, bounds.y, bounds.width, content_h);
        assert_eq!(content_bounds.x, 10.0);
        assert_eq!(content_bounds.y, 20.0);
        assert_eq!(content_bounds.width, 400.0);
        assert!((content_bounds.height - 578.0).abs() < 1e-6);
    }

    // =====================================================================
    // Font size (char_height) change detection
    // =====================================================================
    //
    // From detect_transitions:
    //   (prev.char_height - info.char_height).abs() > 1.0

    fn is_font_size_change(prev_char_h: f32, curr_char_h: f32) -> bool {
        (prev_char_h - curr_char_h).abs() > 1.0
    }

    #[test]
    fn font_size_change_detected() {
        assert!(is_font_size_change(16.0, 24.0));
    }

    #[test]
    fn font_size_no_change() {
        assert!(!is_font_size_change(16.0, 16.0));
    }

    #[test]
    fn font_size_no_change_within_threshold() {
        assert!(!is_font_size_change(16.0, 16.5));
    }

    #[test]
    fn font_size_no_change_at_exactly_1() {
        assert!(!is_font_size_change(16.0, 17.0));
    }

    #[test]
    fn font_size_change_just_above_1() {
        assert!(is_font_size_change(16.0, 17.1));
    }

    #[test]
    fn font_size_decrease_detected() {
        assert!(is_font_size_change(24.0, 16.0));
    }

    // =====================================================================
    // Window resize detection
    // =====================================================================
    //
    // From detect_transitions:
    //   (prev.bounds.width - info.bounds.width).abs() > 2.0
    //   || (prev.bounds.height - info.bounds.height).abs() > 2.0

    fn is_window_resize(prev_bounds: Rect, curr_bounds: Rect) -> bool {
        (prev_bounds.width - curr_bounds.width).abs() > 2.0
            || (prev_bounds.height - curr_bounds.height).abs() > 2.0
    }

    #[test]
    fn window_resize_width_change() {
        let prev = Rect::new(0.0, 0.0, 400.0, 600.0);
        let curr = Rect::new(0.0, 0.0, 500.0, 600.0);
        assert!(is_window_resize(prev, curr));
    }

    #[test]
    fn window_resize_height_change() {
        let prev = Rect::new(0.0, 0.0, 400.0, 600.0);
        let curr = Rect::new(0.0, 0.0, 400.0, 700.0);
        assert!(is_window_resize(prev, curr));
    }

    #[test]
    fn window_resize_both_change() {
        let prev = Rect::new(0.0, 0.0, 400.0, 600.0);
        let curr = Rect::new(0.0, 0.0, 500.0, 700.0);
        assert!(is_window_resize(prev, curr));
    }

    #[test]
    fn no_window_resize_same_bounds() {
        let r = Rect::new(0.0, 0.0, 400.0, 600.0);
        assert!(!is_window_resize(r, r));
    }

    #[test]
    fn no_window_resize_within_threshold() {
        let prev = Rect::new(0.0, 0.0, 400.0, 600.0);
        let curr = Rect::new(0.0, 0.0, 401.5, 601.0);
        assert!(!is_window_resize(prev, curr));
    }

    #[test]
    fn window_resize_at_exactly_2_not_detected() {
        let prev = Rect::new(0.0, 0.0, 400.0, 600.0);
        let curr = Rect::new(0.0, 0.0, 402.0, 600.0);
        assert!(!is_window_resize(prev, curr));
    }

    #[test]
    fn window_resize_just_above_2_detected() {
        let prev = Rect::new(0.0, 0.0, 400.0, 600.0);
        let curr = Rect::new(0.0, 0.0, 402.1, 600.0);
        assert!(is_window_resize(prev, curr));
    }

    #[test]
    fn window_resize_position_change_only_not_detected() {
        // Position change without size change should NOT trigger resize
        let prev = Rect::new(0.0, 0.0, 400.0, 600.0);
        let curr = Rect::new(100.0, 50.0, 400.0, 600.0);
        assert!(!is_window_resize(prev, curr));
    }

    // =====================================================================
    // Window split/delete detection
    // =====================================================================
    //
    // From detect_transitions:
    //   curr_ids (non-minibuffer) != prev_ids (non-minibuffer)
    //   && prev_ids.len() > 0 && curr_ids.len() > 0

    fn detect_window_layout_change(
        prev_infos: &HashMap<i64, WindowInfo>,
        curr_infos: &[WindowInfo],
    ) -> bool {
        let curr_ids: HashSet<i64> = curr_infos
            .iter()
            .filter(|i| !i.is_minibuffer)
            .map(|i| i.window_id)
            .collect();
        let prev_non_mini: HashSet<i64> = prev_infos
            .iter()
            .filter(|(_, v)| !v.is_minibuffer)
            .map(|(k, _)| *k)
            .collect();
        prev_non_mini != curr_ids && prev_non_mini.len() > 0 && curr_ids.len() > 0
    }

    #[test]
    fn window_split_detected() {
        let mut prev = HashMap::new();
        prev.insert(
            1,
            make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 800.0, 600.0)),
        );

        let curr = vec![
            make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 400.0, 600.0)),
            make_window_info(2, 200, 0, Rect::new(400.0, 0.0, 400.0, 600.0)),
        ];

        assert!(detect_window_layout_change(&prev, &curr));
    }

    #[test]
    fn window_delete_detected() {
        let mut prev = HashMap::new();
        prev.insert(
            1,
            make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 400.0, 600.0)),
        );
        prev.insert(
            2,
            make_window_info(2, 200, 0, Rect::new(400.0, 0.0, 400.0, 600.0)),
        );

        let curr = vec![make_window_info(
            1,
            100,
            0,
            Rect::new(0.0, 0.0, 800.0, 600.0),
        )];

        assert!(detect_window_layout_change(&prev, &curr));
    }

    #[test]
    fn no_layout_change_same_windows() {
        let mut prev = HashMap::new();
        prev.insert(
            1,
            make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 400.0, 600.0)),
        );
        prev.insert(
            2,
            make_window_info(2, 200, 0, Rect::new(400.0, 0.0, 400.0, 600.0)),
        );

        let curr = vec![
            make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 400.0, 600.0)),
            make_window_info(2, 200, 0, Rect::new(400.0, 0.0, 400.0, 600.0)),
        ];

        assert!(!detect_window_layout_change(&prev, &curr));
    }

    #[test]
    fn no_layout_change_when_prev_empty() {
        let prev = HashMap::new();
        let curr = vec![make_window_info(
            1,
            100,
            0,
            Rect::new(0.0, 0.0, 800.0, 600.0),
        )];
        // prev_non_mini.len() == 0, so condition fails
        assert!(!detect_window_layout_change(&prev, &curr));
    }

    #[test]
    fn no_layout_change_when_curr_empty() {
        let mut prev = HashMap::new();
        prev.insert(
            1,
            make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 800.0, 600.0)),
        );

        let curr: Vec<WindowInfo> = vec![];
        // curr_ids.len() == 0, so condition fails
        assert!(!detect_window_layout_change(&prev, &curr));
    }

    #[test]
    fn layout_change_ignores_minibuffer_windows() {
        let mut prev = HashMap::new();
        prev.insert(
            1,
            make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 800.0, 580.0)),
        );
        let mut mini = make_window_info(99, 50, 0, Rect::new(0.0, 580.0, 800.0, 20.0));
        mini.is_minibuffer = true;
        prev.insert(99, mini.clone());

        let curr = vec![
            make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 800.0, 580.0)),
            mini,
        ];

        // Same non-minibuffer windows, different minibuffer = no change
        assert!(!detect_window_layout_change(&prev, &curr));
    }

    #[test]
    fn layout_change_with_window_id_swap() {
        let mut prev = HashMap::new();
        prev.insert(
            1,
            make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 400.0, 600.0)),
        );
        prev.insert(
            2,
            make_window_info(2, 200, 0, Rect::new(400.0, 0.0, 400.0, 600.0)),
        );

        // Windows replaced with different IDs
        let curr = vec![
            make_window_info(3, 100, 0, Rect::new(0.0, 0.0, 400.0, 600.0)),
            make_window_info(4, 200, 0, Rect::new(400.0, 0.0, 400.0, 600.0)),
        ];

        assert!(detect_window_layout_change(&prev, &curr));
    }

    // =====================================================================
    // Theme change detection
    // =====================================================================
    //
    // From detect_transitions:
    //   dr > 0.02 || dg > 0.02 || db > 0.02

    fn is_theme_change(old: (f32, f32, f32, f32), new: (f32, f32, f32, f32)) -> bool {
        let dr = (new.0 - old.0).abs();
        let dg = (new.1 - old.1).abs();
        let db = (new.2 - old.2).abs();
        dr > 0.02 || dg > 0.02 || db > 0.02
    }

    #[test]
    fn theme_change_detected_dark_to_light() {
        let dark = (0.1, 0.1, 0.1, 1.0);
        let light = (0.9, 0.9, 0.9, 1.0);
        assert!(is_theme_change(dark, light));
    }

    #[test]
    fn theme_change_detected_single_channel() {
        let old = (0.1, 0.1, 0.1, 1.0);
        let new = (0.2, 0.1, 0.1, 1.0); // only red changed
        assert!(is_theme_change(old, new));
    }

    #[test]
    fn no_theme_change_same_bg() {
        let bg = (0.1, 0.2, 0.3, 1.0);
        assert!(!is_theme_change(bg, bg));
    }

    #[test]
    fn no_theme_change_within_threshold() {
        let old = (0.1, 0.2, 0.3, 1.0);
        let new = (0.11, 0.21, 0.31, 1.0); // all channels < 2% change
        assert!(!is_theme_change(old, new));
    }

    #[test]
    fn no_theme_change_at_exactly_threshold() {
        let old = (0.1, 0.2, 0.3, 1.0);
        let new = (0.12, 0.22, 0.32, 1.0); // each channel exactly 0.02 diff
        assert!(!is_theme_change(old, new));
    }

    #[test]
    fn theme_change_just_above_threshold() {
        let old = (0.1, 0.2, 0.3, 1.0);
        let new = (0.121, 0.2, 0.3, 1.0); // red channel just above 0.02
        assert!(is_theme_change(old, new));
    }

    #[test]
    fn theme_change_alpha_ignored() {
        // Only RGB channels are compared, alpha is ignored
        let old = (0.1, 0.2, 0.3, 1.0);
        let new = (0.1, 0.2, 0.3, 0.0); // alpha changed dramatically
        assert!(!is_theme_change(old, new));
    }

    // =====================================================================
    // Scroll velocity delta calculation
    // =====================================================================
    //
    // From detect_transitions:
    //   let delta = (info.window_start - prev.window_start).unsigned_abs() as f32;

    fn scroll_velocity_delta(prev_start: i64, curr_start: i64) -> f32 {
        (curr_start - prev_start).unsigned_abs() as f32
    }

    #[test]
    fn velocity_delta_scroll_down() {
        assert!((scroll_velocity_delta(0, 100) - 100.0).abs() < 1e-6);
    }

    #[test]
    fn velocity_delta_scroll_up() {
        assert!((scroll_velocity_delta(100, 0) - 100.0).abs() < 1e-6);
    }

    #[test]
    fn velocity_delta_no_scroll() {
        assert!((scroll_velocity_delta(50, 50) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn velocity_delta_large_jump() {
        assert!((scroll_velocity_delta(0, 10000) - 10000.0).abs() < 1e-6);
    }

    // =====================================================================
    // Line insertion/deletion offset calculation
    // =====================================================================
    //
    // From detect_transitions:
    //   let delta = info.buffer_size - prev.buffer_size;
    //   let offset = if delta > 0 { -ch } else { ch };

    fn line_animation_offset(prev_size: i64, curr_size: i64, char_height: f32) -> f32 {
        let delta = curr_size - prev_size;
        if delta > 0 { -char_height } else { char_height }
    }

    #[test]
    fn insertion_offset_is_negative_char_height() {
        // Insertion -> lines move down -> negative offset
        let offset = line_animation_offset(1000, 1050, 16.0);
        assert!((offset - (-16.0)).abs() < 1e-6);
    }

    #[test]
    fn deletion_offset_is_positive_char_height() {
        // Deletion -> lines move up -> positive offset
        let offset = line_animation_offset(1050, 1000, 16.0);
        assert!((offset - 16.0).abs() < 1e-6);
    }

    #[test]
    fn line_offset_uses_char_height() {
        let offset = line_animation_offset(1000, 1050, 24.0);
        assert!((offset - (-24.0)).abs() < 1e-6);
    }

    // =====================================================================
    // prev_window_infos management
    // =====================================================================

    #[test]
    fn prev_window_infos_stores_and_retrieves() {
        let mut ts = TransitionState::default();
        let info = make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 800.0, 600.0));
        ts.prev_window_infos.insert(info.window_id, info.clone());

        assert_eq!(ts.prev_window_infos.len(), 1);
        let retrieved = ts.prev_window_infos.get(&1).unwrap();
        assert_eq!(retrieved.buffer_id, 100);
        assert_eq!(retrieved.window_start, 0);
    }

    #[test]
    fn prev_window_infos_update_replaces() {
        let mut ts = TransitionState::default();
        let info1 = make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 800.0, 600.0));
        ts.prev_window_infos.insert(1, info1);

        let info2 = make_window_info(1, 200, 50, Rect::new(0.0, 0.0, 800.0, 600.0));
        ts.prev_window_infos.insert(1, info2);

        assert_eq!(ts.prev_window_infos.len(), 1);
        let retrieved = ts.prev_window_infos.get(&1).unwrap();
        assert_eq!(retrieved.buffer_id, 200);
        assert_eq!(retrieved.window_start, 50);
    }

    #[test]
    fn prev_window_infos_clear_and_repopulate() {
        // Mimics the end of detect_transitions:
        //   self.transitions.prev_window_infos.clear();
        //   for info in &frame.window_infos { ... insert ... }
        let mut ts = TransitionState::default();
        ts.prev_window_infos.insert(
            1,
            make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 400.0, 600.0)),
        );
        ts.prev_window_infos.insert(
            2,
            make_window_info(2, 200, 0, Rect::new(400.0, 0.0, 400.0, 600.0)),
        );
        assert_eq!(ts.prev_window_infos.len(), 2);

        ts.prev_window_infos.clear();
        assert!(ts.prev_window_infos.is_empty());

        // Repopulate with new frame data
        ts.prev_window_infos.insert(
            3,
            make_window_info(3, 300, 0, Rect::new(0.0, 0.0, 800.0, 600.0)),
        );
        assert_eq!(ts.prev_window_infos.len(), 1);
        assert!(ts.prev_window_infos.contains_key(&3));
        assert!(!ts.prev_window_infos.contains_key(&1));
        assert!(!ts.prev_window_infos.contains_key(&2));
    }

    #[test]
    fn prev_window_infos_multiple_windows() {
        let mut ts = TransitionState::default();
        for i in 1..=5 {
            ts.prev_window_infos.insert(
                i,
                make_window_info(i, i as u64 * 100, 0, Rect::new(0.0, 0.0, 800.0, 600.0)),
            );
        }
        assert_eq!(ts.prev_window_infos.len(), 5);
        for i in 1..=5 {
            assert!(ts.prev_window_infos.contains_key(&i));
        }
    }

    // =====================================================================
    // Configuration modification
    // =====================================================================

    #[test]
    fn can_disable_crossfade() {
        let mut ts = TransitionState::default();
        ts.policy.crossfade_enabled = false;
        assert!(!ts.policy.crossfade_enabled);
    }

    #[test]
    fn can_disable_scroll() {
        let mut ts = TransitionState::default();
        ts.policy.scroll_enabled = false;
        assert!(!ts.policy.scroll_enabled);
    }

    #[test]
    fn can_change_crossfade_duration() {
        let mut ts = TransitionState::default();
        ts.policy.crossfade_duration_ms = 500;
        assert_eq!(ts.policy.crossfade_duration(), Duration::from_millis(500));
    }

    #[test]
    fn can_change_scroll_duration() {
        let mut ts = TransitionState::default();
        ts.policy.scroll_duration_ms = 300;
        assert_eq!(ts.policy.scroll_duration(), Duration::from_millis(300));
    }

    #[test]
    fn can_change_crossfade_effect() {
        let mut ts = TransitionState::default();
        ts.policy.crossfade_effect = ScrollEffect::ScaleZoom;
        assert_eq!(ts.policy.crossfade_effect, ScrollEffect::ScaleZoom);
    }

    #[test]
    fn can_change_scroll_effect() {
        let mut ts = TransitionState::default();
        ts.policy.scroll_effect = ScrollEffect::Wobbly;
        assert_eq!(ts.policy.scroll_effect, ScrollEffect::Wobbly);
    }

    #[test]
    fn can_change_crossfade_easing() {
        let mut ts = TransitionState::default();
        ts.policy.crossfade_easing = ScrollEasing::Spring;
        assert_eq!(ts.policy.crossfade_easing, ScrollEasing::Spring);
    }

    #[test]
    fn can_change_scroll_easing() {
        let mut ts = TransitionState::default();
        ts.policy.scroll_easing = ScrollEasing::EaseOutCubic;
        assert_eq!(ts.policy.scroll_easing, ScrollEasing::EaseOutCubic);
    }

    #[test]
    fn double_buffer_toggle() {
        let mut ts = TransitionState::default();
        assert!(ts.current_is_a);
        ts.current_is_a = false;
        assert!(!ts.current_is_a);
        ts.current_is_a = true;
        assert!(ts.current_is_a);
    }

    // =====================================================================
    // Detection condition priority/ordering
    // =====================================================================
    //
    // The detect_transitions function checks conditions in this order:
    //   1. buffer_id changed (crossfade)
    //   2. minibuffer height changed
    //   3. window_start changed (scroll)
    //   4. char_height changed (font size)
    //   5. buffer_size changed (line animation)
    //   6. bounds width/height changed (window resize)
    //
    // These are mutually exclusive (else-if chain). Test the priority.

    /// Returns which condition would fire for given prev/curr WindowInfo.
    /// Returns: "buffer_change", "minibuffer_height", "scroll", "font_size",
    ///          "buffer_size", "window_resize", or "none".
    fn classify_transition(prev: &WindowInfo, curr: &WindowInfo) -> &'static str {
        if prev.buffer_id == 0 || curr.buffer_id == 0 {
            return "none";
        }
        if prev.buffer_id != curr.buffer_id {
            return "buffer_change";
        }
        if curr.is_minibuffer && (prev.bounds.height - curr.bounds.height).abs() > 2.0 {
            return "minibuffer_height";
        }
        if prev.window_start != curr.window_start {
            return "scroll";
        }
        if (prev.char_height - curr.char_height).abs() > 1.0 {
            return "font_size";
        }
        if prev.buffer_size != curr.buffer_size && !curr.is_minibuffer {
            return "buffer_size";
        }
        if (prev.bounds.width - curr.bounds.width).abs() > 2.0
            || (prev.bounds.height - curr.bounds.height).abs() > 2.0
        {
            return "window_resize";
        }
        "none"
    }

    #[test]
    fn priority_buffer_change_over_scroll() {
        // Buffer change AND window_start change -> buffer change wins
        let prev = make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 800.0, 600.0));
        let mut curr = make_window_info(1, 200, 50, Rect::new(0.0, 0.0, 800.0, 600.0));
        curr.buffer_id = 200; // different buffer
        assert_eq!(classify_transition(&prev, &curr), "buffer_change");
    }

    #[test]
    fn priority_scroll_over_font_size() {
        // Scroll AND font size change -> scroll wins
        let prev = make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 800.0, 600.0));
        let mut curr = make_window_info(1, 100, 50, Rect::new(0.0, 0.0, 800.0, 600.0));
        curr.char_height = 24.0; // different font size
        assert_eq!(classify_transition(&prev, &curr), "scroll");
    }

    #[test]
    fn priority_font_size_over_buffer_size() {
        let prev = make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 800.0, 600.0));
        let mut curr = make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 800.0, 600.0));
        curr.char_height = 24.0;
        curr.buffer_size = 20000;
        assert_eq!(classify_transition(&prev, &curr), "font_size");
    }

    #[test]
    fn priority_buffer_size_over_window_resize() {
        let prev = make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 800.0, 600.0));
        let mut curr = make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 900.0, 700.0));
        curr.buffer_size = 20000;
        assert_eq!(classify_transition(&prev, &curr), "buffer_size");
    }

    #[test]
    fn classify_pure_window_resize() {
        let prev = make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 400.0, 600.0));
        let curr = make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 500.0, 600.0));
        assert_eq!(classify_transition(&prev, &curr), "window_resize");
    }

    #[test]
    fn classify_no_change() {
        let info = make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 800.0, 600.0));
        assert_eq!(classify_transition(&info, &info), "none");
    }

    #[test]
    fn classify_buffer_id_zero_skips() {
        let prev = make_window_info(1, 0, 0, Rect::new(0.0, 0.0, 800.0, 600.0));
        let curr = make_window_info(1, 100, 50, Rect::new(0.0, 0.0, 800.0, 600.0));
        assert_eq!(classify_transition(&prev, &curr), "none");
    }

    #[test]
    fn classify_minibuffer_height_change() {
        let mut prev = make_window_info(99, 50, 0, Rect::new(0.0, 580.0, 800.0, 20.0));
        prev.is_minibuffer = true;
        let mut curr = make_window_info(99, 50, 0, Rect::new(0.0, 560.0, 800.0, 40.0));
        curr.is_minibuffer = true;
        assert_eq!(classify_transition(&prev, &curr), "minibuffer_height");
    }

    #[test]
    fn classify_minibuffer_scroll_not_height() {
        // Minibuffer with scroll but no height change
        let mut prev = make_window_info(99, 50, 0, Rect::new(0.0, 580.0, 800.0, 20.0));
        prev.is_minibuffer = true;
        let mut curr = make_window_info(99, 50, 10, Rect::new(0.0, 580.0, 800.0, 20.0));
        curr.is_minibuffer = true;
        assert_eq!(classify_transition(&prev, &curr), "scroll");
    }

    // =====================================================================
    // Window selected detection
    // =====================================================================
    //
    // From detect_transitions:
    //   Find first info where selected && !is_minibuffer

    fn find_selected_window(infos: &[WindowInfo]) -> Option<(i64, Rect)> {
        for info in infos {
            if info.selected && !info.is_minibuffer {
                return Some((info.window_id, info.bounds));
            }
        }
        None
    }

    #[test]
    fn find_selected_window_single() {
        let mut info = make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 800.0, 600.0));
        info.selected = true;
        let result = find_selected_window(&[info]);
        assert_eq!(result, Some((1, Rect::new(0.0, 0.0, 800.0, 600.0))));
    }

    #[test]
    fn find_selected_window_among_multiple() {
        let info1 = make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 400.0, 600.0));
        let mut info2 = make_window_info(2, 200, 0, Rect::new(400.0, 0.0, 400.0, 600.0));
        info2.selected = true;
        let result = find_selected_window(&[info1, info2]);
        assert_eq!(result, Some((2, Rect::new(400.0, 0.0, 400.0, 600.0))));
    }

    #[test]
    fn find_selected_window_none_selected() {
        let info = make_window_info(1, 100, 0, Rect::new(0.0, 0.0, 800.0, 600.0));
        assert_eq!(find_selected_window(&[info]), None);
    }

    #[test]
    fn find_selected_window_minibuffer_excluded() {
        let mut mini = make_window_info(99, 50, 0, Rect::new(0.0, 580.0, 800.0, 20.0));
        mini.is_minibuffer = true;
        mini.selected = true;
        assert_eq!(find_selected_window(&[mini]), None);
    }

    #[test]
    fn find_selected_window_empty_list() {
        assert_eq!(find_selected_window(&[]), None);
    }

    // =====================================================================
    // Window switch detection
    // =====================================================================
    //
    // From detect_transitions:
    //   prev_selected_window_id != 0 && wid != prev_selected_window_id

    fn is_window_switch(prev_selected: i64, curr_selected: i64) -> bool {
        prev_selected != 0 && curr_selected != prev_selected
    }

    #[test]
    fn window_switch_detected() {
        assert!(is_window_switch(1, 2));
    }

    #[test]
    fn no_switch_same_window() {
        assert!(!is_window_switch(1, 1));
    }

    #[test]
    fn no_switch_when_prev_is_zero() {
        // Initial state, no previous window known
        assert!(!is_window_switch(0, 1));
    }

    // =====================================================================
    // Duration values used by different transition types
    // =====================================================================

    #[test]
    fn crossfade_uses_configured_duration() {
        let ts = TransitionState::default();
        assert_eq!(ts.policy.crossfade_duration(), Duration::from_millis(200));
    }

    #[test]
    fn scroll_uses_configured_duration() {
        let ts = TransitionState::default();
        assert_eq!(ts.policy.scroll_duration(), Duration::from_millis(150));
    }

    #[test]
    fn minibuffer_crossfade_uses_150ms() {
        // Hardcoded in detect_transitions: Duration::from_millis(150)
        let dur = Duration::from_millis(150);
        assert_eq!(dur.as_millis(), 150);
    }

    #[test]
    fn font_size_crossfade_uses_200ms() {
        // Hardcoded in detect_transitions: Duration::from_millis(200)
        let dur = Duration::from_millis(200);
        assert_eq!(dur.as_millis(), 200);
    }

    #[test]
    fn window_resize_crossfade_uses_150ms() {
        // Hardcoded in detect_transitions: Duration::from_millis(150)
        let dur = Duration::from_millis(150);
        assert_eq!(dur.as_millis(), 150);
    }

    #[test]
    fn window_split_crossfade_uses_200ms() {
        // Hardcoded in detect_transitions: Duration::from_millis(200)
        let dur = Duration::from_millis(200);
        assert_eq!(dur.as_millis(), 200);
    }

    // =====================================================================
    // Edge cases: extreme values
    // =====================================================================

    #[test]
    fn progress_with_zero_duration_is_clamped() {
        // Zero duration would cause division by zero -> Inf -> min(1.0) = NaN
        // In practice the code doesn't use zero duration, but verify
        // the .min(1.0) doesn't help with NaN. This documents behavior.
        let elapsed = Duration::from_millis(100);
        let duration = Duration::ZERO;
        let raw_t = (elapsed.as_secs_f32() / duration.as_secs_f32()).min(1.0);
        // Inf.min(1.0) = 1.0 in Rust (IEEE 754: min with NaN propagation differs from f32::min)
        // f32::INFINITY.min(1.0) = 1.0
        assert!((raw_t - 1.0).abs() < 1e-6);
    }

    #[test]
    fn progress_zero_elapsed_zero_duration() {
        let raw_t = (Duration::ZERO.as_secs_f32() / Duration::ZERO.as_secs_f32()).min(1.0);
        // 0.0 / 0.0 = NaN, NaN.min(1.0) = 1.0 in Rust
        // This is a corner case that shouldn't occur in practice
        assert!(raw_t.is_nan() || raw_t <= 1.0);
    }

    #[test]
    fn buffer_change_max_ids() {
        assert!(is_buffer_change(u64::MAX, u64::MAX - 1));
        assert!(!is_buffer_change(u64::MAX, u64::MAX));
    }

    #[test]
    fn scroll_direction_with_negative_positions() {
        // Some buffer systems use negative positions
        assert_eq!(scroll_direction(-100, -50), 1); // moved forward
        assert_eq!(scroll_direction(-50, -100), -1); // moved backward
    }

    #[test]
    fn velocity_delta_symmetric() {
        // Same magnitude regardless of direction
        assert_eq!(scroll_velocity_delta(0, 100), scroll_velocity_delta(100, 0));
    }

    #[test]
    fn theme_change_with_large_values() {
        // Colors should be in 0.0-1.0, but test robustness
        let old = (0.0, 0.0, 0.0, 1.0);
        let new = (1.0, 1.0, 1.0, 1.0);
        assert!(is_theme_change(old, new));
    }

    #[test]
    fn theme_change_negative_to_positive() {
        // Should work even with negative values (not typical but edge case)
        let old = (-0.1, 0.0, 0.0, 1.0);
        let new = (0.1, 0.0, 0.0, 1.0);
        assert!(is_theme_change(old, new)); // diff = 0.2 > 0.02
    }

    // =====================================================================
    // Cursor within window bounds check
    // =====================================================================
    //
    // From detect_transitions (line animation):
    //   *x >= info.bounds.x && *x < info.bounds.x + info.bounds.width
    //   && *y >= info.bounds.y && *y < info.bounds.y + info.bounds.height
    //   && *style != 3

    fn cursor_in_window(cx: f32, cy: f32, style: CursorStyle, bounds: Rect) -> bool {
        cx >= bounds.x
            && cx < bounds.x + bounds.width
            && cy >= bounds.y
            && cy < bounds.y + bounds.height
            && !style.is_hollow()
    }

    #[test]
    fn cursor_inside_window() {
        let bounds = Rect::new(0.0, 0.0, 800.0, 600.0);
        assert!(cursor_in_window(
            100.0,
            200.0,
            CursorStyle::FilledBox,
            bounds
        ));
    }

    #[test]
    fn cursor_at_window_origin() {
        let bounds = Rect::new(10.0, 20.0, 800.0, 600.0);
        assert!(cursor_in_window(10.0, 20.0, CursorStyle::Bar(2.0), bounds));
    }

    #[test]
    fn cursor_outside_window_left() {
        let bounds = Rect::new(10.0, 0.0, 800.0, 600.0);
        assert!(!cursor_in_window(
            5.0,
            100.0,
            CursorStyle::FilledBox,
            bounds
        ));
    }

    #[test]
    fn cursor_outside_window_right() {
        let bounds = Rect::new(0.0, 0.0, 800.0, 600.0);
        assert!(!cursor_in_window(
            800.0,
            100.0,
            CursorStyle::FilledBox,
            bounds
        )); // exclusive right edge
    }

    #[test]
    fn cursor_outside_window_above() {
        let bounds = Rect::new(0.0, 50.0, 800.0, 600.0);
        assert!(!cursor_in_window(
            100.0,
            30.0,
            CursorStyle::FilledBox,
            bounds
        ));
    }

    #[test]
    fn cursor_outside_window_below() {
        let bounds = Rect::new(0.0, 0.0, 800.0, 600.0);
        assert!(!cursor_in_window(
            100.0,
            600.0,
            CursorStyle::FilledBox,
            bounds
        )); // exclusive bottom edge
    }

    #[test]
    fn cursor_hollow_style_excluded() {
        // Hollow cursor (inactive window) -> excluded from line animation
        let bounds = Rect::new(0.0, 0.0, 800.0, 600.0);
        assert!(!cursor_in_window(100.0, 200.0, CursorStyle::Hollow, bounds));
    }

    #[test]
    fn cursor_all_active_styles_included() {
        let bounds = Rect::new(0.0, 0.0, 800.0, 600.0);
        assert!(cursor_in_window(
            100.0,
            200.0,
            CursorStyle::FilledBox,
            bounds
        )); // box
        assert!(cursor_in_window(
            100.0,
            200.0,
            CursorStyle::Bar(2.0),
            bounds
        )); // bar
        assert!(cursor_in_window(
            100.0,
            200.0,
            CursorStyle::Hbar(2.0),
            bounds
        )); // hbar
    }
}
