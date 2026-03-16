//! Font and face builtins for the Elisp interpreter.
//!
//! Font builtins:
//! - `fontp`, `font-spec`, `font-get`, `font-put`, `list-fonts`, `find-font`,
//!   `clear-font-cache`, `font-family-list`, `font-xlfd-name`
//!
//! Face builtins:
//! - `internal-make-lisp-face`, `internal-lisp-face-p`, `internal-copy-lisp-face`,
//!   `internal-set-lisp-face-attribute`, `internal-get-lisp-face-attribute`,
//!   `internal-merge-in-global-face`, `face-attribute-relative-p`,
//!   `merge-face-attribute`, `face-list`, `color-defined-p`, `color-values`,
//!   `defined-colors`, `face-id`, `face-font`, `internal-face-x-get-resource`,
//!   `internal-set-font-selection-order`,
//!   `internal-set-alternative-font-family-alist`,
//!   `internal-set-alternative-font-registry-alist`

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use super::error::{EvalResult, Flow, signal};
use super::intern::{intern, resolve_sym};
use super::value::*;
use crate::window::{FRAME_ID_BASE, FrameId, FrameManager};

// ---------------------------------------------------------------------------
// Argument helpers (local to this module)
// ---------------------------------------------------------------------------

fn expect_args(name: &str, args: &[Value], n: usize) -> Result<(), Flow> {
    if args.len() != n {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_max_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    if args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn live_frame_designator_in_state(frames: &FrameManager, value: &Value) -> bool {
    match value {
        Value::Int(id) if *id >= 0 => frames.get(FrameId(*id as u64)).is_some(),
        Value::Frame(id) => frames.get(FrameId(*id)).is_some(),
        _ => false,
    }
}

fn expect_optional_frame_designator_in_state(
    frames: &FrameManager,
    value: Option<&Value>,
) -> Result<(), Flow> {
    if let Some(frame) = value {
        if !frame.is_nil() && !live_frame_designator_in_state(frames, frame) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *frame],
            ));
        }
    }
    Ok(())
}

fn frame_device_designator_p(value: &Value) -> bool {
    match value {
        Value::Int(id) => *id >= FRAME_ID_BASE as i64,
        Value::Frame(id) => *id >= FRAME_ID_BASE,
        _ => false,
    }
}

fn optional_selected_frame_designator_p(value: &Value) -> bool {
    value.is_nil() || frame_device_designator_p(value)
}

// ---------------------------------------------------------------------------
// Font-spec helpers
// ---------------------------------------------------------------------------

/// The tag keyword used to identify font-spec vectors: `:font-spec`.
const FONT_SPEC_TAG: &str = "font-spec";

/// Check whether a Value is a font-spec (a vector whose first element is
/// the keyword `:font-spec`).
fn is_font_spec(val: &Value) -> bool {
    match val {
        Value::Vector(v) => {
            let elems = with_heap(|h| h.get_vector(*v).clone());
            if elems.is_empty() {
                return false;
            }
            matches!(&elems[0], Value::Keyword(k) if resolve_sym(*k) == FONT_SPEC_TAG)
        }
        _ => false,
    }
}

/// Check whether a value is represented as a font-object vector.
fn is_font_object(val: &Value) -> bool {
    match val {
        Value::Vector(v) => {
            let elems = with_heap(|h| h.get_vector(*v).clone());
            matches!(&elems.first(), Some(Value::Keyword(tag)) if resolve_sym(*tag) == "font-object")
        }
        _ => false,
    }
}

/// Extract a property from a font-spec vector.
///
/// Property lookup is strict: keys only match if they are exactly equal to
/// `prop` (keyword vs symbol distinction is preserved).
fn font_spec_get(vec_elems: &[Value], prop: &Value) -> Value {
    // Skip the tag at index 0; scan remaining pairs.
    let mut i = 1;
    while i + 1 < vec_elems.len() {
        if vec_elems[i] == *prop {
            return vec_elems[i + 1];
        }
        i += 2;
    }
    Value::Nil
}

/// Get a property from a font-spec while accepting both `family` and `:family`
/// style keys, and both keyword and symbol keys.
fn font_spec_get_flexible(vec_elems: &[Value], prop: &str) -> Option<Value> {
    let prop_norm = prop.trim_start_matches(':');
    let mut i = 1;
    while i + 1 < vec_elems.len() {
        let key = &vec_elems[i];
        let key_text = match key {
            Value::Keyword(k) => resolve_sym(*k),
            Value::Symbol(k) => resolve_sym(*k),
            _ => {
                i += 2;
                continue;
            }
        };
        let key_norm = key_text.trim_start_matches(':');
        if key_norm == prop_norm {
            return Some(vec_elems[i + 1]);
        }
        i += 2;
    }
    None
}

fn font_spec_field_to_string(value: &Value) -> String {
    match value {
        Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
        Value::Symbol(id) | Value::Keyword(id) => resolve_sym(*id).to_owned(),
        _ => "*".to_string(),
    }
}

fn xlfd_size_field(size_val: &Value) -> Option<String> {
    match size_val {
        Value::Int(size) => {
            if *size > 0 {
                Some(format!("{}-*", size))
            } else {
                Some("*-*".to_string())
            }
        }
        Value::Float(size, _) => {
            let scaled = size * 10.0;
            if scaled.is_finite() {
                Some(format!("*-{}", scaled.round() as i64))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn fold_xlfd_wildcards(mut name: String) -> String {
    while let Some(pos) = name.find("-*-*") {
        name.replace_range(pos + 1..pos + 3, "");
    }
    name
}

fn normalize_registry_field(value: &Option<Value>) -> String {
    match value {
        None => "*-*".to_string(),
        Some(Value::Str(id)) => {
            let s = with_heap(|h| h.get_string(*id).to_owned());
            if !s.contains('-') {
                format!("{}-*", s)
            } else {
                s
            }
        }
        Some(Value::Symbol(id)) | Some(Value::Keyword(id)) => {
            let s = resolve_sym(*id);
            if !s.contains('-') {
                format!("{}-*", s)
            } else {
                s.to_owned()
            }
        }
        _ => "*-*".to_string(),
    }
}

fn sanitize_style_field(value: &Value) -> String {
    match value {
        Value::Symbol(id) => resolve_sym(*id)
            .chars()
            .filter(|ch| *ch != '-' && *ch != '?' && *ch != ',' && *ch != '"')
            .collect(),
        Value::Keyword(id) => resolve_sym(*id)
            .chars()
            .filter(|ch| *ch != '-' && *ch != '?' && *ch != ',' && *ch != '"')
            .collect(),
        Value::Str(id) => {
            let s = with_heap(|h| h.get_string(*id).to_owned());
            s.chars()
                .filter(|ch| *ch != '-' && *ch != '?' && *ch != ',' && *ch != '"')
                .collect()
        }
        _ => "*".to_string(),
    }
}

fn spacing_field(value: Option<&Value>) -> String {
    match value {
        None => "*".to_string(),
        Some(Value::Int(spacing)) => {
            let value = *spacing;
            if value <= 0 {
                "p".to_string()
            } else if value <= 1 {
                "d".to_string()
            } else if value <= 2 {
                "m".to_string()
            } else {
                "c".to_string()
            }
        }
        Some(value) => sanitize_style_field(value),
    }
}

fn avg_width_field(value: Option<&Value>) -> String {
    match value {
        Some(Value::Int(n)) => n.to_string(),
        Some(Value::Str(id)) => with_heap(|h| h.get_string(*id).to_owned()),
        Some(Value::Symbol(id)) | Some(Value::Keyword(id)) => resolve_sym(*id).to_owned(),
        _ => "*".to_string(),
    }
}

fn xlfd_pixel_field(size: Option<&Value>) -> String {
    match size {
        Some(value) => xlfd_size_field(value).unwrap_or("*-*".to_string()),
        None => "*-*".to_string(),
    }
}

fn xlfd_resolution_field(dpi: Option<&Value>) -> String {
    match dpi {
        Some(Value::Int(size)) => format!("{}-{}", size, size),
        _ => "*-*".to_string(),
    }
}

fn xlfd_fields_from_font_spec(
    v: &[Value],
) -> (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
) {
    let foundry = font_spec_get_flexible(v, "foundry")
        .map(|value| font_spec_field_to_string(&value))
        .unwrap_or_else(|| "*".to_string());
    let family = font_spec_get_flexible(v, "family")
        .map(|value| font_spec_field_to_string(&value))
        .unwrap_or_else(|| "*".to_string());
    let weight = font_spec_get_flexible(v, "weight")
        .map(|value| sanitize_style_field(&value))
        .unwrap_or_else(|| "*".to_string());
    let slant = font_spec_get_flexible(v, "slant")
        .map(|value| sanitize_style_field(&value))
        .unwrap_or_else(|| "*".to_string());
    let set_width = font_spec_get_flexible(v, "set-width")
        .or_else(|| font_spec_get_flexible(v, "setwidth"))
        .map(|value| font_spec_field_to_string(&value))
        .unwrap_or_else(|| "*".to_string());
    let adstyle = font_spec_get_flexible(v, "adstyle")
        .map(|value| font_spec_field_to_string(&value))
        .unwrap_or_else(|| "*".to_string());

    let size = font_spec_get_flexible(v, "size");
    let dpi = font_spec_get_flexible(v, "dpi");
    let spacing = font_spec_get_flexible(v, "spacing");
    let avg_width = font_spec_get_flexible(v, "average_width")
        .or_else(|| font_spec_get_flexible(v, "avg_width"))
        .or_else(|| font_spec_get_flexible(v, "avg-width"));
    let registry = font_spec_get_flexible(v, "registry");

    let pixel = xlfd_pixel_field(size.as_ref());
    let resx = xlfd_resolution_field(dpi.as_ref());
    let spacing = spacing_field(spacing.as_ref());
    let avg_width = avg_width_field(avg_width.as_ref());
    let registry = normalize_registry_field(&registry);

    (
        foundry, family, weight, slant, set_width, adstyle, pixel, resx, spacing, avg_width,
        registry,
    )
}

/// Set (or add) a property in a font-spec in place.
fn font_spec_put(vec_elems: &mut Vec<Value>, prop: &Value, val: &Value) {
    let mut i = 1;
    while i + 1 < vec_elems.len() {
        if vec_elems[i] == *prop {
            vec_elems[i + 1] = *val;
            return;
        }
        i += 2;
    }
    vec_elems.push(*prop);
    vec_elems.push(*val);
}

// ===========================================================================
// Font builtins (pure)
// ===========================================================================

/// `(fontp OBJECT &optional EXTRA-TYPE)` -- return t if OBJECT is a font-spec,
/// font-entity, or font-object.  We represent all of these as tagged vectors
/// with `:font-spec` keyword at position 0.
pub(crate) fn builtin_fontp(args: Vec<Value>) -> EvalResult {
    expect_max_args("fontp", &args, 2)?;
    expect_min_args("fontp", &args, 1)?;
    // Ignore EXTRA-TYPE for now; just check the tag.
    Ok(Value::bool(is_font_spec(&args[0])))
}

/// `(font-spec &rest ARGS)` -- create a font spec from keyword args.
///
/// Usage: `(font-spec :family "Monospace" :weight 'normal :size 12)`
///
/// Returns a vector `[:font-spec :family "Monospace" :weight normal :size 12]`.
pub(crate) fn builtin_font_spec(args: Vec<Value>) -> EvalResult {
    let mut elems: Vec<Value> = Vec::with_capacity(1 + args.len());
    elems.push(Value::Keyword(intern(FONT_SPEC_TAG)));

    for pair_index in (0..args.len()).step_by(2) {
        let key = &args[pair_index];
        let value = args.get(pair_index + 1);

        let Some(value) = value else {
            if matches!(key, Value::Keyword(_) | Value::Symbol(_) | Value::Nil) {
                let key_name = match key {
                    Value::Keyword(k) => format!(":{}", resolve_sym(*k)),
                    Value::Symbol(id) => resolve_sym(*id).to_owned(),
                    Value::Nil => "nil".to_string(),
                    _ => "nil".to_string(),
                };
                return Err(signal(
                    "error",
                    vec![Value::string(format!("No value for key ‘{}’", key_name))],
                ));
            }
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), *key],
            ));
        };

        if matches!(key, Value::Nil) {
            return Err(signal(
                "error",
                vec![
                    Value::string("invalid font property"),
                    Value::list(vec![Value::cons(Value::keyword("type"), *value)]),
                ],
            ));
        }

        if !matches!(key, Value::Keyword(_) | Value::Symbol(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), *key],
            ));
        }

        elems.push(*key);
        elems.push(*value);
    }

    Ok(Value::vector(elems))
}

