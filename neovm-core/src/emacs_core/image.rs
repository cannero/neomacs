//! Image type support builtins.
//!
//! Provides stub/partial implementations of Emacs image builtins:
//! - `image-type-available-p` — check if image type is available
//! - `create-image` — create image descriptor (property list)
//! - `image-size` — return (WIDTH . HEIGHT) cons
//! - `image-mask-p` — check for mask support
//! - `put-image` / `insert-image` / `remove-images` — display stubs
//! - `image-flush` / `clear-image-cache` — cache management stubs
//! - `image-type` — extract type from image spec
//! - `display-images-p` / `image-transforms-p` — capability queries
//!
//! Image specs are property lists: (:type png :file "foo.png" :width 100 ...)

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::value::*;
use crate::window::FRAME_ID_BASE;

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_max_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    if args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_range_args(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Flow> {
    if args.len() < min || args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_frame_designator(_name: &str, value: &Value) -> Result<(), Flow> {
    match value.kind() {
        ValueKind::Nil => Ok(()),
        ValueKind::Fixnum(id) if id >= 0 && (id as u64) >= FRAME_ID_BASE => Ok(()),
        ValueKind::Veclike(VecLikeType::Frame) if value.as_frame_id().unwrap() >= FRAME_ID_BASE => Ok(()),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), *value],
        )),
    }
}

fn normalized_keyword_name(value: &Value) -> Option<&str> {
    match value.kind() {
        ValueKind::Keyword(id) => {
            let s = resolve_sym(id);
            Some(s.strip_prefix(':').unwrap_or(s))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Property list helpers
// ---------------------------------------------------------------------------

/// Get a value from a property list by keyword.
/// The plist is a flat list: (:key1 val1 :key2 val2 ...).
#[cfg(test)]
fn plist_get(plist: &Value, key: &Value) -> Value {
    let mut cursor = *plist;
    loop {
        match cursor.kind() {
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                if eq_value(&pair_car, key) {
                    // Next element is the value.
                    match pair_cdr.kind() {
                        ValueKind::Cons => {
                            return cursor.cons_car();
                        }
                        _ => return Value::NIL,
                    }
                }
                // Skip the value entry.
                match pair.cdr.kind() {
                    ValueKind::Cons => {
                        cursor = pair.cdr.cons_cdr();
                    }
                    _ => return Value::NIL,
                }
            }
            _ => return Value::NIL,
        }
    }
}

/// Check whether a symbol name represents a supported image type.
fn is_supported_image_type(name: &str) -> bool {
    matches!(
        name,
        "png" | "jpeg" | "gif" | "svg" | "webp" | "xpm" | "xbm" | "pbm" | "tiff" | "bmp"
    )
}

fn normalize_image_type_name(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "jpg" => Some("jpeg"),
        "jpeg" => Some("jpeg"),
        "png" => Some("png"),
        "gif" => Some("gif"),
        "svg" => Some("svg"),
        "webp" => Some("webp"),
        "xpm" => Some("xpm"),
        "xbm" => Some("xbm"),
        "pbm" => Some("pbm"),
        "tif" | "tiff" => Some("tiff"),
        "bmp" => Some("bmp"),
        "neomacs" => Some("neomacs"),
        _ => None,
    }
}

fn infer_image_type_from_filename(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?;
    normalize_image_type_name(ext)
}

/// Validate that a value looks like an image spec.
/// Oracle-compatible shape:
/// - list starts with symbol `image`
/// - plist includes a supported symbolic `:type`
/// - plist includes exactly one source key: `:file` or `:data`
/// - source value is a string
fn is_image_spec(value: &Value) -> bool {
    let items = match list_to_vec(value) {
        Some(v) => v,
        None => return false,
    };

    if items.is_empty() || items[0].as_symbol_name() != Some("image") {
        return false;
    }

    let mut type_seen = false;
    let mut type_ok = false;
    let mut file_seen = false;
    let mut file_ok = false;
    let mut data_seen = false;
    let mut data_ok = false;

    let mut i = 1usize;
    while i + 1 < items.len() {
        if let Some(key) = normalized_keyword_name(&items[i]) {
            let val = &items[i + 1];
            match key {
                "type" if !type_seen => {
                    type_seen = true;
                    type_ok = val.as_symbol_name().is_some_and(is_supported_image_type);
                }
                "file" if !file_seen => {
                    file_seen = true;
                    file_ok = val.as_str().is_some();
                }
                "data" if !data_seen => {
                    data_seen = true;
                    data_ok = val.as_str().is_some();
                }
                _ => {}
            }
        }
        i += 2;
    }

    if !type_seen || !type_ok {
        return false;
    }

    match (file_seen, data_seen) {
        (true, false) => file_ok,
        (false, true) => data_ok,
        _ => false,
    }
}

/// Extract the plist portion of an image spec.
/// If the spec starts with `image`, skip that first element.
#[cfg(test)]
fn image_spec_plist(spec: &Value) -> Value {
    let items = match list_to_vec(spec) {
        Some(v) => v,
        None => return Value::NIL,
    };
    if items.is_empty() {
        return Value::NIL;
    }
    if let Some(name) = items[0].as_symbol_name() {
        if name == "image" {
            // Plist is everything after the `image` symbol.
            return Value::list(items[1..].to_vec());
        }
    }
    // Already a bare plist.
    *spec
}

// ---------------------------------------------------------------------------
// Pure builtins
// ---------------------------------------------------------------------------

/// (image-type-available-p TYPE) -> t or nil
///
/// Return t if image type TYPE is available in this Emacs instance.
/// Supported types: png, jpeg, gif, svg, webp, xpm, xbm, pbm, tiff, bmp.
pub(crate) fn builtin_image_type_available_p(args: Vec<Value>) -> EvalResult {
    expect_args("image-type-available-p", &args, 1)?;
    let type_name = match args[0].as_symbol_name() {
        Some(name) => name.to_string(),
        None => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[0]],
            ));
        }
    };
    Ok(Value::bool_val(is_supported_image_type(&type_name)))
}

