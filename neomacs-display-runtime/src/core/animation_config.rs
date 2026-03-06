//! Animation configuration system.
//!
//! Provides user-configurable animation settings that can be controlled
//! from Emacs Lisp via `setq` or `customize`.

use crate::core::buffer_transition::BufferTransitionEffect;
use crate::core::cursor_animation::CursorAnimationMode;
use neomacs_display_protocol::{ScrollEasing, ScrollEffect};
use std::time::Duration;

/// Master animation configuration
#[derive(Debug, Clone)]
pub struct AnimationConfig {
    /// Master switch - disable all animations
    pub enabled: bool,

    /// Cursor animation settings
    pub cursor: CursorAnimationConfig,

    /// Buffer transition settings
    pub buffer_transition: BufferTransitionConfig,

    /// Scroll animation settings
    pub scroll: ScrollAnimationConfig,
}

impl Default for AnimationConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Disabled by default - user opts in
            cursor: CursorAnimationConfig::default(),
            buffer_transition: BufferTransitionConfig::default(),
            scroll: ScrollAnimationConfig::default(),
        }
    }
}

impl AnimationConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable all animations with sensible defaults
    pub fn enable_all(&mut self) {
        self.enabled = true;
        self.cursor.enabled = true;
        self.buffer_transition.enabled = true;
        self.scroll.enabled = true;
    }

    /// Disable all animations
    pub fn disable_all(&mut self) {
        self.enabled = false;
    }

    /// Check if cursor animation should run
    pub fn cursor_animation_active(&self) -> bool {
        self.enabled && self.cursor.enabled
    }

    /// Check if buffer transition should run
    pub fn buffer_transition_active(&self) -> bool {
        self.enabled && self.buffer_transition.enabled
    }

    /// Check if scroll animation should run
    pub fn scroll_animation_active(&self) -> bool {
        self.enabled && self.scroll.enabled
    }
}

/// Cursor animation configuration
#[derive(Debug, Clone)]
pub struct CursorAnimationConfig {
    /// Enable cursor animation
    pub enabled: bool,

    /// Animation mode/style
    pub mode: CursorAnimationMode,

    /// Animation speed (higher = faster, 1-100)
    pub speed: f32,

    /// Enable cursor glow effect
    pub glow: bool,

    /// Glow intensity (0.0 - 1.0)
    pub glow_intensity: f32,

    /// Particle count for particle effects
    pub particle_count: u32,

    /// Particle trail length
    pub trail_length: u32,
}

impl Default for CursorAnimationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: CursorAnimationMode::Smooth, // Just smooth movement by default
            speed: 15.0,
            glow: false,
            glow_intensity: 0.3,
            particle_count: 15,
            trail_length: 40,
        }
    }
}

/// Buffer transition configuration
#[derive(Debug, Clone)]
pub struct BufferTransitionConfig {
    /// Enable buffer switch animations
    pub enabled: bool,

    /// Transition effect type
    pub effect: BufferTransitionEffect,

    /// Transition duration in milliseconds
    pub duration_ms: u32,

    /// Auto-detect buffer switches (vs explicit trigger)
    pub auto_detect: bool,
}

impl Default for BufferTransitionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            effect: BufferTransitionEffect::Crossfade,
            duration_ms: 200,
            auto_detect: true,
        }
    }
}

impl BufferTransitionConfig {
    pub fn duration(&self) -> Duration {
        Duration::from_millis(self.duration_ms as u64)
    }
}

/// Scroll animation configuration
#[derive(Debug, Clone)]
pub struct ScrollAnimationConfig {
    /// Enable smooth scrolling
    pub enabled: bool,

    /// Scroll animation duration in milliseconds
    pub duration_ms: u32,

    /// Lines to scroll before animation kicks in (1 = always animate)
    pub threshold_lines: u32,

    /// Visual effect for scroll transitions
    pub effect: ScrollEffect,

    /// Easing/physics model for scroll timing
    pub easing: ScrollEasing,
}

impl Default for ScrollAnimationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            duration_ms: 150,
            threshold_lines: 1,
            effect: ScrollEffect::default(),
            easing: ScrollEasing::default(),
        }
    }
}

