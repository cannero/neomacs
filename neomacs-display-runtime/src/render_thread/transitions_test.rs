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
