use super::*;

#[test]
fn test_dmabuf_exporter_without_display() {
    let exporter = DmaBufExporter::new(ptr::null_mut());
    assert!(!exporter.is_supported());
}