/// Builder for animation config from string options
impl AnimationConfig {
    /// Set option by name (for Lisp integration)
    /// Returns true if option was recognized
    pub fn set_option(&mut self, name: &str, value: &str) -> bool {
        match name {
            // Master switch
            "animation" | "animations" => {
                self.enabled = parse_bool(value);
                true
            }

            // Cursor options
            "cursor-animation" => {
                self.cursor.enabled = parse_bool(value);
                true
            }
            "cursor-animation-mode" | "cursor-animation-style" => {
                self.cursor.mode = CursorAnimationMode::from_str(value);
                true
            }
            "cursor-animation-speed" => {
                if let Ok(v) = value.parse::<f32>() {
                    self.cursor.speed = v.clamp(1.0, 100.0);
                }
                true
            }
            "cursor-glow" => {
                self.cursor.glow = parse_bool(value);
                true
            }
            "cursor-glow-intensity" => {
                if let Ok(v) = value.parse::<f32>() {
                    self.cursor.glow_intensity = v.clamp(0.0, 1.0);
                }
                true
            }
            "cursor-particle-count" => {
                if let Ok(v) = value.parse::<u32>() {
                    self.cursor.particle_count = v.clamp(1, 100);
                }
                true
            }

            // Buffer transition options
            "buffer-transition" | "buffer-switch-animation" => {
                self.buffer_transition.enabled = parse_bool(value);
                true
            }
            "buffer-transition-effect" | "buffer-transition-style" => {
                self.buffer_transition.effect = BufferTransitionEffect::from_str(value);
                true
            }
            "buffer-transition-duration" => {
                if let Ok(v) = value.parse::<u32>() {
                    self.buffer_transition.duration_ms = v.clamp(50, 1000);
                }
                true
            }

            // Scroll options
            "scroll-animation" | "smooth-scroll" => {
                self.scroll.enabled = parse_bool(value);
                true
            }
            "scroll-animation-duration" => {
                if let Ok(v) = value.parse::<u32>() {
                    self.scroll.duration_ms = v.clamp(50, 500);
                }
                true
            }
            "scroll-effect" | "scroll-animation-effect" | "scroll-style" => {
                self.scroll.effect = ScrollEffect::from_str(value);
                true
            }
            "scroll-easing" | "scroll-animation-easing" => {
                self.scroll.easing = ScrollEasing::from_str(value);
                true
            }

            _ => false,
        }
    }

    /// Get option value as string (for Lisp integration)
    pub fn get_option(&self, name: &str) -> Option<String> {
        match name {
            "animation" | "animations" => Some(bool_str(self.enabled)),
            "cursor-animation" => Some(bool_str(self.cursor.enabled)),
            "cursor-animation-mode" => Some(format!("{:?}", self.cursor.mode).to_lowercase()),
            "cursor-animation-speed" => Some(self.cursor.speed.to_string()),
            "cursor-glow" => Some(bool_str(self.cursor.glow)),
            "buffer-transition" => Some(bool_str(self.buffer_transition.enabled)),
            "buffer-transition-effect" => {
                Some(format!("{:?}", self.buffer_transition.effect).to_lowercase())
            }
            "buffer-transition-duration" => Some(self.buffer_transition.duration_ms.to_string()),
            "scroll-animation" => Some(bool_str(self.scroll.enabled)),
            "scroll-effect" => Some(self.scroll.effect.as_str().to_string()),
            "scroll-easing" => Some(self.scroll.easing.as_str().to_string()),
            _ => None,
        }
    }
}

fn parse_bool(s: &str) -> bool {
    matches!(s.to_lowercase().as_str(), "t" | "true" | "1" | "yes" | "on")
}