/// (create-image FILE-OR-DATA &optional TYPE DATA-P &rest PROPS) -> image descriptor
///
/// Create an image descriptor (a list starting with `image`).
/// FILE-OR-DATA is a file name string or raw data string.
/// TYPE is a symbol like `png`, `jpeg`, etc.
/// DATA-P if non-nil means FILE-OR-DATA is raw image data, not a file name.
/// PROPS are additional property-list pairs (e.g. :width 100 :height 200).
///
/// Returns: (image :type TYPE :file FILE-OR-DATA ... PROPS)
pub(crate) fn builtin_create_image(args: Vec<Value>) -> EvalResult {
    expect_min_args("create-image", &args, 1)?;

    let file_or_data = args[0];
    let data_p = args.len() > 2 && args[2].is_truthy();

    // TYPE argument (optional).
    let image_type = if args.len() > 1 && !args[1].is_nil() {
        match args[1].as_symbol_name() {
            Some(name) => {
                let normalized = normalize_image_type_name(name).unwrap_or(name);
                Value::symbol(normalized)
            }
            None => {
                let rendered = super::print::print_value(&args[1]);
                return Err(signal(
                    "error",
                    vec![Value::string(format!("Invalid image type `{rendered}`"))],
                ));
            }
        }
    } else {
        let inferred = if data_p {
            None
        } else {
            file_or_data
                .as_str()
                .and_then(infer_image_type_from_filename)
                .map(str::to_string)
        };
        match inferred {
            Some(name) => Value::symbol(name),
            None => Value::NIL,
        }
    };

    // Build the image spec property list.
    let mut spec_items: Vec<Value> = Vec::new();
    spec_items.push(Value::symbol("image"));
    spec_items.push(Value::keyword("type"));
    spec_items.push(image_type);

    if data_p {
        spec_items.push(Value::keyword("data"));
        spec_items.push(file_or_data);
    } else {
        spec_items.push(Value::keyword("file"));
        spec_items.push(file_or_data);
    }

    // Emacs adds :scale default on freshly created image specs.
    spec_items.push(Value::keyword("scale"));
    spec_items.push(Value::symbol("default"));

    // Append any extra PROPS (starting from index 3).
    if args.len() > 3 {
        for prop in &args[3..] {
            spec_items.push(*prop);
        }
    }

    Ok(Value::list(spec_items))
}

/// (image-size SPEC &optional PIXELS FRAME) -> (WIDTH . HEIGHT)
///
/// Batch/no-window semantics:
/// - invalid SPEC -> `(error "Invalid image specification")`
/// - valid SPEC in batch -> `(error "Window system frame should be used")`
pub(crate) fn builtin_image_size(args: Vec<Value>) -> EvalResult {
    expect_min_args("image-size", &args, 1)?;
    expect_max_args("image-size", &args, 3)?;

    if !is_image_spec(&args[0]) {
        return Err(signal(
            "error",
            vec![Value::string("Invalid image specification")],
        ));
    }
    Err(signal(
        "error",
        vec![Value::string("Window system frame should be used")],
    ))
}