/// `(font-get FONT PROP)` -- get a property value from a font-spec.
pub(crate) fn builtin_font_get(args: Vec<Value>) -> EvalResult {
    expect_args("font-get", &args, 2)?;
    if !is_font_spec(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("font"), args[0]],
        ));
    }
    if !matches!(&args[1], Value::Keyword(_) | Value::Symbol(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[1]],
        ));
    }

    match &args[0] {
        Value::Vector(v) => {
            let elems = with_heap(|h| h.get_vector(*v).clone());
            Ok(font_spec_get(&elems, &args[1]))
        }
        _ => unreachable!("font-spec check above guarantees vector"),
    }
}

/// `(font-put FONT PROP VAL)` -- set a property in a font-spec and return VAL.
pub(crate) fn builtin_font_put(args: Vec<Value>) -> EvalResult {
    expect_args("font-put", &args, 3)?;
    if !is_font_spec(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("font-spec"), args[0]],
        ));
    }
    match &args[0] {
        Value::Vector(v) => {
            with_heap_mut(|h| {
                let elems = h.get_vector_mut(*v);
                font_spec_put(elems, &args[1], &args[2]);
            });
            Ok(args[2])
        }
        _ => unreachable!("font-spec check above guarantees vector"),
    }
}

/// `(list-fonts FONT-SPEC &optional FRAME MAXNUM PREFER)` -- returns nil in
/// batch-compatible mode.
pub(crate) fn builtin_list_fonts(args: Vec<Value>) -> EvalResult {
    expect_min_args("list-fonts", &args, 1)?;
    expect_max_args("list-fonts", &args, 4)?;
    if !is_font_spec(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("font-spec"), args[0]],
        ));
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_list_fonts_in_state(frames: &FrameManager, args: Vec<Value>) -> EvalResult {
    expect_min_args("list-fonts", &args, 1)?;
    expect_max_args("list-fonts", &args, 4)?;
    if !is_font_spec(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("font-spec"), args[0]],
        ));
    }
    expect_optional_frame_designator_in_state(frames, args.get(1))?;
    Ok(Value::Nil)
}

/// Evaluator-aware variant of `list-fonts`.
///
/// Accepts live frame designators in the optional FRAME slot.
pub(crate) fn builtin_list_fonts_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_list_fonts_in_state(&eval.frames, args)
}

/// `(find-font FONT-SPEC &optional FRAME)` -- returns nil in
/// batch-compatible mode.
pub(crate) fn builtin_find_font(args: Vec<Value>) -> EvalResult {
    expect_min_args("find-font", &args, 1)?;
    expect_max_args("find-font", &args, 2)?;
    if !is_font_spec(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("font-spec"), args[0]],
        ));
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_find_font_in_state(frames: &FrameManager, args: Vec<Value>) -> EvalResult {
    expect_min_args("find-font", &args, 1)?;
    expect_max_args("find-font", &args, 2)?;
    if !is_font_spec(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("font-spec"), args[0]],
        ));
    }
    expect_optional_frame_designator_in_state(frames, args.get(1))?;
    Ok(Value::Nil)
}

/// Evaluator-aware variant of `find-font`.
///
/// Accepts live frame designators in the optional FRAME slot.
pub(crate) fn builtin_find_font_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_find_font_in_state(&eval.frames, args)
}

/// `(clear-font-cache)` -- reset internal font/face caches and return nil.
pub(crate) fn builtin_clear_font_cache(args: Vec<Value>) -> EvalResult {
    expect_max_args("clear-font-cache", &args, 0)?;
    clear_font_cache_state();
    Ok(Value::Nil)
}

/// `(font-family-list &optional FRAME)` -- returns nil in batch-compatible mode.
pub(crate) fn builtin_font_family_list(args: Vec<Value>) -> EvalResult {
    expect_max_args("font-family-list", &args, 1)?;
    if let Some(frame) = args.first() {
        if !frame.is_nil() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *frame],
            ));
        }
    }
    Ok(Value::Nil)
}

pub(crate) fn builtin_font_family_list_in_state(
    frames: &FrameManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("font-family-list", &args, 1)?;
    expect_optional_frame_designator_in_state(frames, args.first())?;
    Ok(Value::Nil)
}

/// Evaluator-aware variant of `font-family-list`.
///
/// Accepts live frame designators in the optional FRAME slot.
pub(crate) fn builtin_font_family_list_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_font_family_list_in_state(&eval.frames, args)
}

/// `(font-xlfd-name FONT &optional FOLD-WILDCARDS)` -- render font-spec fields
/// into an XLFD string; wildcard folding is supported in compatibility mode.
pub(crate) fn builtin_font_xlfd_name(args: Vec<Value>) -> EvalResult {
    expect_min_args("font-xlfd-name", &args, 1)?;
    expect_max_args("font-xlfd-name", &args, 3)?;
    if !is_font_spec(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("font"), args[0]],
        ));
    }

    let fields = match &args[0] {
        Value::Vector(v) => {
            let elems = with_heap(|h| h.get_vector(*v).clone());
            xlfd_fields_from_font_spec(&elems)
        }
        _ => (
            "*".to_string(),
            "*".to_string(),
            "*".to_string(),
            "*".to_string(),
            "*".to_string(),
            "*".to_string(),
            "*-*".to_string(),
            "*-*".to_string(),
            "*".to_string(),
            "*".to_string(),
            "*-*".to_string(),
        ),
    };

    let (
        foundry,
        family,
        weight,
        slant,
        set_width,
        adstyle,
        pixel,
        resx,
        spacing,
        avg_width,
        registry,
    ) = fields;
    let rendered = if args.get(1).is_some_and(Value::is_truthy) {
        let name = format!(
            "-{}-{}-{}-{}-{}-{}-{}-{}-{}-{}-{}",
            foundry,
            family,
            weight,
            slant,
            set_width,
            adstyle,
            pixel,
            resx,
            spacing,
            avg_width,
            registry
        );
        fold_xlfd_wildcards(name)
    } else {
        format!(
            "-{}-{}-{}-{}-{}-{}-{}-{}-{}-{}-{}",
            foundry,
            family,
            weight,
            slant,
            set_width,
            adstyle,
            pixel,
            resx,
            spacing,
            avg_width,
            registry
        )
    };
    Ok(Value::string(rendered))
}