fn bool_str(b: bool) -> String {
    if b {
        "t".to_string()
    } else {
        "nil".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_disabled() {
        let config = AnimationConfig::default();
        assert!(!config.enabled);
    }

    #[test]
    fn test_enable_all() {
        let mut config = AnimationConfig::default();
        config.enable_all();
        assert!(config.enabled);
        assert!(config.cursor.enabled);
        assert!(config.buffer_transition.enabled);
    }

    #[test]
    fn test_set_option() {
        let mut config = AnimationConfig::default();

        assert!(config.set_option("animation", "t"));
        assert!(config.enabled);

        assert!(config.set_option("cursor-animation-mode", "railgun"));
        assert_eq!(config.cursor.mode, CursorAnimationMode::Railgun);

        assert!(config.set_option("buffer-transition-effect", "page-curl"));
        assert_eq!(
            config.buffer_transition.effect,
            BufferTransitionEffect::PageCurl
        );
    }

    // ── Default value tests ──────────────────────────────────────────────

    #[test]
    fn test_animation_config_default_values() {
        let config = AnimationConfig::default();
        assert!(!config.enabled);
        // Sub-configs are enabled by default (master switch gates them)
        assert!(config.cursor.enabled);
        assert!(config.buffer_transition.enabled);
        assert!(config.scroll.enabled);
    }

    #[test]
    fn test_cursor_animation_config_defaults() {
        let cursor = CursorAnimationConfig::default();
        assert!(cursor.enabled);
        assert_eq!(cursor.mode, CursorAnimationMode::Smooth);
        assert_eq!(cursor.speed, 15.0);
        assert!(!cursor.glow);
        assert_eq!(cursor.glow_intensity, 0.3);
        assert_eq!(cursor.particle_count, 15);
        assert_eq!(cursor.trail_length, 40);
    }

    #[test]
    fn test_buffer_transition_config_defaults() {
        let bt = BufferTransitionConfig::default();
        assert!(bt.enabled);
        assert_eq!(bt.effect, BufferTransitionEffect::Crossfade);
        assert_eq!(bt.duration_ms, 200);
        assert!(bt.auto_detect);
    }

    #[test]
    fn test_scroll_animation_config_defaults() {
        let scroll = ScrollAnimationConfig::default();
        assert!(scroll.enabled);
        assert_eq!(scroll.duration_ms, 150);
        assert_eq!(scroll.threshold_lines, 1);
        assert_eq!(scroll.effect, ScrollEffect::Slide);
        assert_eq!(scroll.easing, ScrollEasing::EaseOutQuad);
    }

    #[test]
    fn test_new_equals_default() {
        let from_new = AnimationConfig::new();
        let from_default = AnimationConfig::default();
        assert_eq!(from_new.enabled, from_default.enabled);
        assert_eq!(from_new.cursor.enabled, from_default.cursor.enabled);
        assert_eq!(from_new.cursor.speed, from_default.cursor.speed);
        assert_eq!(
            from_new.buffer_transition.duration_ms,
            from_default.buffer_transition.duration_ms
        );
        assert_eq!(from_new.scroll.duration_ms, from_default.scroll.duration_ms);
    }

    // ── Active checks (master + sub-switch gating) ──────────────────────

    #[test]
    fn test_cursor_animation_active_requires_both_switches() {
        let mut config = AnimationConfig::default();
        // Master off, cursor on => inactive
        assert!(!config.cursor_animation_active());

        // Master on, cursor on => active
        config.enabled = true;
        assert!(config.cursor_animation_active());

        // Master on, cursor off => inactive
        config.cursor.enabled = false;
        assert!(!config.cursor_animation_active());
    }

    #[test]
    fn test_buffer_transition_active_requires_both_switches() {
        let mut config = AnimationConfig::default();
        assert!(!config.buffer_transition_active());

        config.enabled = true;
        assert!(config.buffer_transition_active());

        config.buffer_transition.enabled = false;
        assert!(!config.buffer_transition_active());
    }

    #[test]
    fn test_scroll_animation_active_requires_both_switches() {
        let mut config = AnimationConfig::default();
        assert!(!config.scroll_animation_active());

        config.enabled = true;
        assert!(config.scroll_animation_active());

        config.scroll.enabled = false;
        assert!(!config.scroll_animation_active());
    }

    // ── enable_all / disable_all ────────────────────────────────────────

    #[test]
    fn test_disable_all_sets_master_off() {
        let mut config = AnimationConfig::default();
        config.enable_all();
        assert!(config.enabled);
        assert!(config.cursor_animation_active());

        config.disable_all();
        assert!(!config.enabled);
        // Sub-switches remain on, but active checks return false
        assert!(config.cursor.enabled);
        assert!(config.buffer_transition.enabled);
        assert!(config.scroll.enabled);
        assert!(!config.cursor_animation_active());
        assert!(!config.buffer_transition_active());
        assert!(!config.scroll_animation_active());
    }

    // ── BufferTransitionConfig::duration() ──────────────────────────────

    #[test]
    fn test_buffer_transition_duration_conversion() {
        let bt = BufferTransitionConfig::default();
        assert_eq!(bt.duration(), Duration::from_millis(200));

        let mut bt2 = BufferTransitionConfig::default();
        bt2.duration_ms = 500;
        assert_eq!(bt2.duration(), Duration::from_millis(500));

        let mut bt3 = BufferTransitionConfig::default();
        bt3.duration_ms = 0;
        assert_eq!(bt3.duration(), Duration::from_millis(0));
    }

    // ── set_option: boolean parsing variants ────────────────────────────

    #[test]
    fn test_set_option_bool_truthy_values() {
        for val in &["t", "true", "1", "yes", "on", "True", "TRUE", "Yes", "ON"] {
            let mut config = AnimationConfig::default();
            config.set_option("animation", val);
            assert!(config.enabled, "Expected '{}' to be truthy", val);
        }
    }

    #[test]
    fn test_set_option_bool_falsy_values() {
        for val in &["nil", "false", "0", "no", "off", "", "anything-else"] {
            let mut config = AnimationConfig::default();
            config.enable_all();
            config.set_option("animation", val);
            assert!(!config.enabled, "Expected '{}' to be falsy", val);
        }
    }

    // ── set_option: clamping / boundary values ──────────────────────────

    #[test]
    fn test_cursor_speed_clamped_to_range() {
        let mut config = AnimationConfig::default();

        config.set_option("cursor-animation-speed", "0");
        assert_eq!(
            config.cursor.speed, 1.0,
            "Below minimum should clamp to 1.0"
        );

        config.set_option("cursor-animation-speed", "-10");
        assert_eq!(config.cursor.speed, 1.0, "Negative should clamp to 1.0");

        config.set_option("cursor-animation-speed", "200");
        assert_eq!(
            config.cursor.speed, 100.0,
            "Above maximum should clamp to 100.0"
        );

        config.set_option("cursor-animation-speed", "50");
        assert_eq!(config.cursor.speed, 50.0, "Within range should be exact");

        config.set_option("cursor-animation-speed", "1");
        assert_eq!(config.cursor.speed, 1.0, "Minimum boundary");

        config.set_option("cursor-animation-speed", "100");
        assert_eq!(config.cursor.speed, 100.0, "Maximum boundary");
    }

    #[test]
    fn test_glow_intensity_clamped_to_range() {
        let mut config = AnimationConfig::default();

        config.set_option("cursor-glow-intensity", "-0.5");
        assert_eq!(config.cursor.glow_intensity, 0.0);

        config.set_option("cursor-glow-intensity", "1.5");
        assert_eq!(config.cursor.glow_intensity, 1.0);

        config.set_option("cursor-glow-intensity", "0.0");
        assert_eq!(config.cursor.glow_intensity, 0.0);

        config.set_option("cursor-glow-intensity", "1.0");
        assert_eq!(config.cursor.glow_intensity, 1.0);

        config.set_option("cursor-glow-intensity", "0.7");
        assert_eq!(config.cursor.glow_intensity, 0.7);
    }

    #[test]
    fn test_particle_count_clamped_to_range() {
        let mut config = AnimationConfig::default();

        config.set_option("cursor-particle-count", "0");
        assert_eq!(config.cursor.particle_count, 1, "Below minimum clamps to 1");

        config.set_option("cursor-particle-count", "200");
        assert_eq!(
            config.cursor.particle_count, 100,
            "Above maximum clamps to 100"
        );

        config.set_option("cursor-particle-count", "50");
        assert_eq!(config.cursor.particle_count, 50);
    }

    #[test]
    fn test_buffer_transition_duration_clamped() {
        let mut config = AnimationConfig::default();

        config.set_option("buffer-transition-duration", "10");
        assert_eq!(
            config.buffer_transition.duration_ms, 50,
            "Below minimum clamps to 50"
        );

        config.set_option("buffer-transition-duration", "5000");
        assert_eq!(
            config.buffer_transition.duration_ms, 1000,
            "Above maximum clamps to 1000"
        );

        config.set_option("buffer-transition-duration", "300");
        assert_eq!(config.buffer_transition.duration_ms, 300);
    }

    #[test]
    fn test_scroll_duration_clamped() {
        let mut config = AnimationConfig::default();

        config.set_option("scroll-animation-duration", "10");
        assert_eq!(config.scroll.duration_ms, 50, "Below minimum clamps to 50");

        config.set_option("scroll-animation-duration", "9999");
        assert_eq!(
            config.scroll.duration_ms, 500,
            "Above maximum clamps to 500"
        );

        config.set_option("scroll-animation-duration", "250");
        assert_eq!(config.scroll.duration_ms, 250);
    }

    // ── set_option: invalid parse leaves value unchanged ────────────────

    #[test]
    fn test_set_option_invalid_number_leaves_value_unchanged() {
        let mut config = AnimationConfig::default();
        let original_speed = config.cursor.speed;

        config.set_option("cursor-animation-speed", "not-a-number");
        assert_eq!(
            config.cursor.speed, original_speed,
            "Non-numeric input should not change speed"
        );

        config.set_option("cursor-animation-speed", "");
        assert_eq!(
            config.cursor.speed, original_speed,
            "Empty input should not change speed"
        );
    }

    // ── set_option: unknown option returns false ────────────────────────

    #[test]
    fn test_set_option_unknown_returns_false() {
        let mut config = AnimationConfig::default();
        assert!(!config.set_option("nonexistent-option", "42"));
        assert!(!config.set_option("", "t"));
        assert!(!config.set_option("cursor-color", "red"));
    }

    // ── set_option: alias names ─────────────────────────────────────────

    #[test]
    fn test_set_option_alias_names() {
        let mut config = AnimationConfig::default();

        // "animations" is an alias for "animation"
        config.set_option("animations", "t");
        assert!(config.enabled);

        // "buffer-switch-animation" is an alias for "buffer-transition"
        config.set_option("buffer-switch-animation", "false");
        assert!(!config.buffer_transition.enabled);

        // "smooth-scroll" is an alias for "scroll-animation"
        config.set_option("smooth-scroll", "nil");
        assert!(!config.scroll.enabled);

        // "cursor-animation-style" is an alias for "cursor-animation-mode"
        config.set_option("cursor-animation-style", "torpedo");
        assert_eq!(config.cursor.mode, CursorAnimationMode::Torpedo);

        // "buffer-transition-style" alias
        config.set_option("buffer-transition-style", "blur");
        assert_eq!(
            config.buffer_transition.effect,
            BufferTransitionEffect::Blur
        );

        // scroll effect aliases
        config.set_option("scroll-animation-effect", "crossfade");
        assert_eq!(config.scroll.effect, ScrollEffect::Crossfade);
        config.set_option("scroll-style", "cascade");
        assert_eq!(config.scroll.effect, ScrollEffect::Cascade);

        // scroll easing aliases
        config.set_option("scroll-animation-easing", "spring");
        assert_eq!(config.scroll.easing, ScrollEasing::Spring);
    }

    // ── get_option ──────────────────────────────────────────────────────

    #[test]
    fn test_get_option_returns_correct_values() {
        let config = AnimationConfig::default();

        assert_eq!(config.get_option("animation"), Some("nil".to_string()));
        assert_eq!(config.get_option("cursor-animation"), Some("t".to_string()));
        assert_eq!(
            config.get_option("cursor-animation-speed"),
            Some("15".to_string())
        );
        assert_eq!(config.get_option("cursor-glow"), Some("nil".to_string()));
        assert_eq!(
            config.get_option("buffer-transition"),
            Some("t".to_string())
        );
        assert_eq!(
            config.get_option("buffer-transition-duration"),
            Some("200".to_string())
        );
        assert_eq!(config.get_option("scroll-animation"), Some("t".to_string()));
        assert_eq!(
            config.get_option("scroll-effect"),
            Some("slide".to_string())
        );
        assert_eq!(
            config.get_option("scroll-easing"),
            Some("ease-out-quad".to_string())
        );
    }

    #[test]
    fn test_get_option_unknown_returns_none() {
        let config = AnimationConfig::default();
        assert_eq!(config.get_option("nonexistent"), None);
        assert_eq!(config.get_option(""), None);
    }

    #[test]
    fn test_get_option_reflects_set_option() {
        let mut config = AnimationConfig::default();

        config.set_option("animation", "t");
        assert_eq!(config.get_option("animation"), Some("t".to_string()));

        config.set_option("cursor-animation-mode", "torpedo");
        assert_eq!(
            config.get_option("cursor-animation-mode"),
            Some("torpedo".to_string())
        );

        config.set_option("buffer-transition-effect", "slide-left");
        assert_eq!(
            config.get_option("buffer-transition-effect"),
            Some("slideleft".to_string())
        );

        config.set_option("scroll-effect", "parallax");
        assert_eq!(
            config.get_option("scroll-effect"),
            Some("parallax".to_string())
        );

        config.set_option("scroll-easing", "spring");
        assert_eq!(
            config.get_option("scroll-easing"),
            Some("spring".to_string())
        );
    }

    // ── set_option: all cursor animation modes ──────────────────────────

    #[test]
    fn test_set_all_cursor_modes() {
        let modes = [
            ("none", CursorAnimationMode::None),
            ("smooth", CursorAnimationMode::Smooth),
            ("railgun", CursorAnimationMode::Railgun),
            ("torpedo", CursorAnimationMode::Torpedo),
            ("pixiedust", CursorAnimationMode::Pixiedust),
            ("sonicboom", CursorAnimationMode::Sonicboom),
            ("ripple", CursorAnimationMode::Ripple),
            ("wireframe", CursorAnimationMode::Wireframe),
        ];
        for (name, expected) in &modes {
            let mut config = AnimationConfig::default();
            config.set_option("cursor-animation-mode", name);
            assert_eq!(
                config.cursor.mode, *expected,
                "Mode '{}' should parse correctly",
                name
            );
        }
    }

    // ── set_option: all buffer transition effects ───────────────────────

    #[test]
    fn test_set_all_buffer_transition_effects() {
        let effects = [
            ("none", BufferTransitionEffect::None),
            ("crossfade", BufferTransitionEffect::Crossfade),
            ("slide-left", BufferTransitionEffect::SlideLeft),
            ("slide-right", BufferTransitionEffect::SlideRight),
            ("slide-up", BufferTransitionEffect::SlideUp),
            ("slide-down", BufferTransitionEffect::SlideDown),
            ("scale-fade", BufferTransitionEffect::ScaleFade),
            ("push", BufferTransitionEffect::Push),
            ("blur", BufferTransitionEffect::Blur),
            ("page-curl", BufferTransitionEffect::PageCurl),
        ];
        for (name, expected) in &effects {
            let mut config = AnimationConfig::default();
            config.set_option("buffer-transition-effect", name);
            assert_eq!(
                config.buffer_transition.effect, *expected,
                "Effect '{}' should parse correctly",
                name
            );
        }
    }

    // ── set_option: all scroll effects ──────────────────────────────────

    #[test]
    fn test_set_all_scroll_effects() {
        let effects = [
            ("slide", ScrollEffect::Slide),
            ("crossfade", ScrollEffect::Crossfade),
            ("cascade", ScrollEffect::Cascade),
            ("parallax", ScrollEffect::Parallax),
        ];
        for (name, expected) in &effects {
            let mut config = AnimationConfig::default();
            config.set_option("scroll-effect", name);
            assert_eq!(
                config.scroll.effect, *expected,
                "Scroll effect '{}' should parse correctly",
                name
            );
        }
    }

    // ── set_option: all scroll easings ──────────────────────────────────

    #[test]
    fn test_set_all_scroll_easings() {
        let easings = [
            ("ease-out", ScrollEasing::EaseOutQuad),
            ("ease-out-quad", ScrollEasing::EaseOutQuad),
            ("ease-out-cubic", ScrollEasing::EaseOutCubic),
            ("spring", ScrollEasing::Spring),
            ("linear", ScrollEasing::Linear),
            ("ease-in-out", ScrollEasing::EaseInOutCubic),
        ];
        for (name, expected) in &easings {
            let mut config = AnimationConfig::default();
            config.set_option("scroll-easing", name);
            assert_eq!(
                config.scroll.easing, *expected,
                "Scroll easing '{}' should parse correctly",
                name
            );
        }
    }

    // ── parse_bool / bool_str helpers ───────────────────────────────────

    #[test]
    fn test_parse_bool_and_bool_str() {
        // parse_bool truthy
        assert!(parse_bool("t"));
        assert!(parse_bool("true"));
        assert!(parse_bool("1"));
        assert!(parse_bool("yes"));
        assert!(parse_bool("on"));
        assert!(parse_bool("TRUE"));
        assert!(parse_bool("On"));

        // parse_bool falsy
        assert!(!parse_bool("nil"));
        assert!(!parse_bool("false"));
        assert!(!parse_bool("0"));
        assert!(!parse_bool("no"));
        assert!(!parse_bool("off"));
        assert!(!parse_bool(""));
        assert!(!parse_bool("random"));

        // bool_str
        assert_eq!(bool_str(true), "t");
        assert_eq!(bool_str(false), "nil");
    }

    // ── Clone trait ─────────────────────────────────────────────────────

    #[test]
    fn test_config_clone_is_independent() {
        let mut config = AnimationConfig::default();
        config.enable_all();
        config.cursor.speed = 42.0;

        let mut cloned = config.clone();
        cloned.cursor.speed = 99.0;
        cloned.disable_all();

        // Original should be unaffected
        assert!(config.enabled);
        assert_eq!(config.cursor.speed, 42.0);
    }
}
