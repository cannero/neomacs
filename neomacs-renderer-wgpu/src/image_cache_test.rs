use super::*;

#[test]
fn test_convert_argb32_to_rgba_basic() {
    // Create a 2x2 ARGB32 image
    // ARGB32 format: A, R, G, B (4 bytes per pixel)
    let width = 2u32;
    let height = 2u32;
    let stride = width * 4; // No padding
    let data: Vec<u8> = vec![
        // Row 0
        255, 100, 150, 200, // Pixel (0,0): A=255, R=100, G=150, B=200
        128, 50, 75, 100, // Pixel (1,0): A=128, R=50, G=75, B=100
        // Row 1
        64, 25, 37, 50, // Pixel (0,1): A=64, R=25, G=37, B=50
        0, 0, 0, 0, // Pixel (1,1): A=0, R=0, G=0, B=0 (transparent)
    ];

    let result = ImageCache::convert_argb32_to_rgba(&data, width, height, stride, 0, 0);
    assert!(result.is_some());

    let (w, h, rgba) = result.unwrap();
    assert_eq!(w, 2);
    assert_eq!(h, 2);
    assert_eq!(rgba.len(), 16); // 2x2x4 bytes

    // Expected RGBA output: R, G, B, A
    // Pixel (0,0): R=100, G=150, B=200, A=255
    assert_eq!(&rgba[0..4], &[100, 150, 200, 255]);
    // Pixel (1,0): R=50, G=75, B=100, A=128
    assert_eq!(&rgba[4..8], &[50, 75, 100, 128]);
    // Pixel (0,1): R=25, G=37, B=50, A=64
    assert_eq!(&rgba[8..12], &[25, 37, 50, 64]);
    // Pixel (1,1): R=0, G=0, B=0, A=0
    assert_eq!(&rgba[12..16], &[0, 0, 0, 0]);
}

#[test]
fn test_convert_argb32_with_stride_padding() {
    // 2x2 image with stride = 12 (4 bytes padding per row)
    let width = 2u32;
    let height = 2u32;
    let stride = 12u32; // 8 bytes data + 4 bytes padding per row
    let data: Vec<u8> = vec![
        // Row 0 (8 bytes data + 4 bytes padding)
        255, 100, 150, 200, // Pixel (0,0)
        128, 50, 75, 100, // Pixel (1,0)
        0, 0, 0, 0, // Padding (ignored)
        // Row 1 (8 bytes data + 4 bytes padding)
        64, 25, 37, 50, // Pixel (0,1)
        32, 10, 20, 30, // Pixel (1,1)
        0, 0, 0, 0, // Padding (ignored)
    ];

    let result = ImageCache::convert_argb32_to_rgba(&data, width, height, stride, 0, 0);
    assert!(result.is_some());

    let (w, h, rgba) = result.unwrap();
    assert_eq!(w, 2);
    assert_eq!(h, 2);

    // Verify conversion (padding should be ignored)
    assert_eq!(&rgba[0..4], &[100, 150, 200, 255]); // Pixel (0,0)
    assert_eq!(&rgba[4..8], &[50, 75, 100, 128]); // Pixel (1,0)
    assert_eq!(&rgba[8..12], &[25, 37, 50, 64]); // Pixel (0,1)
    assert_eq!(&rgba[12..16], &[10, 20, 30, 32]); // Pixel (1,1)
}

#[test]
fn test_convert_argb32_invalid_data_size() {
    // Data too small for 2x2 image
    let data: Vec<u8> = vec![255, 100, 150, 200]; // Only 1 pixel
    let result = ImageCache::convert_argb32_to_rgba(&data, 2, 2, 8, 0, 0);
    assert!(result.is_none());
}

#[test]
fn test_convert_rgb24_to_rgba_basic() {
    // Create a 2x2 RGB24 image
    // RGB24 format: R, G, B (3 bytes per pixel)
    let width = 2u32;
    let height = 2u32;
    let stride = width * 3; // No padding
    let data: Vec<u8> = vec![
        // Row 0
        100, 150, 200, // Pixel (0,0): R=100, G=150, B=200
        50, 75, 100, // Pixel (1,0): R=50, G=75, B=100
        // Row 1
        25, 37, 50, // Pixel (0,1): R=25, G=37, B=50
        0, 0, 0, // Pixel (1,1): R=0, G=0, B=0 (black)
    ];

    let result = ImageCache::convert_rgb24_to_rgba(&data, width, height, stride, 0, 0);
    assert!(result.is_some());

    let (w, h, rgba) = result.unwrap();
    assert_eq!(w, 2);
    assert_eq!(h, 2);
    assert_eq!(rgba.len(), 16); // 2x2x4 bytes

    // Expected RGBA output: R, G, B, A (A should always be 255)
    assert_eq!(&rgba[0..4], &[100, 150, 200, 255]);
    assert_eq!(&rgba[4..8], &[50, 75, 100, 255]);
    assert_eq!(&rgba[8..12], &[25, 37, 50, 255]);
    assert_eq!(&rgba[12..16], &[0, 0, 0, 255]);
}

