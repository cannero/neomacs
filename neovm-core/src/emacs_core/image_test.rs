use super::*;

// -----------------------------------------------------------------------
// image-type-available-p
// -----------------------------------------------------------------------

#[test]
fn type_available_png() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_type_available_p(vec![Value::symbol("png")]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_truthy());
}

#[test]
fn type_available_jpeg() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_type_available_p(vec![Value::symbol("jpeg")]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_truthy());
}

#[test]
fn type_available_gif() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_type_available_p(vec![Value::symbol("gif")]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_truthy());
}

#[test]
fn type_available_svg() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_type_available_p(vec![Value::symbol("svg")]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_truthy());
}

#[test]
fn type_available_webp() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_type_available_p(vec![Value::symbol("webp")]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_truthy());
}

#[test]
fn type_available_neomacs() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_type_available_p(vec![Value::symbol("neomacs")]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn type_available_jpg_alias_is_nil() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_type_available_p(vec![Value::symbol("jpg")]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn type_available_unknown() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_type_available_p(vec![Value::symbol("heic")]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn type_available_wrong_type() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_type_available_p(vec![Value::fixnum(42)]);
    assert!(result.is_err());
}

#[test]
fn type_available_wrong_arity() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_type_available_p(vec![]);
    assert!(result.is_err());
}

// -----------------------------------------------------------------------
// create-image
// -----------------------------------------------------------------------

#[test]
fn create_image_file() {
    crate::test_utils::init_test_tracing();
    let result = builtin_create_image(vec![Value::string("test.png"), Value::symbol("png")]);
    assert!(result.is_ok());
    let spec = result.unwrap();
    assert!(is_image_spec(&spec));

    let plist = image_spec_plist(&spec);
    let img_type = plist_get(&plist, &Value::keyword("type"));
    assert_eq!(img_type.as_symbol_name(), Some("png"));

    let file = plist_get(&plist, &Value::keyword("file"));
    assert_eq!(file.as_str(), Some("test.png"));
}

#[test]
fn create_image_data() {
    crate::test_utils::init_test_tracing();
    let result = builtin_create_image(vec![
        Value::string("raw-png-data"),
        Value::symbol("png"),
        Value::T, // DATA-P
    ]);
    assert!(result.is_ok());
    let spec = result.unwrap();

    let plist = image_spec_plist(&spec);
    let data = plist_get(&plist, &Value::keyword("data"));
    assert_eq!(data.as_str(), Some("raw-png-data"));

    // Should NOT have :file.
    let file = plist_get(&plist, &Value::keyword("file"));
    assert!(file.is_nil());
}

#[test]
fn create_image_file_accepts_raw_unibyte_name() {
    crate::test_utils::init_test_tracing();
    let raw = Value::heap_string(crate::heap_types::LispString::from_unibyte(
        b"test-\xFF.png".to_vec(),
    ));
    let result = builtin_create_image(vec![raw, Value::symbol("png")]);
    assert!(result.is_ok());
    let spec = result.unwrap();
    assert!(is_image_spec(&spec));
}

#[test]
fn create_image_data_accepts_raw_unibyte_payload() {
    crate::test_utils::init_test_tracing();
    let raw = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![0xFF]));
    let result = builtin_create_image(vec![raw, Value::symbol("png"), Value::T]);
    assert!(result.is_ok());
    let spec = result.unwrap();
    assert!(is_image_spec(&spec));
}

#[test]
fn create_image_default_type() {
    crate::test_utils::init_test_tracing();
    let result = builtin_create_image(vec![Value::string("foo.png")]);
    assert!(result.is_ok());
    let spec = result.unwrap();

    let plist = image_spec_plist(&spec);
    let img_type = plist_get(&plist, &Value::keyword("type"));
    assert_eq!(img_type.as_symbol_name(), Some("png"));
}

