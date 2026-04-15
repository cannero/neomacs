use super::*;

#[test]
fn target_space_for_survivor_stays_in_nursery_below_promotion_age() {
    assert_eq!(
        target_space_for_survivor(MovePolicy::Movable, 0, 2),
        SpaceKind::Nursery
    );
}

#[test]
fn target_space_for_survivor_promotes_to_old_at_promotion_age() {
    assert_eq!(
        target_space_for_survivor(MovePolicy::Movable, 1, 2),
        SpaceKind::Old
    );
}

#[test]
fn target_space_for_survivor_honors_promote_to_pinned_policy() {
    assert_eq!(
        target_space_for_survivor(MovePolicy::PromoteToPinned, 1, 2),
        SpaceKind::Pinned
    );
}