/// (image-mask-p SPEC &optional FRAME) -> nil
///
/// Batch/no-window semantics:
/// - invalid SPEC -> `(error "Invalid image specification")`
/// - valid SPEC in batch -> `(error "Window system frame should be used")`
pub(crate) fn builtin_image_mask_p(args: Vec<Value>) -> EvalResult {
    expect_min_args("image-mask-p", &args, 1)?;
    expect_max_args("image-mask-p", &args, 2)?;

    if !is_image_spec(&args[0]) {
        return Err(signal(
            "error",
            vec![Value::string("Invalid image specification")],
        ));
    }
    Err(signal(
        "error",
        vec![Value::string("Window system frame should be used")],
    ))
}

/// (put-image IMAGE POINT &optional STRING AREA) -> nil
///
/// Display IMAGE at POINT in the current buffer as an overlay.
/// Stub: does nothing, returns nil.
pub(crate) fn builtin_put_image(args: Vec<Value>) -> EvalResult {
    expect_min_args("put-image", &args, 2)?;
    expect_max_args("put-image", &args, 4)?;

    // Validate that first arg looks like an image spec.
    if !is_image_spec(&args[0]) {
        let rendered = super::print::print_value(&args[0]);
        return Err(signal(
            "error",
            vec![Value::string(format!("Not an image: {rendered}"))],
        ));
    }

    // Validate POINT is integer-or-marker in batch.
    if !args[1].is_fixnum() || args[1].is_char() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), args[1]],
        ));
    }

    // Optional AREA must be nil, left-margin, or right-margin.
    if args.len() > 3 && !args[3].is_nil() {
        let valid = matches!(
            args[3].as_symbol_name(),
            Some("left-margin") | Some("right-margin")
        );
        if !valid {
            let rendered = super::print::print_value(&args[3]);
            return Err(signal(
                "error",
                vec![Value::string(format!("Invalid area {rendered}"))],
            ));
        }
    }

    // Batch compatibility: return a truthy placeholder for inserted overlay.
    Ok(Value::T)
}

/// (insert-image IMAGE &optional STRING AREA SLICE) -> nil
///
/// Insert IMAGE into the current buffer at point.
/// Batch stub: validates IMAGE and returns t.
pub(crate) fn builtin_insert_image(args: Vec<Value>) -> EvalResult {
    expect_min_args("insert-image", &args, 1)?;
    expect_max_args("insert-image", &args, 5)?;

    if !is_image_spec(&args[0]) {
        let rendered = super::print::print_value(&args[0]);
        return Err(signal(
            "error",
            vec![Value::string(format!("Not an image: {rendered}"))],
        ));
    }

    // Optional AREA must be nil, left-margin, or right-margin.
    if args.len() > 2 && !args[2].is_nil() {
        let valid = matches!(
            args[2].as_symbol_name(),
            Some("left-margin") | Some("right-margin")
        );
        if !valid {
            let rendered = super::print::print_value(&args[2]);
            return Err(signal(
                "error",
                vec![Value::string(format!("Invalid area {rendered}"))],
            ));
        }
    }

    Ok(Value::T)
}

/// (remove-images START END &optional BUFFER) -> nil
///
/// Remove images between START and END in BUFFER.
/// Stub: does nothing, returns nil.
pub(crate) fn builtin_remove_images(args: Vec<Value>) -> EvalResult {
    expect_min_args("remove-images", &args, 2)?;
    expect_max_args("remove-images", &args, 3)?;

    // Validate START and END are integer-or-marker in batch.
    if !args[0].is_fixnum() || args[0].is_char() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), args[0]],
        ));
    }
    if !args[1].is_fixnum() || args[1].is_char() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), args[1]],
        ));
    }

    // Stub: no-op.
    Ok(Value::NIL)
}

/// (image-flush SPEC &optional FRAME) -> nil
///
/// Flush the image cache for image SPEC.
/// Batch semantics:
/// - invalid SPEC -> `(error "Invalid image specification")`
/// - FRAME = t -> nil (all-frames path)
/// - otherwise -> `(error "Window system frame should be used")`
pub(crate) fn builtin_image_flush(args: Vec<Value>) -> EvalResult {
    expect_min_args("image-flush", &args, 1)?;
    expect_max_args("image-flush", &args, 2)?;

    if !is_image_spec(&args[0]) {
        return Err(signal(
            "error",
            vec![Value::string("Invalid image specification")],
        ));
    }

    if let Some(frame) = args.get(1) {
        if frame.is_t() {
            return Ok(Value::NIL);
        }
        if !frame.is_nil() {
            expect_frame_designator("image-flush", frame)?;
        }
    }

    Err(signal(
        "error",
        vec![Value::string("Window system frame should be used")],
    ))
}