#[test]
fn create_image_default_type_from_jpg_extension() {
    crate::test_utils::init_test_tracing();
    let result = builtin_create_image(vec![Value::string("foo.JPG")]);
    assert!(result.is_ok());
    let spec = result.unwrap();

    let plist = image_spec_plist(&spec);
    let img_type = plist_get(&plist, &Value::keyword("type"));
    assert_eq!(img_type.as_symbol_name(), Some("jpeg"));
}

#[test]
fn create_image_default_type_falls_back_to_neomacs() {
    crate::test_utils::init_test_tracing();
    let result = builtin_create_image(vec![Value::string("foo.unknown")]);
    assert!(result.is_ok());
    let spec = result.unwrap();

    let plist = image_spec_plist(&spec);
    let img_type = plist_get(&plist, &Value::keyword("type"));
    assert!(img_type.is_nil());
}

#[test]
fn create_image_data_type_from_mime_hint() {
    crate::test_utils::init_test_tracing();
    let result = builtin_create_image(vec![
        Value::string("raw-image-bytes"),
        Value::NIL,
        Value::symbol("image/jpeg"),
    ]);
    assert!(result.is_ok());
    let spec = result.unwrap();

    let plist = image_spec_plist(&spec);
    let img_type = plist_get(&plist, &Value::keyword("type"));
    assert!(img_type.is_nil());
}

#[test]
fn create_image_with_props() {
    crate::test_utils::init_test_tracing();
    let result = builtin_create_image(vec![
        Value::string("icon.svg"),
        Value::symbol("svg"),
        Value::NIL,
        Value::keyword("width"),
        Value::fixnum(64),
        Value::keyword("height"),
        Value::fixnum(64),
    ]);
    assert!(result.is_ok());
    let spec = result.unwrap();

    let plist = image_spec_plist(&spec);
    let width = plist_get(&plist, &Value::keyword("width"));
    assert_eq!(width.as_int(), Some(64));

    let height = plist_get(&plist, &Value::keyword("height"));
    assert_eq!(height.as_int(), Some(64));
}

#[test]
fn create_image_wrong_arity() {
    crate::test_utils::init_test_tracing();
    let result = builtin_create_image(vec![]);
    assert!(result.is_err());
}

#[test]
fn create_image_bad_type() {
    crate::test_utils::init_test_tracing();
    let result = builtin_create_image(vec![
        Value::string("test.png"),
        Value::fixnum(42), // not a symbol
    ]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "error"
    ));
}

// -----------------------------------------------------------------------
// image-size
// -----------------------------------------------------------------------

#[test]
fn image_size_pixels() {
    crate::test_utils::init_test_tracing();
    let spec = builtin_create_image(vec![Value::string("test.png"), Value::symbol("png")]).unwrap();

    let result = builtin_image_size(vec![spec, Value::T]);
    assert!(result.is_err());
}

#[test]
fn image_size_chars() {
    crate::test_utils::init_test_tracing();
    let spec = builtin_create_image(vec![Value::string("test.png"), Value::symbol("png")]).unwrap();

    let result = builtin_image_size(vec![spec]);
    assert!(result.is_err());
}

#[test]
fn image_size_not_image_spec() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_size(vec![Value::fixnum(42)]);
    assert!(result.is_err());
}

#[test]
fn image_size_wrong_arity() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_size(vec![]);
    assert!(result.is_err());
}

// -----------------------------------------------------------------------
// image-mask-p
// -----------------------------------------------------------------------

#[test]
fn image_mask_p_batch_errors_without_window_system() {
    crate::test_utils::init_test_tracing();
    let spec = builtin_create_image(vec![Value::string("test.png"), Value::symbol("png")]).unwrap();

    let result = builtin_image_mask_p(vec![spec]);
    assert!(result.is_err());
}

#[test]
fn image_mask_p_not_image() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_mask_p(vec![Value::string("not an image")]);
    assert!(result.is_err());
}

// -----------------------------------------------------------------------
// put-image
// -----------------------------------------------------------------------

#[test]
fn put_image_requires_image_and_point() {
    crate::test_utils::init_test_tracing();
    let spec = builtin_create_image(vec![Value::string("test.png"), Value::symbol("png")]).unwrap();

    let result = builtin_put_image(vec![spec, Value::fixnum(1)]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_truthy());
}