/// `(close-font FONT-OBJECT &optional FRAME)` -- close an open font object.
///
/// NeoVM currently has no runtime font-object handles, so this validates the
/// argument shape and returns nil for accepted objects.
pub(crate) fn builtin_close_font(args: Vec<Value>) -> EvalResult {
    expect_min_args("close-font", &args, 1)?;
    expect_max_args("close-font", &args, 2)?;
    if !is_font_object(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("font-object"), args[0]],
        ));
    }
    Ok(Value::Nil)
}

// ===========================================================================
// Face builtins (pure)
// ===========================================================================

/// Well-known face names returned by `face-list` and recognised by
/// `internal-lisp-face-p`.
const KNOWN_FACES: &[&str] = &[
    "default",
    "bold",
    "italic",
    "underline",
    "fixed-pitch",
    "variable-pitch",
    "highlight",
    "region",
    "mode-line",
    "mode-line-highlight",
    "mode-line-emphasis",
    "mode-line-buffer-id",
    "mode-line-inactive",
    "header-line",
    "header-line-highlight",
    "header-line-active",
    "header-line-inactive",
    "fringe",
    "vertical-border",
    "scroll-bar",
    "border",
    "internal-border",
    "child-frame-border",
    "cursor",
    "mouse",
    "tool-bar",
    "tab-bar",
    "tab-line",
];
const FIRST_DYNAMIC_FACE_ID: i64 = 133;

fn known_face_id(name: &str) -> Option<i64> {
    match name {
        "default" => Some(0),
        "bold" => Some(1),
        "italic" => Some(2),
        "underline" => Some(4),
        "highlight" => Some(12),
        "region" => Some(13),
        "mode-line" => Some(25),
        "mode-line-inactive" => Some(27),
        "fringe" => Some(40),
        "cursor" => Some(43),
        _ => None,
    }
}

const LISP_FACE_VECTOR_LEN: usize = 20;
const VALID_FACE_ATTRIBUTES: &[&str] = &[
    ":family",
    ":foundry",
    ":height",
    ":weight",
    ":slant",
    ":underline",
    ":overline",
    ":strike-through",
    ":box",
    ":inverse-video",
    ":foreground",
    ":distant-foreground",
    ":background",
    ":stipple",
    ":width",
    ":inherit",
    ":extend",
    ":font",
    ":fontset",
];
const LISP_FACE_VECTOR_ATTRIBUTES: &[&str] = &[
    ":family",
    ":foundry",
    ":width",
    ":height",
    ":weight",
    ":slant",
    ":underline",
    ":inverse-video",
    ":foreground",
    ":background",
    ":stipple",
    ":overline",
    ":strike-through",
    ":box",
    ":font",
    ":inherit",
    ":extend",
    ":distant-foreground",
    ":fontset",
];
const DISCRETE_BOOLEAN_FACE_ATTRIBUTES: &[&str] = &[
    ":underline",
    ":overline",
    ":strike-through",
    ":inverse-video",
    ":extend",
];
const SET_ONLY_FACE_ATTRIBUTES: &[&str] = &[":bold", ":italic"];
const VALID_FACE_WEIGHTS: &[&str] = &[
    "ultra-light",
    "extra-light",
    "light",
    "semi-light",
    "normal",
    "semi-bold",
    "bold",
    "extra-bold",
    "ultra-bold",
];
const VALID_FACE_SLANTS: &[&str] = &[
    "normal",
    "italic",
    "oblique",
    "reverse-italic",
    "reverse-oblique",
];
const VALID_FACE_WIDTHS: &[&str] = &[
    "ultra-condensed",
    "extra-condensed",
    "condensed",
    "semi-condensed",
    "normal",
    "semi-expanded",
    "expanded",
    "extra-expanded",
    "ultra-expanded",
];

#[derive(Default)]
struct FaceAttrState {
    selected_created: HashSet<String>,
    selected_overrides: HashMap<String, HashMap<String, Value>>,
    defaults_overrides: HashMap<String, HashMap<String, Value>>,
}

thread_local! {
    static CREATED_LISP_FACES: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static CREATED_FACE_IDS: RefCell<HashMap<String, i64>> = RefCell::new(HashMap::new());
    static NEXT_CREATED_FACE_ID: RefCell<i64> = RefCell::new(FIRST_DYNAMIC_FACE_ID);
    static FACE_ATTR_STATE: RefCell<FaceAttrState> = RefCell::new(FaceAttrState::default());
}

pub(crate) fn clear_font_cache_state() {
    CREATED_LISP_FACES.with(|slot| slot.borrow_mut().clear());
    CREATED_FACE_IDS.with(|slot| slot.borrow_mut().clear());
    NEXT_CREATED_FACE_ID.with(|slot| *slot.borrow_mut() = FIRST_DYNAMIC_FACE_ID);
    FACE_ATTR_STATE.with(|slot| *slot.borrow_mut() = FaceAttrState::default());
}

/// Collect GC roots from face attribute overrides.
pub(crate) fn collect_font_gc_roots(roots: &mut Vec<Value>) {
    FACE_ATTR_STATE.with(|slot| {
        let state = slot.borrow();
        for attrs in state.selected_overrides.values() {
            roots.extend(attrs.values().copied());
        }
        for attrs in state.defaults_overrides.values() {
            roots.extend(attrs.values().copied());
        }
    });
}

fn is_created_lisp_face(name: &str) -> bool {
    CREATED_LISP_FACES.with(|slot| slot.borrow().contains(name))
}

fn mark_created_lisp_face(name: &str) {
    let inserted = CREATED_LISP_FACES.with(|slot| slot.borrow_mut().insert(name.to_string()));
    if inserted {
        ensure_dynamic_face_id(name);
    }
}

fn ensure_dynamic_face_id(name: &str) {
    if known_face_id(name).is_some() {
        return;
    }
    CREATED_FACE_IDS.with(|slot| {
        let mut ids = slot.borrow_mut();
        if ids.contains_key(name) {
            return;
        }
        NEXT_CREATED_FACE_ID.with(|next_slot| {
            let mut next = next_slot.borrow_mut();
            ids.insert(name.to_string(), *next);
            *next += 1;
        });
    });
}

fn dynamic_face_id(name: &str) -> Option<i64> {
    CREATED_FACE_IDS.with(|slot| slot.borrow().get(name).copied())
}

fn face_id_for_name(name: &str) -> Option<i64> {
    if let Some(id) = known_face_id(name) {
        return Some(id);
    }
    if KNOWN_FACES.contains(&name) {
        ensure_dynamic_face_id(name);
    }
    dynamic_face_id(name)
}

fn is_selected_created_lisp_face(name: &str) -> bool {
    FACE_ATTR_STATE.with(|slot| slot.borrow().selected_created.contains(name))
}

fn mark_selected_created_lisp_face(name: &str) {
    FACE_ATTR_STATE.with(|slot| {
        slot.borrow_mut().selected_created.insert(name.to_string());
    });
}

fn face_exists_for_domain(name: &str, defaults_frame: bool) -> bool {
    if KNOWN_FACES.contains(&name) {
        return true;
    }
    if defaults_frame {
        is_created_lisp_face(name)
    } else {
        is_selected_created_lisp_face(name)
    }
}

fn get_face_override(face_name: &str, attr: &str, defaults_frame: bool) -> Option<Value> {
    FACE_ATTR_STATE.with(|slot| {
        let state = slot.borrow();
        let map = if defaults_frame {
            &state.defaults_overrides
        } else {
            &state.selected_overrides
        };
        map.get(face_name)
            .and_then(|attrs| attrs.get(attr))
            .copied()
    })
}

fn set_face_override(face_name: &str, attr: &str, value: Value, defaults_frame: bool) {
    FACE_ATTR_STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        let map = if defaults_frame {
            &mut state.defaults_overrides
        } else {
            &mut state.selected_overrides
        };
        map.entry(face_name.to_string())
            .or_default()
            .insert(attr.to_string(), value);
    });
}

fn clear_face_overrides(face_name: &str, defaults_frame: bool) {
    FACE_ATTR_STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        if defaults_frame {
            state.defaults_overrides.remove(face_name);
        } else {
            state.selected_overrides.remove(face_name);
        }
    });
}

