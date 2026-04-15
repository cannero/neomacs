use super::*;

#[test]
fn mode_line_iterator_sets_mode_line_p() {
    let it = It::new_for_mode_line(0);
    assert!(it.mode_line_p);
    assert_eq!(it.method, ItMethod::FromString);
    assert_eq!(it.charpos, -1);
    assert_eq!(it.bytepos, -1);
}

#[test]
fn buffer_iterator_does_not_set_mode_line_p() {
    let it = It::new_for_buffer(1, 1, 0);
    assert!(!it.mode_line_p);
    assert_eq!(it.method, ItMethod::FromBuffer);
    assert_eq!(it.charpos, 1);
}

#[test]
fn reset_row_geometry_zeroes_per_row_fields() {
    let mut it = It::new_for_buffer(1, 1, 0);
    it.current_x = 100.0;
    it.ascent = 15.0;
    it.descent = 5.0;
    it.pixel_width = 10.0;
    it.reset_row_geometry();
    assert_eq!(it.current_x, 0.0);
    assert_eq!(it.ascent, 0.0);
    assert_eq!(it.descent, 0.0);
    assert_eq!(it.pixel_width, 0.0);
}

#[test]
fn bidi_fields_default_inactive() {
    // Per the Rev 3 correction: bidi fields are core to struct
    // it but neomacs day-1 ships with bidi_p=false. The fields
    // MUST exist (so the walker's type signature matches GNU's
    // calling convention) but day-1 uses unicode order.
    let it = It::new_for_mode_line(0);
    assert!(!it.bidi_p);
    assert_eq!(it.paragraph_embedding, BidiDir::Ltr);
}