#[test]
fn put_image_accepts_char_point() {
    crate::test_utils::init_test_tracing();
    let spec = builtin_create_image(vec![Value::string("test.png"), Value::symbol("png")]).unwrap();

    let result = builtin_put_image(vec![spec, Value::char('a')]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_truthy());
}

#[test]
fn put_image_bad_point() {
    crate::test_utils::init_test_tracing();
    let spec = builtin_create_image(vec![Value::string("test.png"), Value::symbol("png")]).unwrap();

    let result = builtin_put_image(vec![spec, Value::string("not a point")]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
            && sig.data.first() == Some(&Value::symbol("integer-or-marker-p"))
    ));
}

#[test]
fn put_image_invalid_area() {
    crate::test_utils::init_test_tracing();
    let spec = builtin_create_image(vec![Value::string("test.png"), Value::symbol("png")]).unwrap();
    let result = builtin_put_image(vec![
        spec,
        Value::fixnum(1),
        Value::NIL,
        Value::symbol("center"),
    ]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "error"
            && sig.data.first() == Some(&Value::string("Invalid area center"))
    ));
}

#[test]
fn put_image_not_image() {
    crate::test_utils::init_test_tracing();
    let result = builtin_put_image(vec![Value::fixnum(1), Value::fixnum(1)]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "error"
            && sig.data.first() == Some(&Value::string("Not an image: 1"))
    ));
}

// -----------------------------------------------------------------------
// insert-image
// -----------------------------------------------------------------------

#[test]
fn insert_image_without_position_returns_true() {
    crate::test_utils::init_test_tracing();
    let spec = builtin_create_image(vec![Value::string("test.png"), Value::symbol("png")]).unwrap();

    let result = builtin_insert_image(vec![spec]);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), Value::T);
}

#[test]
fn insert_image_not_image() {
    crate::test_utils::init_test_tracing();
    let result = builtin_insert_image(vec![Value::fixnum(42)]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "error"
            && sig.data.first() == Some(&Value::string("Not an image: 42"))
    ));
}

#[test]
fn insert_image_invalid_area() {
    crate::test_utils::init_test_tracing();
    let spec = builtin_create_image(vec![Value::string("test.png"), Value::symbol("png")]).unwrap();
    let result = builtin_insert_image(vec![spec, Value::NIL, Value::symbol("center")]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "error"
            && sig.data.first() == Some(&Value::string("Invalid area center"))
    ));
}

#[test]
fn insert_image_too_many_args() {
    crate::test_utils::init_test_tracing();
    let spec = builtin_create_image(vec![Value::string("test.png"), Value::symbol("png")]).unwrap();
    let result = builtin_insert_image(vec![
        spec,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
        Value::NIL,
    ]);
    assert!(result.is_err());
}

// -----------------------------------------------------------------------
// remove-images
// -----------------------------------------------------------------------