fn copy_defaults_overrides(src: &str, dst: &str) {
    FACE_ATTR_STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        let copied = state.defaults_overrides.get(src).cloned();
        if let Some(attrs) = copied {
            state.defaults_overrides.insert(dst.to_string(), attrs);
        } else {
            state.defaults_overrides.remove(dst);
        }
    });
}

fn merge_defaults_overrides_into_selected(face_name: &str) {
    FACE_ATTR_STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        let defaults = state.defaults_overrides.get(face_name).cloned();
        if let Some(attrs) = defaults {
            let selected = state
                .selected_overrides
                .entry(face_name.to_string())
                .or_default();
            for (attr, value) in attrs {
                if matches!(&value, Value::Symbol(id) if resolve_sym(*id) == "unspecified" || resolve_sym(*id) == "relative") {
                    continue;
                }
                selected.insert(attr, value);
            }
        }
    });
}

fn symbol_name_for_face_value(face: &Value) -> Option<String> {
    match face {
        Value::Nil => Some("nil".to_string()),
        Value::True => Some("t".to_string()),
        Value::Symbol(id) => Some(resolve_sym(*id).to_owned()),
        _ => None,
    }
}

fn require_symbol_face_name(face: &Value) -> Result<String, Flow> {
    symbol_name_for_face_value(face)
        .ok_or_else(|| signal("wrong-type-argument", vec![Value::symbol("symbolp"), *face]))
}

fn known_face_name(face: &Value) -> Option<String> {
    let name = match face {
        Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
        _ => symbol_name_for_face_value(face)?,
    };
    if KNOWN_FACES.contains(&name.as_str()) || is_created_lisp_face(&name) {
        Some(name)
    } else {
        None
    }
}

fn resolve_copy_source_face_symbol(face: &Value) -> Result<String, Flow> {
    let name = symbol_name_for_face_value(face).expect("checked symbol before resolve");
    if KNOWN_FACES.contains(&name.as_str()) || is_created_lisp_face(&name) {
        return Ok(name);
    }
    if face.is_nil() {
        return Err(signal("error", vec![Value::string("Invalid face")]));
    }
    Err(signal("error", vec![Value::string("Invalid face"), *face]))
}

fn resolve_face_name_for_domain(face: &Value, defaults_frame: bool) -> Result<String, Flow> {
    match face {
        Value::Str(id) => {
            let name = with_heap(|h| h.get_string(*id).to_owned());
            if face_exists_for_domain(&name, defaults_frame) {
                Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("symbolp"), *face],
                ))
            } else {
                Err(signal(
                    "error",
                    vec![Value::string("Invalid face"), Value::symbol(&name)],
                ))
            }
        }
        Value::Nil | Value::True | Value::Symbol(_) => {
            let name = symbol_name_for_face_value(face).expect("symbol-like");
            if face_exists_for_domain(&name, defaults_frame) {
                Ok(name)
            } else if face.is_nil() {
                Err(signal("error", vec![Value::string("Invalid face")]))
            } else {
                Err(signal("error", vec![Value::string("Invalid face"), *face]))
            }
        }
        _ => Err(signal("error", vec![Value::string("Invalid face"), *face])),
    }
}

fn resolve_face_name_for_merge(face: &Value) -> Result<String, Flow> {
    match face {
        Value::Str(id) => {
            let name = with_heap(|h| h.get_string(*id).to_owned());
            if face_exists_for_domain(&name, true) {
                Ok(name)
            } else {
                Err(signal(
                    "error",
                    vec![Value::string("Invalid face"), Value::symbol(&name)],
                ))
            }
        }
        Value::Nil | Value::True | Value::Symbol(_) => {
            let name = symbol_name_for_face_value(face).expect("symbol-like");
            if face_exists_for_domain(&name, true) {
                Ok(name)
            } else if face.is_nil() {
                Err(signal("error", vec![Value::string("Invalid face")]))
            } else {
                Err(signal("error", vec![Value::string("Invalid face"), *face]))
            }
        }
        _ => Err(signal("error", vec![Value::string("Invalid face"), *face])),
    }
}

fn make_lisp_face_vector() -> Value {
    let mut values = Vec::with_capacity(LISP_FACE_VECTOR_LEN);
    values.push(Value::symbol("face"));
    values.extend((1..LISP_FACE_VECTOR_LEN).map(|_| Value::symbol("unspecified")));
    Value::vector(values)
}

fn make_lisp_face_vector_for_domain(face_name: &str, defaults_frame: bool) -> Value {
    let mut values = Vec::with_capacity(LISP_FACE_VECTOR_LEN);
    values.push(Value::symbol("face"));
    values.extend(
        LISP_FACE_VECTOR_ATTRIBUTES
            .iter()
            .map(|attr| lisp_face_attribute_value(face_name, attr, defaults_frame)),
    );
    Value::vector(values)
}

fn normalize_face_attribute_name(attr: &Value) -> Result<String, Flow> {
    let name = match attr {
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        Value::Keyword(id) => {
            let s = resolve_sym(*id);
            if s.starts_with(':') {
                s.to_owned()
            } else {
                format!(":{s}")
            }
        }
        Value::Nil | Value::True => attr.as_symbol_name().unwrap_or_default().to_string(),
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), *attr],
            ));
        }
    };

    if VALID_FACE_ATTRIBUTES.contains(&name.as_str()) {
        Ok(name)
    } else if attr.is_nil() {
        Err(signal(
            "error",
            vec![Value::string("Invalid face attribute name")],
        ))
    } else {
        Err(signal(
            "error",
            vec![Value::string("Invalid face attribute name"), *attr],
        ))
    }
}

fn normalize_set_face_attribute_name(attr: &Value) -> Result<String, Flow> {
    let name = match attr {
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        Value::Keyword(id) => {
            let s = resolve_sym(*id);
            if s.starts_with(':') {
                s.to_owned()
            } else {
                format!(":{s}")
            }
        }
        Value::Nil | Value::True => attr.as_symbol_name().unwrap_or_default().to_string(),
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), *attr],
            ));
        }
    };

    if VALID_FACE_ATTRIBUTES.contains(&name.as_str())
        || SET_ONLY_FACE_ATTRIBUTES.contains(&name.as_str())
    {
        Ok(name)
    } else if attr.is_nil() {
        Err(signal(
            "error",
            vec![Value::string("Invalid face attribute name")],
        ))
    } else {
        Err(signal(
            "error",
            vec![Value::string("Invalid face attribute name"), *attr],
        ))
    }
}

fn default_face_attribute_value(attr: &str) -> Value {
    match attr {
        ":family" | ":foundry" => Value::string("default"),
        ":height" => Value::Int(1),
        ":weight" | ":slant" | ":width" => Value::symbol("normal"),
        ":underline" | ":overline" | ":strike-through" | ":box" | ":inverse-video" | ":stipple"
        | ":inherit" | ":extend" | ":fontset" => Value::Nil,
        ":foreground" => Value::string("unspecified-fg"),
        ":background" => Value::string("unspecified-bg"),
        ":distant-foreground" | ":font" => Value::symbol("unspecified"),
        _ => Value::symbol("unspecified"),
    }
}

fn lisp_face_attribute_base_value(face: &str, attr: &str, defaults_frame: bool) -> Value {
    if defaults_frame {
        return Value::symbol("unspecified");
    }
    if face == "default" {
        return default_face_attribute_value(attr);
    }
    match (face, attr) {
        ("bold", ":weight") => Value::symbol("bold"),
        ("italic", ":slant") => Value::symbol("italic"),
        ("underline", ":underline") => Value::True,
        ("highlight", ":inverse-video") => Value::True,
        ("region", ":inverse-video") => Value::True,
        ("mode-line", ":inverse-video") => Value::True,
        ("mode-line-highlight", ":inherit") => Value::symbol("highlight"),
        ("mode-line-emphasis", ":weight") => Value::symbol("bold"),
        ("mode-line-buffer-id", ":weight") => Value::symbol("bold"),
        ("mode-line-inactive", ":inherit") => Value::symbol("mode-line"),
        ("header-line", ":inherit") => Value::symbol("mode-line"),
        ("header-line-highlight", ":inherit") => Value::symbol("mode-line-highlight"),
        ("header-line-active", ":inherit") => Value::symbol("header-line"),
        ("header-line-inactive", ":inherit") => Value::symbol("header-line"),
        ("fringe", ":background") => Value::string("gray"),
        ("cursor", ":background") => Value::string("white"),
        ("vertical-border", ":inherit") => Value::symbol("mode-line-inactive"),
        ("tool-bar", ":foreground") => Value::string("black"),
        ("tool-bar", ":box") => Value::symbol("t"),
        ("tab-bar", ":inherit") => Value::symbol("variable-pitch"),
        ("tab-line", ":inherit") => Value::symbol("variable-pitch"),
        _ => Value::symbol("unspecified"),
    }
}

fn lisp_face_attribute_value(face: &str, attr: &str, defaults_frame: bool) -> Value {
    if let Some(value) = get_face_override(face, attr, defaults_frame) {
        return value;
    }
    lisp_face_attribute_base_value(face, attr, defaults_frame)
}