/// (clear-image-cache &optional FILTER) -> nil
///
/// Clear the image cache.  FILTER can be nil (clear all), a frame,
/// or t (clear all frames).
/// Stub: does nothing, returns nil.
pub(crate) fn builtin_clear_image_cache(args: Vec<Value>) -> EvalResult {
    if args.len() > 2 {
        return Err(signal(
            "wrong-number-of-arguments",
            vec![
                Value::symbol("clear-image-cache"),
                Value::fixnum(args.len() as i64),
            ],
        ));
    }

    if args.len() == 2 {
        let animation_cache = &args[1];
        if !animation_cache.is_nil() && !animation_cache.is_cons() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), *animation_cache],
            ));
        }
        // When animation-cache is non-nil, Emacs does not validate `filter`.
    }

    if args.is_empty() {
        return Err(signal(
            "error",
            vec![Value::string("Window system frame should be used")],
        ));
    }

    if args[0].is_nil() {
        return Err(signal(
            "error",
            vec![Value::string("Window system frame should be used")],
        ));
    }

    Ok(Value::NIL)
}

/// (image-cache-size) -> integer
///
/// NeoVM currently has no persistent image cache, so this is always 0.
pub(crate) fn builtin_image_cache_size(args: Vec<Value>) -> EvalResult {
    expect_args("image-cache-size", &args, 0)?;
    Ok(Value::fixnum(0))
}

/// (image-metadata SPEC &optional FRAME) -> metadata object or nil
///
/// Returns nil for non-image specifications. For valid image specs on
/// non-window-system frames, this signals the same error shape as GNU Emacs.
pub(crate) fn builtin_image_metadata(args: Vec<Value>) -> EvalResult {
    expect_range_args("image-metadata", &args, 1, 2)?;

    if !is_image_spec(&args[0]) {
        return Ok(Value::NIL);
    }

    if let Some(frame) = args.get(1) {
        expect_frame_designator("image-metadata", frame)?;
    }

    Err(signal(
        "error",
        vec![Value::string("Window system frame should be used")],
    ))
}

/// (imagep OBJECT) -> t if OBJECT looks like an image descriptor.
pub(crate) fn builtin_imagep(args: Vec<Value>) -> EvalResult {
    expect_args("imagep", &args, 1)?;
    Ok(Value::bool_val(is_image_spec(&args[0])))
}

/// (image-type SOURCE &optional TYPE DATA-P) -> symbol
///
/// Compatibility behavior:
/// - SOURCE must be a file name string.
/// - TYPE, when non-nil, must be a symbol and is returned (normalized aliases).
/// - Without TYPE, type is inferred from file extension.
/// - If type inference fails, signal `unknown-image-type`.
pub(crate) fn builtin_image_type(args: Vec<Value>) -> EvalResult {
    expect_min_args("image-type", &args, 1)?;
    expect_max_args("image-type", &args, 3)?;

    let source = &args[0];
    let explicit_type = args.get(1).cloned().unwrap_or(Value::NIL);
    let data_p = args.get(2).cloned().unwrap_or(Value::NIL);

    if source.as_str().is_none() {
        let rendered = super::print::print_value(source);
        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Invalid image file name `{rendered}`"
            ))],
        ));
    }

    let resolved = if explicit_type.is_nil() {
        if data_p.is_truthy() {
            None
        } else {
            source
                .as_str()
                .and_then(infer_image_type_from_filename)
                .map(str::to_string)
        }
    } else {
        let rendered = super::print::print_value(&explicit_type);
        let sym_name = explicit_type.as_symbol_name().ok_or_else(|| {
            signal(
                "error",
                vec![Value::string(format!("Invalid image type `{rendered}`"))],
            )
        })?;
        Some(
            normalize_image_type_name(sym_name)
                .unwrap_or(sym_name)
                .to_string(),
        )
    };

    let Some(resolved) = resolved else {
        return Err(signal(
            "unknown-image-type",
            vec![Value::list(vec![Value::string(
                "Cannot determine image type",
            )])],
        ));
    };

    Ok(Value::symbol(resolved))
}

/// (image-transforms-p &optional FRAME) -> bool
///
/// Return nil if FRAME does not match an active frame designator in Neovm.
/// (Compatibility layer keeps this conservative and follows official Emacs semantics
/// observed for common callers.)
pub(crate) fn builtin_image_transforms_p(args: Vec<Value>) -> EvalResult {
    expect_max_args("image-transforms-p", &args, 1)?;
    if let Some(frame_or_display) = args.first() {
        expect_frame_designator("image-transforms-p", frame_or_display)?;
    }
    Ok(Value::NIL)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "image_test.rs"]
mod tests;