#[test]
fn remove_images_no_error_for_default_args() {
    crate::test_utils::init_test_tracing();
    let result = builtin_remove_images(vec![Value::fixnum(1), Value::fixnum(100)]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn remove_images_accepts_char_positions() {
    crate::test_utils::init_test_tracing();
    let result = builtin_remove_images(vec![Value::char('a'), Value::char('z')]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn remove_images_bad_start() {
    crate::test_utils::init_test_tracing();
    let result = builtin_remove_images(vec![Value::string("x"), Value::fixnum(100)]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
            && sig.data.first() == Some(&Value::symbol("integer-or-marker-p"))
    ));
}

#[test]
fn remove_images_bad_end() {
    crate::test_utils::init_test_tracing();
    let result = builtin_remove_images(vec![Value::fixnum(1), Value::string("x")]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
            && sig.data.first() == Some(&Value::symbol("integer-or-marker-p"))
    ));
}

#[test]
fn remove_images_bad_buffer() {
    crate::test_utils::init_test_tracing();
    let result = builtin_remove_images(vec![Value::fixnum(1), Value::fixnum(10), Value::fixnum(1)]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn remove_images_wrong_arity() {
    crate::test_utils::init_test_tracing();
    let result = builtin_remove_images(vec![Value::fixnum(1)]);
    assert!(result.is_err());
}

// -----------------------------------------------------------------------
// image-flush
// -----------------------------------------------------------------------

#[test]
fn image_flush_rejects_non_window_frame() {
    crate::test_utils::init_test_tracing();
    let spec = builtin_create_image(vec![Value::string("test.png"), Value::symbol("png")]).unwrap();

    let result = builtin_image_flush(vec![spec]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "error"
            && sig.data.first() == Some(&Value::string("Window system frame should be used"))
    ));
}

#[test]
fn image_flush_all_frames() {
    crate::test_utils::init_test_tracing();
    let spec = builtin_create_image(vec![Value::string("test.png"), Value::symbol("png")]).unwrap();
    let result = builtin_image_flush(vec![spec, Value::T]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn image_flush_non_t_frame_errors() {
    crate::test_utils::init_test_tracing();
    let spec = builtin_create_image(vec![Value::string("test.png"), Value::symbol("png")]).unwrap();
    let result = builtin_image_flush(vec![spec, Value::fixnum(1)]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data.first() == Some(&Value::symbol("frame-live-p"))
    ));
}

#[test]
fn image_flush_not_image() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_flush(vec![Value::fixnum(42)]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "error"
            && sig.data.first() == Some(&Value::string("Invalid image specification"))
    ));
}

// -----------------------------------------------------------------------
// clear-image-cache
// -----------------------------------------------------------------------

#[test]
fn clear_image_cache_no_args() {
    crate::test_utils::init_test_tracing();
    let result = builtin_clear_image_cache(vec![]);
    assert!(result.is_err());
}

#[test]
fn clear_image_cache_nil_filter_errors() {
    crate::test_utils::init_test_tracing();
    let result = builtin_clear_image_cache(vec![Value::NIL]);
    assert!(result.is_err());
}

#[test]
fn clear_image_cache_with_filter() {
    crate::test_utils::init_test_tracing();
    let result = builtin_clear_image_cache(vec![Value::T]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn clear_image_cache_animation_cache_non_list() {
    crate::test_utils::init_test_tracing();
    let result = builtin_clear_image_cache(vec![Value::T, Value::T]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
            && sig.data.first() == Some(&Value::symbol("listp"))
    ));
}

#[test]
fn clear_image_cache_nil_second_arg_but_valid_filter() {
    crate::test_utils::init_test_tracing();
    let result = builtin_clear_image_cache(vec![Value::T, Value::NIL]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn clear_image_cache_with_animation_cache_list() {
    crate::test_utils::init_test_tracing();
    let cache_arg = Value::list(vec![Value::symbol("foo"), Value::symbol("bar")]);
    let result = builtin_clear_image_cache(vec![Value::T, cache_arg]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn image_cache_size_is_zero() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_cache_size(vec![]);
    assert_eq!(result.unwrap(), Value::fixnum(0));
}

#[test]
fn imagep_matches_image_spec_shape() {
    crate::test_utils::init_test_tracing();
    let spec = builtin_create_image(vec![Value::string("test.png"), Value::symbol("png")])
        .expect("create-image should succeed");
    assert!(builtin_imagep(vec![spec]).unwrap().is_truthy());
    assert!(builtin_imagep(vec![Value::fixnum(1)]).unwrap().is_nil());
    assert!(
        builtin_imagep(vec![Value::list(vec![
            Value::symbol("image"),
            Value::keyword("type"),
            Value::symbol("png"),
        ])])
        .unwrap()
        .is_nil()
    );
    assert!(
        builtin_imagep(vec![Value::list(vec![
            Value::symbol("image"),
            Value::keyword("file"),
            Value::string("x.png"),
        ])])
        .unwrap()
        .is_nil()
    );
}

#[test]
fn image_metadata_non_spec_returns_nil() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_metadata(vec![Value::fixnum(1)]).unwrap();
    assert!(result.is_nil());
}

#[test]
fn image_metadata_window_system_error_shape() {
    crate::test_utils::init_test_tracing();
    let spec = builtin_create_image(vec![Value::string("test.png"), Value::symbol("png")])
        .expect("create-image should succeed");
    let result = builtin_image_metadata(vec![spec]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "error"
            && sig.data.first() == Some(&Value::string("Window system frame should be used"))
    ));
}

#[test]
fn image_metadata_second_arg_validates_frame_designator() {
    crate::test_utils::init_test_tracing();
    let spec = builtin_create_image(vec![Value::string("test.png"), Value::symbol("png")])
        .expect("create-image should succeed");
    let result = builtin_image_metadata(vec![spec, Value::T]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
            && sig.data.first() == Some(&Value::symbol("frame-live-p"))
    ));
}

// -----------------------------------------------------------------------
// image-type
// -----------------------------------------------------------------------

#[test]
fn image_type_png() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_type(vec![Value::string("test.png")]);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_symbol_name(), Some("png"));
}

#[test]
fn image_type_svg() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_type(vec![Value::string("icon.svg")]);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_symbol_name(), Some("svg"));
}