fn resolve_known_face_name_for_compare(face: &Value, defaults_frame: bool) -> Result<String, Flow> {
    match face {
        Value::Str(id) => {
            let name = with_heap(|h| h.get_string(*id).to_owned());
            if face_exists_for_domain(&name, defaults_frame) {
                Ok(name)
            } else {
                Err(signal(
                    "error",
                    vec![Value::string("Invalid face"), Value::symbol(&name)],
                ))
            }
        }
        _ => resolve_face_name_for_domain(face, defaults_frame),
    }
}

fn face_attr_value_name(attr: &Value) -> Result<String, Flow> {
    match attr {
        Value::Keyword(id) => {
            let s = resolve_sym(*id);
            if s.starts_with(':') {
                Ok(s.to_owned())
            } else {
                Ok(format!(":{s}"))
            }
        }
        Value::Symbol(id) => Ok(resolve_sym(*id).to_owned()),
        Value::Nil => Ok("nil".to_string()),
        Value::True => Ok("t".to_string()),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *attr],
        )),
    }
}

fn frame_defaults_flag(frame: Option<&Value>) -> Result<bool, Flow> {
    match frame {
        None => Ok(false),
        Some(v) if v.is_nil() => Ok(false),
        Some(Value::True) => Ok(true),
        Some(v) if frame_device_designator_p(v) => Ok(false),
        Some(v) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), *v],
        )),
    }
}

fn proper_list_to_vec_or_listp_error(value: &Value) -> Result<Vec<Value>, Flow> {
    let mut out = Vec::new();
    let mut cursor = *value;
    loop {
        match cursor {
            Value::Nil => return Ok(out),
            Value::Cons(cell) => {
                let cell = read_cons(cell);
                out.push(cell.car);
                cursor = cell.cdr;
            }
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), other],
                ));
            }
        }
    }
}

fn check_non_empty_string(value: &Value, empty_message: &str) -> Result<(), Flow> {
    match value {
        Value::Str(id) => {
            if with_heap(|h| h.get_string(*id).is_empty()) {
                Err(signal("error", vec![Value::string(empty_message), *value]))
            } else {
                Ok(())
            }
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )),
    }
}

fn symbol_name_or_type_error(value: &Value) -> Result<String, Flow> {
    match value {
        Value::Nil => Ok("nil".to_string()),
        Value::True => Ok("t".to_string()),
        Value::Symbol(id) => Ok(resolve_sym(*id).to_owned()),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *value],
        )),
    }
}

fn normalize_face_attr_for_set(
    face_name: &str,
    attr: &str,
    value: Value,
) -> Result<(String, Value), Flow> {
    let normalized = match attr {
        ":foreground" | ":background" | ":distant-foreground" if value.is_nil() => {
            Value::symbol("unspecified")
        }
        _ => value,
    };
    let is_reset_like = matches!(&normalized, Value::Symbol(id) if { let s = resolve_sym(*id); s == "unspecified" || s == ":ignore-defface" || s == "reset" });

    match attr {
        ":family" | ":foundry" => {
            if !is_reset_like {
                match &normalized {
                    Value::Str(id) if !with_heap(|h| h.get_string(*id).is_empty()) => {}
                    Value::Str(_) => {
                        let msg = if attr == ":family" {
                            "Invalid face family"
                        } else {
                            "Invalid face foundry"
                        };
                        return Err(signal("error", vec![Value::string(msg), normalized]));
                    }
                    _ => {
                        return Err(signal(
                            "wrong-type-argument",
                            vec![Value::symbol("stringp"), normalized],
                        ));
                    }
                }
            }
        }
        ":height" => {
            if !is_reset_like {
                if face_name == "default" {
                    match &normalized {
                        Value::Int(n) if *n > 0 => {}
                        _ => {
                            return Err(signal(
                                "error",
                                vec![
                                    Value::string("Default face height not absolute and positive"),
                                    normalized,
                                ],
                            ));
                        }
                    }
                } else {
                    match &normalized {
                        Value::Int(n) if *n > 0 => {}
                        Value::Float(f, _) if *f > 0.0 => {}
                        _ => {
                            return Err(signal(
                                "error",
                                vec![
                                    Value::string(
                                        "Face height does not produce a positive integer",
                                    ),
                                    normalized,
                                ],
                            ));
                        }
                    }
                }
            }
        }
        ":weight" => {
            if !is_reset_like {
                let sym = symbol_name_or_type_error(&normalized)?;
                if !VALID_FACE_WEIGHTS.contains(&sym.as_str()) {
                    return Err(signal(
                        "error",
                        vec![Value::string("Invalid face weight"), normalized],
                    ));
                }
            }
        }
        ":slant" => {
            if !is_reset_like {
                let sym = symbol_name_or_type_error(&normalized)?;
                if !VALID_FACE_SLANTS.contains(&sym.as_str()) {
                    return Err(signal(
                        "error",
                        vec![Value::string("Invalid face slant"), normalized],
                    ));
                }
            }
        }
        ":width" => {
            if !is_reset_like {
                let sym = symbol_name_or_type_error(&normalized)?;
                if !VALID_FACE_WIDTHS.contains(&sym.as_str()) {
                    return Err(signal(
                        "error",
                        vec![Value::string("Invalid face width"), normalized],
                    ));
                }
            }
        }
        ":foreground" => {
            if !is_reset_like {
                check_non_empty_string(&normalized, "Empty foreground color value")?;
            }
        }
        ":background" => {
            if !is_reset_like {
                check_non_empty_string(&normalized, "Empty background color value")?;
            }
        }
        ":distant-foreground" => {
            if !is_reset_like {
                check_non_empty_string(&normalized, "Empty distant-foreground color value")?;
            }
        }
        ":inverse-video" => {
            if !is_reset_like {
                let sym = symbol_name_or_type_error(&normalized)?;
                if sym != "t" && sym != "nil" {
                    return Err(signal(
                        "error",
                        vec![
                            Value::string("Invalid inverse-video face attribute value"),
                            normalized,
                        ],
                    ));
                }
            }
        }
        ":extend" => {
            if !is_reset_like {
                let sym = symbol_name_or_type_error(&normalized)?;
                if sym != "t" && sym != "nil" {
                    return Err(signal(
                        "error",
                        vec![
                            Value::string("Invalid extend face attribute value"),
                            normalized,
                        ],
                    ));
                }
            }
        }
        ":inherit" => {
            let valid = match &normalized {
                Value::Nil | Value::True | Value::Symbol(_) => true,
                Value::Cons(_) => list_to_vec(&normalized)
                    .map(|vals| vals.iter().all(Value::is_symbol))
                    .unwrap_or(false),
                _ => false,
            };
            if !valid {
                let mut payload = vec![Value::string("Invalid face inheritance")];
                if let Some(vals) = list_to_vec(&normalized) {
                    payload.extend(vals);
                } else {
                    payload.push(normalized);
                }
                return Err(signal("error", payload));
            }
        }
        ":bold" => {
            let mapped = if normalized.is_nil() {
                Value::symbol("normal")
            } else {
                Value::symbol("bold")
            };
            return Ok((":weight".to_string(), mapped));
        }
        ":italic" => {
            let mapped = if normalized.is_nil() {
                Value::symbol("normal")
            } else {
                Value::symbol("italic")
            };
            return Ok((":slant".to_string(), mapped));
        }
        _ => {}
    }

    Ok((attr.to_string(), normalized))
}

/// `(internal-lisp-face-p FACE &optional FRAME)` -- return a face descriptor
/// vector for known faces, nil otherwise.
pub(crate) fn builtin_internal_lisp_face_p(args: Vec<Value>) -> EvalResult {
    expect_min_args("internal-lisp-face-p", &args, 1)?;
    expect_max_args("internal-lisp-face-p", &args, 2)?;
    let frame_designator = if let Some(frame) = args.get(1) {
        if !optional_selected_frame_designator_p(frame) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *frame],
            ));
        }
        !frame.is_nil()
    } else {
        false
    };
    if let Some(face_name) = known_face_name(&args[0]) {
        if frame_designator {
            Ok(make_lisp_face_vector_for_domain(&face_name, false))
        } else {
            Ok(make_lisp_face_vector())
        }
    } else {
        Ok(Value::Nil)
    }
}

/// `(internal-make-lisp-face FACE &optional FRAME)` -- create/reset FACE as a
/// Lisp face and return its attribute vector.
pub(crate) fn builtin_internal_make_lisp_face(args: Vec<Value>) -> EvalResult {
    expect_min_args("internal-make-lisp-face", &args, 1)?;
    expect_max_args("internal-make-lisp-face", &args, 2)?;
    let face_name = require_symbol_face_name(&args[0])?;
    if let Some(frame) = args.get(1) {
        if !optional_selected_frame_designator_p(frame) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *frame],
            ));
        }
    }
    mark_created_lisp_face(&face_name);
    clear_face_overrides(&face_name, true);
    Ok(make_lisp_face_vector())
}