#[test]
fn test_convert_rgb24_with_stride_padding() {
    // 2x2 image with stride = 8 (2 bytes padding per row)
    let width = 2u32;
    let height = 2u32;
    let stride = 8u32; // 6 bytes data + 2 bytes padding per row
    let data: Vec<u8> = vec![
        // Row 0 (6 bytes data + 2 bytes padding)
        100, 150, 200, // Pixel (0,0)
        50, 75, 100, // Pixel (1,0)
        0, 0, // Padding (ignored)
        // Row 1 (6 bytes data + 2 bytes padding)
        25, 37, 50, // Pixel (0,1)
        10, 20, 30, // Pixel (1,1)
        0, 0, // Padding (ignored)
    ];

    let result = ImageCache::convert_rgb24_to_rgba(&data, width, height, stride, 0, 0);
    assert!(result.is_some());

    let (w, h, rgba) = result.unwrap();
    assert_eq!(w, 2);
    assert_eq!(h, 2);

    // Verify conversion (padding should be ignored)
    assert_eq!(&rgba[0..4], &[100, 150, 200, 255]); // Pixel (0,0)
    assert_eq!(&rgba[4..8], &[50, 75, 100, 255]); // Pixel (1,0)
    assert_eq!(&rgba[8..12], &[25, 37, 50, 255]); // Pixel (0,1)
    assert_eq!(&rgba[12..16], &[10, 20, 30, 255]); // Pixel (1,1)
}

#[test]
fn test_convert_rgb24_invalid_data_size() {
    // Data too small for 2x2 image
    let data: Vec<u8> = vec![100, 150, 200]; // Only 1 pixel
    let result = ImageCache::convert_rgb24_to_rgba(&data, 2, 2, 6, 0, 0);
    assert!(result.is_none());
}

#[test]
fn test_constrain_dimensions() {
    // No constraints (uses MAX_TEXTURE_SIZE internally)
    assert_eq!(ImageCache::constrain_dimensions(100, 100, 0, 0), (100, 100));

    // Width constrained
    assert_eq!(
        ImageCache::constrain_dimensions(200, 100, 100, 0),
        (100, 50)
    );

    // Height constrained
    assert_eq!(
        ImageCache::constrain_dimensions(100, 200, 0, 100),
        (50, 100)
    );

    // Both constrained, width is limiting factor
    assert_eq!(
        ImageCache::constrain_dimensions(400, 200, 100, 100),
        (100, 50)
    );

    // Both constrained, height is limiting factor
    assert_eq!(
        ImageCache::constrain_dimensions(200, 400, 100, 100),
        (50, 100)
    );

    // Minimum 1x1 - very narrow image
    let (w, h) = ImageCache::constrain_dimensions(1, 1000, 10, 100);
    assert_eq!(w, 1);
    assert_eq!(h, 100); // Height is constrained to 100, width stays 1 (min)
}

#[test]
fn test_convert_argb32_single_pixel() {
    // Single pixel image - edge case
    let data: Vec<u8> = vec![255, 128, 64, 32]; // A=255, R=128, G=64, B=32
    let result = ImageCache::convert_argb32_to_rgba(&data, 1, 1, 4, 0, 0);
    assert!(result.is_some());

    let (w, h, rgba) = result.unwrap();
    assert_eq!(w, 1);
    assert_eq!(h, 1);
    assert_eq!(rgba, vec![128, 64, 32, 255]); // R=128, G=64, B=32, A=255
}

#[test]
fn test_convert_rgb24_single_pixel() {
    // Single pixel image - edge case
    let data: Vec<u8> = vec![128, 64, 32]; // R=128, G=64, B=32
    let result = ImageCache::convert_rgb24_to_rgba(&data, 1, 1, 3, 0, 0);
    assert!(result.is_some());

    let (w, h, rgba) = result.unwrap();
    assert_eq!(w, 1);
    assert_eq!(h, 1);
    assert_eq!(rgba, vec![128, 64, 32, 255]); // R=128, G=64, B=32, A=255
}