#[test]
fn image_type_not_image() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_type(vec![Value::fixnum(42)]);
    assert!(result.is_err());
}

#[test]
fn image_type_wrong_arity() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_type(vec![]);
    assert!(result.is_err());
}

#[test]
fn image_type_from_filename_extension() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_type(vec![Value::string("foo.JPG")]);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_symbol_name(), Some("jpeg"));
}

#[test]
fn image_type_explicit_type() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_type(vec![
        Value::string("no-extension"),
        Value::symbol("png"),
        Value::NIL,
    ]);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_symbol_name(), Some("png"));
}

#[test]
fn image_type_unknown_signals() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_type(vec![Value::string("unknown.bin")]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig)) if sig.symbol_name() == "unknown-image-type"
    ));
}

// -----------------------------------------------------------------------
// image-transforms-p
// -----------------------------------------------------------------------

#[test]
fn image_transforms_p_returns_t() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_transforms_p(vec![]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn image_transforms_p_with_frame() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_transforms_p(vec![Value::NIL]);
    assert!(result.is_ok());
    assert!(result.unwrap().is_nil());
}

#[test]
fn image_transforms_p_with_non_integer_or_small_frame() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_transforms_p(vec![Value::fixnum(1)]);
    assert!(matches!(
        result,
        Err(Flow::Signal(sig))
            if sig.symbol_name() == "wrong-type-argument"
                && sig.data
                    == vec![Value::symbol("frame-live-p"), Value::fixnum(1)]
    ));
}

#[test]
fn image_transforms_p_too_many_args() {
    crate::test_utils::init_test_tracing();
    let result = builtin_image_transforms_p(vec![Value::NIL, Value::NIL]);
    assert!(result.is_err());
}

// -----------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------

#[test]
fn plist_get_basic() {
    crate::test_utils::init_test_tracing();
    let plist = Value::list(vec![
        Value::keyword("type"),
        Value::symbol("png"),
        Value::keyword("file"),
        Value::string("test.png"),
    ]);
    let val = plist_get(&plist, &Value::keyword("type"));
    assert_eq!(val.as_symbol_name(), Some("png"));

    let file = plist_get(&plist, &Value::keyword("file"));
    assert_eq!(file.as_str(), Some("test.png"));
}

#[test]
fn plist_get_missing() {
    crate::test_utils::init_test_tracing();
    let plist = Value::list(vec![Value::keyword("type"), Value::symbol("png")]);
    let val = plist_get(&plist, &Value::keyword("missing"));
    assert!(val.is_nil());
}

