use super::*;

#[test]
fn card_index_of_inside_range_is_offset_divided_by_card_size() {
    let table = CardTable::new(0x1000, 0x2000, 512);
    assert_eq!(table.card_index_of(0x1000), Some(0));
    assert_eq!(table.card_index_of(0x11ff), Some(0));
    assert_eq!(table.card_index_of(0x1200), Some(1));
    // Last byte of the 0x2000-byte covered range → card 15.
    assert_eq!(table.card_index_of(0x2fff), Some(15));
}

#[test]
fn card_index_of_outside_range_is_none() {
    let table = CardTable::new(0x1000, 0x1000, 512);
    assert_eq!(table.card_index_of(0x0fff), None);
    assert_eq!(table.card_index_of(0x2000), None);
    assert_eq!(table.card_index_of(0x5000), None);
}

#[test]
fn record_write_sets_card_dirty_in_range() {
    let table = CardTable::new(0x1000, 0x1000, 512);
    assert!(!table.is_dirty(0x1000));
    table.record_write(0x1234);
    assert!(table.is_dirty(0x1234));
    // Neighboring card unaffected.
    assert!(!table.is_dirty(0x1400));
}

#[test]
fn record_write_outside_range_is_a_no_op() {
    let table = CardTable::new(0x1000, 0x1000, 512);
    table.record_write(0x5000);
    // No panic, and the table still reports clean for every
    // in-range card.
    for index in 0..table.card_count() {
        let (start, _) = table.card_range(index);
        assert!(!table.is_dirty(start));
    }
}

#[test]
fn clear_all_restores_every_card_to_clean() {
    let table = CardTable::new(0x1000, 0x1000, 512);
    table.record_write(0x1100);
    table.record_write(0x1600);
    table.record_write(0x1f00);
    assert_eq!(table.dirty_card_indices().len(), 3);
    table.clear_all();
    assert_eq!(table.dirty_card_indices().len(), 0);
}

#[test]
fn dirty_card_indices_reports_indices_in_ascending_order() {
    let table = CardTable::new(0x0, 0x2000, 512);
    table.record_write(0x1000); // card 8
    table.record_write(0x200); // card 1
    table.record_write(0x1800); // card 12
    let dirty = table.dirty_card_indices();
    assert_eq!(dirty, vec![1, 8, 12]);
}

#[test]
fn card_range_matches_index() {
    let table = CardTable::new(0x1000, 0x1000, 512);
    assert_eq!(table.card_range(0), (0x1000, 0x1200));
    assert_eq!(table.card_range(7), (0x1e00, 0x2000));
}

#[test]
fn default_card_size_is_512_bytes() {
    let table = CardTable::with_default_card_size(0, 4096);
    assert_eq!(table.card_size(), DEFAULT_CARD_SIZE_BYTES);
    assert_eq!(table.card_count(), 4096 / DEFAULT_CARD_SIZE_BYTES);
}