/// `(internal-copy-lisp-face FROM TO FRAME NEW-FRAME)` -- copy defaults overrides to
/// `TO` and return `TO`.
pub(crate) fn builtin_internal_copy_lisp_face(args: Vec<Value>) -> EvalResult {
    expect_args("internal-copy-lisp-face", &args, 4)?;
    let _ = require_symbol_face_name(&args[0])?;
    let to_name = require_symbol_face_name(&args[1])?;
    let copy_defaults_domain = matches!(args[2], Value::True);
    if !copy_defaults_domain && !frame_device_designator_p(&args[2]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), args[2]],
        ));
    }
    if !copy_defaults_domain && !args[3].is_nil() && !frame_device_designator_p(&args[3]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), args[3]],
        ));
    }
    let from_name = resolve_copy_source_face_symbol(&args[0])?;
    mark_created_lisp_face(&to_name);
    copy_defaults_overrides(&from_name, &to_name);
    Ok(args[1])
}

/// `(internal-set-lisp-face-attribute FACE ATTR VALUE &optional FRAME)` --
/// set FACE attribute in selected-frame/default face domains.
pub(crate) fn builtin_internal_set_lisp_face_attribute(args: Vec<Value>) -> EvalResult {
    expect_min_args("internal-set-lisp-face-attribute", &args, 3)?;
    expect_max_args("internal-set-lisp-face-attribute", &args, 4)?;
    let face = &args[0];
    let face_name = require_symbol_face_name(face)?;
    let attr_name = normalize_set_face_attribute_name(&args[1])?;
    let value = args[2];

    let apply_set = |defaults_frame: bool| -> Result<(), Flow> {
        if defaults_frame {
            if !face_exists_for_domain(&face_name, true) {
                if face.is_nil() {
                    return Err(signal("error", vec![Value::string("Invalid face")]));
                }
                return Err(signal("error", vec![Value::string("Invalid face"), *face]));
            }
        } else if !face_exists_for_domain(&face_name, false) {
            mark_selected_created_lisp_face(&face_name);
            mark_created_lisp_face(&face_name);
        }

        let (canonical_attr, canonical_value) =
            normalize_face_attr_for_set(&face_name, &attr_name, value)?;
        set_face_override(&face_name, &canonical_attr, canonical_value, defaults_frame);
        Ok(())
    };

    match args.get(3) {
        None | Some(Value::Nil) => apply_set(false)?,
        Some(Value::True) => apply_set(true)?,
        Some(Value::Int(0)) => {
            apply_set(true)?;
            apply_set(false)?;
        }
        Some(frame) if frame_device_designator_p(frame) => apply_set(false)?,
        Some(other) => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *other],
            ));
        }
    }

    Ok(*face)
}

/// `(internal-get-lisp-face-attribute FACE ATTR &optional FRAME)` -- batch
/// semantics-compatible face attribute query for core predefined faces.
pub(crate) fn builtin_internal_get_lisp_face_attribute(args: Vec<Value>) -> EvalResult {
    expect_min_args("internal-get-lisp-face-attribute", &args, 2)?;
    expect_max_args("internal-get-lisp-face-attribute", &args, 3)?;
    let defaults_frame = if let Some(frame) = args.get(2) {
        if frame.is_nil() {
            false
        } else if matches!(frame, Value::True) {
            true
        } else if frame_device_designator_p(frame) {
            false
        } else {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *frame],
            ));
        }
    } else {
        false
    };

    let face_name = resolve_face_name_for_domain(&args[0], defaults_frame)?;

    let attr_name = normalize_face_attribute_name(&args[1])?;
    Ok(lisp_face_attribute_value(
        &face_name,
        &attr_name,
        defaults_frame,
    ))
}

/// `(internal-lisp-face-attribute-values ATTR)` -- return valid discrete values
/// for known boolean-like face attributes.
pub(crate) fn builtin_internal_lisp_face_attribute_values(args: Vec<Value>) -> EvalResult {
    expect_args("internal-lisp-face-attribute-values", &args, 1)?;
    let attr_name = face_attr_value_name(&args[0])?;
    if DISCRETE_BOOLEAN_FACE_ATTRIBUTES.contains(&attr_name.as_str()) {
        Ok(Value::list(vec![Value::True, Value::Nil]))
    } else {
        Ok(Value::Nil)
    }
}

/// `(internal-lisp-face-equal-p FACE1 FACE2 &optional FRAME)` -- return t if
/// FACE1 and FACE2 resolve to equal face attributes in the selected frame or in
/// default face definitions when FRAME is t.
pub(crate) fn builtin_internal_lisp_face_equal_p(args: Vec<Value>) -> EvalResult {
    expect_min_args("internal-lisp-face-equal-p", &args, 2)?;
    expect_max_args("internal-lisp-face-equal-p", &args, 3)?;
    let defaults_frame = frame_defaults_flag(args.get(2))?;
    let face1 = resolve_known_face_name_for_compare(&args[0], defaults_frame)?;
    let face2 = resolve_known_face_name_for_compare(&args[1], defaults_frame)?;
    for attr in VALID_FACE_ATTRIBUTES {
        let v1 = lisp_face_attribute_value(&face1, attr, defaults_frame);
        let v2 = lisp_face_attribute_value(&face2, attr, defaults_frame);
        if v1 != v2 {
            return Ok(Value::Nil);
        }
    }
    Ok(Value::True)
}

/// `(internal-lisp-face-empty-p FACE &optional FRAME)` -- return t if FACE has
/// only unspecified attributes in selected/default face definitions.
pub(crate) fn builtin_internal_lisp_face_empty_p(args: Vec<Value>) -> EvalResult {
    expect_min_args("internal-lisp-face-empty-p", &args, 1)?;
    expect_max_args("internal-lisp-face-empty-p", &args, 2)?;
    let defaults_frame = frame_defaults_flag(args.get(1))?;
    let face = resolve_known_face_name_for_compare(&args[0], defaults_frame)?;
    for attr in VALID_FACE_ATTRIBUTES {
        let v = lisp_face_attribute_value(&face, attr, defaults_frame);
        if !matches!(v, Value::Symbol(id) if resolve_sym(id) == "unspecified") {
            return Ok(Value::Nil);
        }
    }
    Ok(Value::True)
}

/// `(internal-merge-in-global-face FACE FRAME)` -- merge concrete defaults-face
/// overrides into selected-frame face state.
pub(crate) fn builtin_internal_merge_in_global_face(args: Vec<Value>) -> EvalResult {
    expect_args("internal-merge-in-global-face", &args, 2)?;
    if !frame_device_designator_p(&args[1]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), args[1]],
        ));
    }
    let face_name = resolve_face_name_for_merge(&args[0])?;
    if !KNOWN_FACES.contains(&face_name.as_str()) {
        mark_created_lisp_face(&face_name);
        mark_selected_created_lisp_face(&face_name);
    }
    merge_defaults_overrides_into_selected(&face_name);
    Ok(Value::Nil)
}

/// `(face-attribute-relative-p ATTRIBUTE VALUE)` -- return t if VALUE is the
/// value is a relative form for ATTRIBUTE.
pub(crate) fn builtin_face_attribute_relative_p(args: Vec<Value>) -> EvalResult {
    expect_args("face-attribute-relative-p", &args, 2)?;
    let height_attr = match &args[0] {
        Value::Keyword(id) | Value::Symbol(id) => {
            let n = resolve_sym(*id);
            n == "height" || n == ":height"
        }
        _ => false,
    };
    if !height_attr {
        return Ok(Value::Nil);
    }

    Ok(Value::bool(!matches!(
        &args[1],
        Value::Int(_) | Value::Char(_)
    )))
}

/// `(merge-face-attribute ATTRIBUTE VALUE1 VALUE2)` -- return VALUE1 unless it
/// is the symbol `unspecified`, in which case return VALUE2.
pub(crate) fn builtin_merge_face_attribute(args: Vec<Value>) -> EvalResult {
    expect_args("merge-face-attribute", &args, 3)?;
    let v1_unspecified = match &args[1] {
        Value::Symbol(id) => resolve_sym(*id) == "unspecified",
        _ => false,
    };
    if v1_unspecified {
        Ok(args[2])
    } else {
        Ok(args[1])
    }
}

/// `(face-list &optional FRAME)` -- return list of known face names.
pub(crate) fn builtin_face_list(args: Vec<Value>) -> EvalResult {
    expect_max_args("face-list", &args, 1)?;
    Ok(Value::list(
        KNOWN_FACES.iter().map(|s| Value::symbol(*s)).collect(),
    ))
}

fn expect_color_string(value: &Value) -> Result<String, Flow> {
    match value {
        Value::Str(id) => Ok(with_heap(|h| h.get_string(*id).to_owned())),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

fn expect_optional_color_frame_arg(args: &[Value], idx: usize) -> Result<(), Flow> {
    if let Some(frame) = args.get(idx) {
        if !frame.is_nil() && !matches!(frame, Value::Frame(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("framep"), *frame],
            ));
        }
    }
    Ok(())
}

fn parse_color_16bit_any(color_name: &str) -> Option<(i64, i64, i64)> {
    let lower = color_name.trim().to_lowercase();
    if let Some(hex) = lower.strip_prefix('#') {
        parse_hex_color_16bit(hex)
    } else {
        parse_named_color_16bit(&lower)
    }
}

