use super::*;

#[test]
fn basic_insert_undo() {
    crate::test_utils::init_test_tracing();
    let mut list = Value::NIL;
    undo_list_record_insert(&mut list, 0, 5, 0);
    undo_list_record_insert(&mut list, 5, 3, 5);
    undo_list_boundary(&mut list);

    // Should have: nil, (1 . 9) [merged], at minimum
    // Actually the second insert merges with the first: (1 . 9)
    assert!(undo_list_has_trailing_boundary(&list));

    let group = undo_list_pop_group(&mut list);
    assert_eq!(group.len(), 1); // merged into one entry
    let entry = group[0];
    assert!(entry.is_cons());
    assert_eq!(entry.cons_car(), Value::fixnum(1));
    assert_eq!(entry.cons_cdr(), Value::fixnum(9));
}

#[test]
fn delete_records_text() {
    crate::test_utils::init_test_tracing();
    let mut list = Value::NIL;
    undo_list_record_delete(
        &mut list,
        3,
        crate::heap_types::LispString::from_unibyte(b"hello".to_vec()),
        3,
    );
    undo_list_boundary(&mut list);

    let group = undo_list_pop_group(&mut list);
    assert_eq!(group.len(), 1);
    let entry = group[0];
    assert!(entry.is_cons());
    let car = entry.cons_car();
    assert!(car.is_string());
    // POS should be positive (4) because pt==beg
    assert_eq!(entry.cons_cdr(), Value::fixnum(4));
}

#[test]
fn boundary_separates_groups() {
    crate::test_utils::init_test_tracing();
    let mut list = Value::NIL;
    undo_list_record_insert(&mut list, 0, 1, 0);
    undo_list_boundary(&mut list);
    undo_list_record_insert(&mut list, 1, 1, 1);
    undo_list_boundary(&mut list);

    let g2 = undo_list_pop_group(&mut list);
    assert_eq!(g2.len(), 1);
    let entry = g2[0];
    assert!(entry.is_cons());
    assert_eq!(entry.cons_car(), Value::fixnum(2)); // 1+1
    assert_eq!(entry.cons_cdr(), Value::fixnum(3)); // 1+1+1

    let g1 = undo_list_pop_group(&mut list);
    assert_eq!(g1.len(), 1);
    let entry = g1[0];
    assert!(entry.is_cons());
    assert_eq!(entry.cons_car(), Value::fixnum(1)); // 0+1
    assert_eq!(entry.cons_cdr(), Value::fixnum(2)); // 0+1+1
}

#[test]
fn disabled_records_nothing() {
    crate::test_utils::init_test_tracing();
    let mut list = Value::T;
    undo_list_record_insert(&mut list, 0, 5, 0);
    assert!(undo_list_is_disabled(&list));
}

#[test]
fn cursor_move_dedup() {
    crate::test_utils::init_test_tracing();
    let mut list = Value::NIL;
    undo_list_record_point(&mut list, 5);
    undo_list_record_point(&mut list, 5);
    undo_list_record_point(&mut list, 5);
    // Should only have one entry
    assert!(list.is_cons());
    assert_eq!(list.cons_car(), Value::fixnum(6));
    assert!(list.cons_cdr().is_nil());

    undo_list_record_point(&mut list, 10);
    // Now should have two entries
    assert!(list.is_cons());
    assert_eq!(list.cons_car(), Value::fixnum(11));
}

#[test]
fn no_double_boundary() {
    crate::test_utils::init_test_tracing();
    let mut list = Value::NIL;
    undo_list_record_insert(&mut list, 0, 1, 0);
    undo_list_boundary(&mut list);
    undo_list_boundary(&mut list);
    undo_list_boundary(&mut list);
    // Only one boundary after the insert
    assert!(undo_list_has_trailing_boundary(&list));
    // Pop it: boundary + insert = 1 record in group
    let group = undo_list_pop_group(&mut list);
    assert_eq!(group.len(), 1);
}

#[test]
fn to_value_produces_list() {
    crate::test_utils::init_test_tracing();
    let mut list = Value::NIL;
    undo_list_record_insert(&mut list, 0, 5, 0);
    undo_list_boundary(&mut list);
    assert!(list.is_list());
}

#[test]
fn undoing_flag_not_needed() {
    crate::test_utils::init_test_tracing();
    // The undoing flag is now tracked on Buffer, not in the undo list itself.
    // This test just verifies that disabled lists don't record.
    let mut list = Value::T; // disabled
    undo_list_record_insert(&mut list, 0, 5, 0);
    assert!(undo_list_is_disabled(&list));

    let mut list2 = Value::NIL; // enabled
    undo_list_record_insert(&mut list2, 0, 5, 0);
    assert!(!undo_list_is_empty(&list2));
}
