use super::*;

#[test]
fn marker_data_new_fields_default() {
    let data = MarkerData {
        buffer: None,
        insertion_type: false,
        marker_id: None,
        bytepos: 0,
        charpos: 0,
        next_marker: std::ptr::null_mut(),
    };
    assert_eq!(data.bytepos, 0);
    assert_eq!(data.charpos, 0);
    assert!(data.next_marker.is_null());
}