/// `(color-defined-p COLOR &optional FRAME)` -- nil if unknown; otherwise truthy
/// for known RGB/hex and supported terminal color names.
pub(crate) fn builtin_color_defined_p(args: Vec<Value>) -> EvalResult {
    expect_min_args("color-defined-p", &args, 1)?;
    expect_max_args("color-defined-p", &args, 2)?;
    expect_optional_color_device_arg(&args, 1)?;
    match &args[0] {
        Value::Str(_) => Ok(Value::bool(!builtin_color_values(vec![args[0]])?.is_nil())),
        _ => Ok(Value::Nil),
    }
}

/// `(color-values COLOR &optional FRAME)` -- resolve COLOR and return a
/// terminal-compatible `(R G B)` list with 16-bit component values.
///
/// In batch/TTY compatibility mode we approximate resolved colors to the
/// nearest entry in the 8-color terminal palette.
pub(crate) fn builtin_color_values(args: Vec<Value>) -> EvalResult {
    expect_min_args("color-values", &args, 1)?;
    expect_max_args("color-values", &args, 2)?;
    expect_optional_color_device_arg(&args, 1)?;
    let color_name = match &args[0] {
        Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
        _ => return Ok(Value::Nil),
    };
    let lower = color_name.trim().to_lowercase();
    let resolved = if let Some(hex) = lower.strip_prefix('#') {
        parse_hex_color_16bit(hex)
    } else {
        parse_named_color_16bit(&lower)
    };
    let Some((r, g, b)) = resolved.map(approximate_tty_color) else {
        return Ok(Value::Nil);
    };
    Ok(Value::list(vec![
        Value::Int(r),
        Value::Int(g),
        Value::Int(b),
    ]))
}

/// `(color-values-from-color-spec COLOR-SPEC)` -- parse hex color spec and
/// return raw `(R G B)` 16-bit channel values.
pub(crate) fn builtin_color_values_from_color_spec(args: Vec<Value>) -> EvalResult {
    expect_args("color-values-from-color-spec", &args, 1)?;
    let color_spec = expect_color_string(&args[0])?;
    let lower = color_spec.trim().to_lowercase();
    let Some(hex) = lower.strip_prefix('#') else {
        return Ok(Value::Nil);
    };
    let Some((r, g, b)) = parse_hex_color_16bit(hex) else {
        return Ok(Value::Nil);
    };
    Ok(Value::list(vec![
        Value::Int(r),
        Value::Int(g),
        Value::Int(b),
    ]))
}

/// `(color-gray-p COLOR &optional FRAME)` -- t if COLOR resolves to equal RGB
/// channels, nil otherwise.
pub(crate) fn builtin_color_gray_p(args: Vec<Value>) -> EvalResult {
    expect_min_args("color-gray-p", &args, 1)?;
    expect_max_args("color-gray-p", &args, 2)?;
    let color = expect_color_string(&args[0])?;
    expect_optional_color_frame_arg(&args, 1)?;
    let Some((r, g, b)) = parse_color_16bit_any(&color) else {
        return Ok(Value::Nil);
    };
    Ok(Value::bool(r == g && g == b))
}

/// `(color-supported-p COLOR &optional FRAME BACKGROUND-P)` -- t if COLOR
/// resolves on this build's color parser.
pub(crate) fn builtin_color_supported_p(args: Vec<Value>) -> EvalResult {
    expect_min_args("color-supported-p", &args, 1)?;
    expect_max_args("color-supported-p", &args, 3)?;
    let color = expect_color_string(&args[0])?;
    expect_optional_color_frame_arg(&args, 1)?;
    let _ = args.get(2);
    Ok(Value::bool(parse_color_16bit_any(&color).is_some()))
}

fn expect_optional_color_distance_frame_arg(args: &[Value], idx: usize) -> Result<(), Flow> {
    if let Some(frame) = args.get(idx) {
        if !frame.is_nil() && !matches!(frame, Value::Frame(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *frame],
            ));
        }
    }
    Ok(())
}

fn invalid_color_error(value: &Value) -> Flow {
    signal("error", vec![Value::string("Invalid color"), *value])
}

fn parse_color_distance_input(value: &Value) -> Result<(i64, i64, i64), Flow> {
    let Value::Str(color_id) = value else {
        return Err(invalid_color_error(value));
    };
    let color = with_heap(|h| h.get_string(*color_id).to_owned());
    let Some(rgb) = parse_color_16bit_any(&color).map(approximate_tty_color) else {
        return Err(invalid_color_error(value));
    };
    Ok(rgb)
}

fn color_distance_metric(lhs: (i64, i64, i64), rhs: (i64, i64, i64)) -> i64 {
    // Emacs-compatible perceptual approximation (redmean) over 8-bit channels.
    let r1 = lhs.0 / 257;
    let g1 = lhs.1 / 257;
    let b1 = lhs.2 / 257;
    let r2 = rhs.0 / 257;
    let g2 = rhs.1 / 257;
    let b2 = rhs.2 / 257;

    let dr = r1 - r2;
    let dg = g1 - g2;
    let db = b1 - b2;
    let rmean = (r1 + r2) / 2;
    (((512 + rmean) * dr * dr) >> 8) + 4 * dg * dg + (((767 - rmean) * db * db) >> 8)
}

/// `(color-distance COLOR1 COLOR2 &optional FRAME METRIC-FN)` -- return a
/// perceptual distance between colors.
pub(crate) fn builtin_color_distance(args: Vec<Value>) -> EvalResult {
    expect_min_args("color-distance", &args, 2)?;
    expect_max_args("color-distance", &args, 4)?;
    expect_optional_color_distance_frame_arg(&args, 2)?;
    let lhs = parse_color_distance_input(&args[0])?;
    let rhs = parse_color_distance_input(&args[1])?;
    Ok(Value::Int(color_distance_metric(lhs, rhs)))
}

fn parse_hex_color_16bit(hex: &str) -> Option<(i64, i64, i64)> {
    match hex.len() {
        3 => {
            let r = i64::from(hex[0..1].chars().next()?.to_digit(16)? as u16);
            let g = i64::from(hex[1..2].chars().next()?.to_digit(16)? as u16);
            let b = i64::from(hex[2..3].chars().next()?.to_digit(16)? as u16);
            Some((
                r | (r << 4) | (r << 8) | (r << 12),
                g | (g << 4) | (g << 8) | (g << 12),
                b | (b << 4) | (b << 8) | (b << 12),
            ))
        }
        6 => Some((
            i64::from(u16::from_str_radix(&hex[0..2], 16).ok()?) * 257,
            i64::from(u16::from_str_radix(&hex[2..4], 16).ok()?) * 257,
            i64::from(u16::from_str_radix(&hex[4..6], 16).ok()?) * 257,
        )),
        12 => Some((
            i64::from(u16::from_str_radix(&hex[0..4], 16).ok()?),
            i64::from(u16::from_str_radix(&hex[4..8], 16).ok()?),
            i64::from(u16::from_str_radix(&hex[8..12], 16).ok()?),
        )),
        _ => None,
    }
}

fn parse_named_color_16bit(name: &str) -> Option<(i64, i64, i64)> {
    match name {
        "black" => Some((0, 0, 0)),
        "white" => Some((65535, 65535, 65535)),
        "red" => Some((65535, 0, 0)),
        "green" => Some((0, 65535, 0)),
        "blue" => Some((0, 0, 65535)),
        "yellow" => Some((65535, 65535, 0)),
        "cyan" => Some((0, 65535, 65535)),
        "magenta" => Some((65535, 0, 65535)),
        "gray" | "grey" => Some((48573, 48573, 48573)),
        "dark gray" | "dark grey" | "darkgray" | "darkgrey" => Some((43690, 43690, 43690)),
        "light gray" | "light grey" | "lightgray" | "lightgrey" => Some((55512, 55512, 55512)),
        "orange" => Some((65535, 42405, 0)),
        "orange red" | "orangered" => Some((65535, 17990, 0)),
        "pink" => Some((65535, 49344, 52171)),
        "brown" => Some((42405, 10794, 10794)),
        "purple" => Some((32896, 0, 32896)),
        _ => None,
    }
}

fn approximate_tty_color((r, g, b): (i64, i64, i64)) -> (i64, i64, i64) {
    // Emacs batch/TTY behavior is effectively a coarse 8-color quantization.
    // A narrow channel spread is treated as gray, otherwise channels are
    // quantized relative to the local min/max midpoint.
    const GRAY_BAND: i64 = 0x1111;
    const BRIGHT_THRESHOLD: i64 = 0x8888;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max - min <= GRAY_BAND {
        return if max >= BRIGHT_THRESHOLD {
            (65535, 65535, 65535)
        } else {
            (0, 0, 0)
        };
    }

    let mid = (max + min) / 2;
    (
        if r >= mid { 65535 } else { 0 },
        if g >= mid { 65535 } else { 0 },
        if b >= mid { 65535 } else { 0 },
    )
}

fn invalid_get_device_terminal_error(value: &Value) -> Flow {
    signal(
        "error",
        vec![Value::string(format!(
            "Invalid argument {} in 'get-device-terminal'",
            super::print::print_value(value)
        ))],
    )
}