#[test]
fn is_image_spec_valid() {
    crate::test_utils::init_test_tracing();
    let spec = Value::list(vec![
        Value::symbol("image"),
        Value::keyword("type"),
        Value::symbol("png"),
        Value::keyword("file"),
        Value::string("test.png"),
    ]);
    assert!(is_image_spec(&spec));
}

#[test]
fn is_image_spec_bare_plist() {
    crate::test_utils::init_test_tracing();
    let spec = Value::list(vec![Value::keyword("type"), Value::symbol("png")]);
    assert!(!is_image_spec(&spec));
}

#[test]
fn is_image_spec_not_image() {
    crate::test_utils::init_test_tracing();
    assert!(!is_image_spec(&Value::fixnum(42)));
    assert!(!is_image_spec(&Value::NIL));
    assert!(!is_image_spec(&Value::string("not an image")));
}

#[test]
fn is_image_spec_empty_list() {
    crate::test_utils::init_test_tracing();
    let spec = Value::list(vec![]);
    assert!(!is_image_spec(&spec));
}

#[test]
fn is_image_spec_requires_supported_type_and_one_source() {
    crate::test_utils::init_test_tracing();
    let valid_file = Value::list(vec![
        Value::symbol("image"),
        Value::keyword("type"),
        Value::symbol("png"),
        Value::keyword("file"),
        Value::string("x.png"),
    ]);
    assert!(is_image_spec(&valid_file));

    let valid_data = Value::list(vec![
        Value::symbol("image"),
        Value::keyword("type"),
        Value::symbol("png"),
        Value::keyword("data"),
        Value::string("raw"),
    ]);
    assert!(is_image_spec(&valid_data));

    let unsupported_type = Value::list(vec![
        Value::symbol("image"),
        Value::keyword("type"),
        Value::symbol("jpg"),
        Value::keyword("file"),
        Value::string("x.jpg"),
    ]);
    assert!(!is_image_spec(&unsupported_type));

    let both_sources = Value::list(vec![
        Value::symbol("image"),
        Value::keyword("type"),
        Value::symbol("png"),
        Value::keyword("file"),
        Value::string("x.png"),
        Value::keyword("data"),
        Value::string("raw"),
    ]);
    assert!(!is_image_spec(&both_sources));

    let missing_source = Value::list(vec![
        Value::symbol("image"),
        Value::keyword("type"),
        Value::symbol("png"),
    ]);
    assert!(!is_image_spec(&missing_source));
}

#[test]
fn image_spec_plist_with_image_prefix() {
    crate::test_utils::init_test_tracing();
    let spec = Value::list(vec![
        Value::symbol("image"),
        Value::keyword("type"),
        Value::symbol("png"),
    ]);
    let plist = image_spec_plist(&spec);
    let val = plist_get(&plist, &Value::keyword("type"));
    assert_eq!(val.as_symbol_name(), Some("png"));
}

#[test]
fn image_spec_plist_bare() {
    crate::test_utils::init_test_tracing();
    let spec = Value::list(vec![Value::keyword("type"), Value::symbol("jpeg")]);
    let plist = image_spec_plist(&spec);
    let val = plist_get(&plist, &Value::keyword("type"));
    assert_eq!(val.as_symbol_name(), Some("jpeg"));
}

#[test]
fn round_trip_create_then_type() {
    crate::test_utils::init_test_tracing();
    // `create-image` keeps the explicit :type marker in the resulting spec.
    let spec =
        builtin_create_image(vec![Value::string("photo.jpg"), Value::symbol("jpeg")]).unwrap();
    let plist = image_spec_plist(&spec);
    let img_type = plist_get(&plist, &Value::keyword("type"));
    assert_eq!(img_type.as_symbol_name(), Some("jpeg"));
}

#[test]
fn round_trip_create_then_size() {
    crate::test_utils::init_test_tracing();
    // In batch, image-size requires a window-system frame.
    let spec =
        builtin_create_image(vec![Value::string("photo.jpg"), Value::symbol("jpeg")]).unwrap();

    let result = builtin_image_size(vec![spec, Value::T]);
    assert!(result.is_err());
}
