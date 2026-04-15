use super::*;

#[test]
fn nearest_lower_then_upper_for_static_weights() {
    let ws = [400u16, 600, 800];
    assert_eq!(pick_nearest_css_weight(&ws, 700), 600);
    assert_eq!(pick_nearest_css_weight(&ws, 850), 800);
    assert_eq!(pick_nearest_css_weight(&ws, 300), 400);
}

#[test]
fn nearest_weight_prefers_closest_match() {
    let ws = [400u16, 700];
    assert_eq!(pick_nearest_css_weight(&ws, 600), 700);
    assert_eq!(pick_nearest_css_weight(&ws, 650), 700);
    assert_eq!(pick_nearest_css_weight(&ws, 550), 400);
}

#[test]
fn variable_font_preserves_requested_weight_within_range() {
    let info = FamilyWeightInfo {
        discrete_weights: vec![400],
        variable_weight_range: Some((100, 900)),
    };
    assert_eq!(resolve_requested_weight(&info, 700), 700);
}

#[test]
fn variable_font_clamps_only_to_axis_bounds() {
    let info = FamilyWeightInfo {
        discrete_weights: vec![400],
        variable_weight_range: Some((200, 750)),
    };
    assert_eq!(resolve_requested_weight(&info, 150), 200);
    assert_eq!(resolve_requested_weight(&info, 900), 750);
}
