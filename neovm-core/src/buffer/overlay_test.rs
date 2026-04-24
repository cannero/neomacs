use super::*;
use crate::buffer::BufferId;

fn alloc_overlay(start: usize, end: usize) -> Value {
    Value::make_overlay(OverlayData {
        plist: Value::NIL,
        buffer: Some(BufferId(1)),
        start,
        end,
        front_advance: false,
        rear_advance: false,
    })
}

#[test]
fn insert_and_delete_overlay_preserves_object_identity() {
    crate::test_utils::init_test_tracing();
    let mut list = OverlayList::new();
    let overlay = alloc_overlay(2, 5);
    list.insert_overlay(overlay);
    assert_eq!(list.overlays_at(3), vec![overlay]);
    assert!(list.delete_overlay(overlay));
    assert!(list.overlays_at(3).is_empty());
    assert!(overlay_live_buffer(overlay).is_none());
}

#[test]
fn overlay_put_preserves_existing_property_position() {
    crate::test_utils::init_test_tracing();
    let mut list = OverlayList::new();
    let overlay = alloc_overlay(0, 1);
    list.insert_overlay(overlay);
    let face = Value::symbol("face");
    let help = Value::symbol("help-echo");
    list.overlay_put(overlay, face, Value::symbol("bold"))
        .unwrap();
    list.overlay_put(overlay, help, Value::string("tip"))
        .unwrap();
    list.overlay_put(overlay, face, Value::symbol("italic"))
        .unwrap();
    let plist = list.overlay_plist(overlay).unwrap();
    assert_eq!(
        crate::emacs_core::print::print_value(&plist),
        "(face italic help-echo \"tip\")"
    );
}

#[test]
fn move_overlay_updates_boundaries() {
    crate::test_utils::init_test_tracing();
    let mut list = OverlayList::new();
    let overlay = alloc_overlay(0, 2);
    list.insert_overlay(overlay);
    list.move_overlay(overlay, 4, 7);
    assert_eq!(list.overlay_start(overlay), Some(4));
    assert_eq!(list.overlay_end(overlay), Some(7));
    assert_eq!(list.overlays_at(5), vec![overlay]);
}

#[test]
fn move_overlay_evaporates_zero_width_overlay() {
    crate::test_utils::init_test_tracing();
    let mut list = OverlayList::new();
    let overlay = alloc_overlay(2, 5);
    list.insert_overlay(overlay);
    list.overlay_put(overlay, Value::symbol("evaporate"), Value::T)
        .unwrap();
    list.move_overlay(overlay, 4, 4);
    assert!(list.is_empty());
    assert!(overlay_live_buffer(overlay).is_none());
}

#[test]
fn insert_adjusts_front_and_rear_advance() {
    crate::test_utils::init_test_tracing();
    let mut list = OverlayList::new();
    let overlay = alloc_overlay(5, 10);
    list.insert_overlay(overlay);
    list.set_front_advance(overlay, true);
    list.set_rear_advance(overlay, true);
    list.adjust_for_insert(5, 2, false);
    assert_eq!(list.overlay_start(overlay), Some(7));
    assert_eq!(list.overlay_end(overlay), Some(12));
}

#[test]
fn empty_front_advance_overlay_does_not_invert_on_insert() {
    crate::test_utils::init_test_tracing();
    let mut list = OverlayList::new();
    let overlay = alloc_overlay(5, 5);
    list.insert_overlay(overlay);
    list.set_front_advance(overlay, true);
    list.set_rear_advance(overlay, false);
    list.adjust_for_insert(5, 2, false);
    assert_eq!(list.overlay_start(overlay), Some(5));
    assert_eq!(list.overlay_end(overlay), Some(5));
}

#[test]
fn before_markers_insert_moves_overlay_boundaries_at_point() {
    crate::test_utils::init_test_tracing();
    let mut list = OverlayList::new();
    let starts_here = alloc_overlay(5, 10);
    let ends_here = alloc_overlay(2, 5);
    let empty = alloc_overlay(5, 5);
    list.insert_overlay(starts_here);
    list.insert_overlay(ends_here);
    list.insert_overlay(empty);
    list.adjust_for_insert(5, 2, true);
    assert_eq!(list.overlay_start(starts_here), Some(7));
    assert_eq!(list.overlay_end(starts_here), Some(12));
    assert_eq!(list.overlay_start(ends_here), Some(2));
    assert_eq!(list.overlay_end(ends_here), Some(7));
    assert_eq!(list.overlay_start(empty), Some(7));
    assert_eq!(list.overlay_end(empty), Some(7));
}

#[test]
fn delete_evaporates_zero_width_overlay() {
    crate::test_utils::init_test_tracing();
    let mut list = OverlayList::new();
    let overlay = alloc_overlay(5, 10);
    list.insert_overlay(overlay);
    list.overlay_put(overlay, Value::symbol("evaporate"), Value::T)
        .unwrap();
    list.adjust_for_delete(5, 10);
    assert!(list.is_empty());
    assert!(overlay_live_buffer(overlay).is_none());
}

#[test]
fn priority_sort_uses_gnu_precedence_rules() {
    crate::test_utils::init_test_tracing();
    let mut list = OverlayList::new();
    let low = alloc_overlay(2, 7);
    let high = alloc_overlay(4, 7);
    list.insert_overlay(low);
    list.insert_overlay(high);
    list.overlay_put(low, Value::symbol("face"), Value::symbol("bold"))
        .unwrap();
    list.overlay_put(low, Value::symbol("priority"), Value::fixnum(1))
        .unwrap();
    list.overlay_put(high, Value::symbol("face"), Value::symbol("italic"))
        .unwrap();
    list.overlay_put(
        high,
        Value::symbol("priority"),
        Value::cons(Value::fixnum(1), Value::fixnum(2)),
    )
    .unwrap();
    let mut ids = list.overlays_at(4);
    list.sort_overlay_ids_by_priority_desc(&mut ids);
    assert_eq!(ids, vec![high, low]);
}
