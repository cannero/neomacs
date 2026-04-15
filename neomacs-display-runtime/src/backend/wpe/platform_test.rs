use super::*;

#[test]
fn test_platform_display_types() {
    // Just verify types compile
    let _: *mut plat::WPEDisplay = std::ptr::null_mut();
    let _: *mut plat::WPEView = std::ptr::null_mut();
    let _: *mut plat::WPEBuffer = std::ptr::null_mut();
}