fn color_device_designator_p(value: &Value) -> bool {
    match value {
        Value::Nil => true,
        other => frame_device_designator_p(other),
    }
}

fn expect_optional_color_device_arg(args: &[Value], idx: usize) -> Result<(), Flow> {
    if let Some(value) = args.get(idx) {
        if !color_device_designator_p(value) {
            return Err(invalid_get_device_terminal_error(value));
        }
    }
    Ok(())
}

/// `(defined-colors &optional FRAME)` -- return a list of defined color names.
pub(crate) fn builtin_defined_colors(args: Vec<Value>) -> EvalResult {
    expect_max_args("defined-colors", &args, 1)?;
    expect_optional_color_device_arg(&args, 0)?;
    let colors = vec![
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ];
    Ok(Value::list(colors.into_iter().map(Value::string).collect()))
}

/// `(face-id FACE &optional FRAME)` -- return numeric face id for known and
/// dynamically created faces.
pub(crate) fn builtin_face_id(args: Vec<Value>) -> EvalResult {
    expect_min_args("face-id", &args, 1)?;
    expect_max_args("face-id", &args, 2)?;
    if matches!(&args[0], Value::Str(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    }

    if let Some(name) = symbol_name_for_face_value(&args[0]) {
        if let Some(id) = face_id_for_name(&name) {
            return Ok(Value::Int(id));
        }
        if is_created_lisp_face(&name) {
            ensure_dynamic_face_id(&name);
            if let Some(id) = face_id_for_name(&name) {
                return Ok(Value::Int(id));
            }
        }
    }
    let rendered = super::print::print_value(&args[0]);
    Err(signal(
        "error",
        vec![Value::string(format!("Not a face: {rendered}"))],
    ))
}

/// `(face-font FACE &optional FRAME CHARACTER)` -- returns nil for batch
/// compatibility when the face exists.
pub(crate) fn builtin_face_font(args: Vec<Value>) -> EvalResult {
    expect_min_args("face-font", &args, 1)?;
    expect_max_args("face-font", &args, 3)?;
    match &args[0] {
        Value::Str(id) => {
            let name = with_heap(|h| h.get_string(*id).to_owned());
            if KNOWN_FACES.contains(&name.as_str()) {
                Ok(Value::Nil)
            } else {
                let payload = if name.is_empty() {
                    Value::symbol("")
                } else {
                    Value::symbol(&name)
                };
                Err(signal(
                    "error",
                    vec![Value::string("Invalid face"), payload],
                ))
            }
        }
        Value::Nil => Err(signal("error", vec![Value::string("Invalid face")])),
        Value::True | Value::Symbol(_) => {
            if let Some(name) = symbol_name_for_face_value(&args[0]) {
                if KNOWN_FACES.contains(&name.as_str()) {
                    return Ok(Value::Nil);
                }
            }
            Err(signal(
                "error",
                vec![Value::string("Invalid face"), args[0]],
            ))
        }
        _ => Err(signal(
            "error",
            vec![Value::string("Invalid face"), args[0]],
        )),
    }
}

/// `(internal-face-x-get-resource RESOURCE CLASS FRAME)` -- validate arguments and
/// return nil (font resource lookup is not implemented).
pub(crate) fn builtin_internal_face_x_get_resource(args: Vec<Value>) -> EvalResult {
    expect_min_args("internal-face-x-get-resource", &args, 2)?;
    expect_max_args("internal-face-x-get-resource", &args, 3)?;
    for arg in args.iter().take(2) {
        if !arg.is_string() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *arg],
            ));
        }
    }
    Ok(Value::Nil)
}

/// `(internal-set-font-selection-order ORDER)` -- validate order list shape and return nil.
pub(crate) fn builtin_internal_set_font_selection_order(args: Vec<Value>) -> EvalResult {
    expect_args("internal-set-font-selection-order", &args, 1)?;
    let order = &args[0];
    if !order.is_nil() && !matches!(order, Value::Cons(_)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("listp"), *order],
        ));
    }

    let valid_keywords = [":width", ":height", ":weight", ":slant"];
    let valid = if let Some(values) = list_to_vec(order) {
        if values.len() == valid_keywords.len() {
            let mut seen = HashSet::new();
            values.iter().all(|value| {
                if let Value::Keyword(id) = value {
                    let s = resolve_sym(*id);
                    let key = if s.starts_with(':') {
                        s.to_owned()
                    } else {
                        format!(":{s}")
                    };
                    valid_keywords.contains(&key.as_str()) && seen.insert(key)
                } else {
                    false
                }
            })
        } else {
            false
        }
    } else {
        false
    };

    if valid {
        return Ok(Value::Nil);
    }

    if let Some(values) = list_to_vec(order) {
        if values.is_empty() {
            return Err(signal(
                "error",
                vec![Value::string("Invalid font sort order")],
            ));
        }
        let mut payload = vec![Value::string("Invalid font sort order")];
        payload.extend(values);
        return Err(signal("error", payload));
    }

    Err(signal(
        "error",
        vec![Value::string("Invalid font sort order"), *order],
    ))
}

/// `(internal-set-alternative-font-family-alist ALIST)` -- normalize string
/// entries to symbols and return the normalized list.
pub(crate) fn builtin_internal_set_alternative_font_family_alist(args: Vec<Value>) -> EvalResult {
    expect_args("internal-set-alternative-font-family-alist", &args, 1)?;
    let entries = proper_list_to_vec_or_listp_error(&args[0])?;
    let mut normalized = Vec::with_capacity(entries.len());
    for entry in entries {
        let members = proper_list_to_vec_or_listp_error(&entry)?;
        let mut converted = Vec::with_capacity(members.len());
        for member in members {
            match member {
                Value::Str(id) => {
                    converted.push(Value::symbol(with_heap(|h| h.get_string(id).to_owned())))
                }
                other => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("stringp"), other],
                    ));
                }
            }
        }
        normalized.push(Value::list(converted));
    }
    Ok(Value::list(normalized))
}

/// `(internal-set-alternative-font-registry-alist ALIST)` -- validate ALIST shape and
/// return it unchanged.
pub(crate) fn builtin_internal_set_alternative_font_registry_alist(args: Vec<Value>) -> EvalResult {
    expect_args("internal-set-alternative-font-registry-alist", &args, 1)?;
    let entries = proper_list_to_vec_or_listp_error(&args[0])?;
    for entry in entries {
        let _ = proper_list_to_vec_or_listp_error(&entry)?;
    }
    Ok(args[0])
}

// ---------------------------------------------------------------------------
// xfaces.c: x-load-color-file
// ---------------------------------------------------------------------------

/// `(x-load-color-file FILENAME)` — read an RGB color file (rgb.txt format)
/// and return an alist of `(NAME R G B)` entries.
///
/// Each line has the format `R G B  name` where R/G/B are 0-255 decimal.
/// Lines starting with `!` or `#` are comments and are skipped.
pub(crate) fn builtin_x_load_color_file(args: Vec<Value>) -> EvalResult {
    expect_args("x-load-color-file", &args, 1)?;
    let filename = match &args[0] {
        Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };

    // Expand the filename (resolve ~, relative paths, etc.)
    let expanded = super::fileio::expand_file_name(&filename, None);
    let contents = match std::fs::read_to_string(&expanded) {
        Ok(s) => s,
        Err(_) => return Ok(Value::Nil),
    };

    let mut result = Value::Nil;
    // Build alist in reverse order, then reverse (or build in correct order
    // by collecting into vec and reversing).
    let mut entries: Vec<Value> = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('!') || trimmed.starts_with('#') {
            continue;
        }
        // Parse: R G B  color-name
        let mut parts = trimmed.splitn(4, char::is_whitespace);
        let r_str = match parts.next() {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        // Skip whitespace between fields
        let g_str = loop {
            match parts.next() {
                Some(s) if !s.is_empty() => break s,
                Some(_) => continue,
                None => break "",
            }
        };
        if g_str.is_empty() {
            continue;
        }
        let b_str = loop {
            match parts.next() {
                Some(s) if !s.is_empty() => break s,
                Some(_) => continue,
                None => break "",
            }
        };
        if b_str.is_empty() {
            continue;
        }
        let name_part = loop {
            match parts.next() {
                Some(s) if !s.is_empty() => break s,
                Some(_) => continue,
                None => break "",
            }
        };
        if name_part.is_empty() {
            continue;
        }

        let r: u16 = match r_str.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let g: u16 = match g_str.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let b: u16 = match b_str.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Scale 0-255 to 0-65535 (same as Emacs: val * 257)
        let r16 = (r as i64) * 257;
        let g16 = (g as i64) * 257;
        let b16 = (b as i64) * 257;

        // Build (NAME R G B) as a proper list
        let color_entry = Value::cons(
            Value::string(name_part),
            Value::cons(
                Value::Int(r16),
                Value::cons(Value::Int(g16), Value::cons(Value::Int(b16), Value::Nil)),
            ),
        );
        entries.push(color_entry);
    }

    // Build alist from entries (preserve file order)
    for entry in entries.into_iter().rev() {
        result = Value::cons(entry, result);
    }

    Ok(result)
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "font_test.rs"]
mod tests;
