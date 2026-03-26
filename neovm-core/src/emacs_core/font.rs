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
use std::sync::{OnceLock, RwLock};

use super::error::{EvalResult, Flow, signal};
use super::intern::{intern, resolve_sym};
use super::textprop::string_elisp_pos_to_byte;
use super::value::*;
use crate::buffer::{Buffer, BufferManager};
use crate::face::{
    BoxStyle, Color, Face as RuntimeFace, FaceHeight, FontSlant, FontWeight, FontWidth,
    UnderlineStyle,
};
use crate::window::{FRAME_ID_BASE, FrameId, FrameManager, WindowId};

type AlternativeFontFamilyAlist = Vec<(String, Vec<String>)>;

static ALTERNATIVE_FONT_FAMILY_ALIST: OnceLock<RwLock<AlternativeFontFamilyAlist>> =
    OnceLock::new();

fn alternative_font_family_alist() -> &'static RwLock<AlternativeFontFamilyAlist> {
    ALTERNATIVE_FONT_FAMILY_ALIST.get_or_init(|| RwLock::new(Vec::new()))
}

pub fn alternative_font_families(family: &str) -> Vec<String> {
    let lookup = family.trim();
    if lookup.is_empty() {
        return Vec::new();
    }

    let Ok(alist) = alternative_font_family_alist().read() else {
        return vec![lookup.to_string()];
    };

    let key = lookup.to_ascii_lowercase();
    alist
        .iter()
        .find_map(|(name, families)| (name == &key).then_some(families.clone()))
        .unwrap_or_else(|| vec![lookup.to_string()])
}

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

fn frame_id_from_designator(value: &Value) -> Option<FrameId> {
    match value {
        Value::Int(id) if *id >= 0 => Some(FrameId(*id as u64)),
        Value::Frame(id) => Some(FrameId(*id)),
        _ => None,
    }
}

fn font_value_text(value: &Value) -> Option<String> {
    match value {
        Value::Str(id) => Some(with_heap(|heap| heap.get_string(*id).to_owned())),
        Value::Symbol(id) | Value::Keyword(id) => Some(resolve_sym(*id).to_owned()),
        _ => None,
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

fn live_frame_id_for_face_update(
    eval: &mut super::eval::Context,
    frame: Option<&Value>,
) -> Result<Option<FrameId>, Flow> {
    match frame {
        None | Some(Value::Nil) | Some(Value::Int(0)) => {
            Ok(Some(super::window_cmds::ensure_selected_frame_id(eval)))
        }
        Some(Value::True) => Ok(None),
        Some(value) if live_frame_designator_in_state(&eval.frames, value) => Ok(Some(
            frame_id_from_designator(value)
                .expect("live frame designator should decode to frame id"),
        )),
        Some(other) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), *other],
        )),
    }
}

fn mirror_runtime_face_into_frame(
    eval: &mut super::eval::Context,
    frame_id: FrameId,
    face_name: &str,
) {
    let Some(face) = eval.face_table().get(face_name).cloned() else {
        return;
    };
    if let Some(frame) = eval.frames.get_mut(frame_id) {
        crate::emacs_core::xfaces::mirror_runtime_face_into_frame(frame, face_name, &face);
    }
}

/// Realize the selected frame's `font-parameter` into the runtime/frame-local
/// `default` face without mutating Lisp override state.
///
/// GNU keeps the defface for `default` empty and realizes the actual frame
/// font through the face subsystem in C.  Neomacs still needs the runtime
/// `FaceTable` and per-frame face hash table populated early so startup code
/// that depends on the realized default face does not fall back to a static
/// global seed.
pub fn seed_live_frame_default_face_from_font_parameter(
    eval: &mut super::eval::Context,
    frame_id: FrameId,
) {
    let Some(font_value) = eval
        .frames
        .get(frame_id)
        .and_then(|frame| frame.parameters.get("font-parameter").copied())
    else {
        return;
    };

    for (attr_name, attr_value) in derived_face_attrs_from_font_value(&font_value) {
        if let Some(face_attr) = lisp_value_to_face_attr(&attr_name, attr_value) {
            eval.set_face_attribute("default", &attr_name, face_attr);
        }
    }

    mirror_runtime_face_into_frame(eval, frame_id, "default");
}

// ---------------------------------------------------------------------------
// Font-spec helpers
// ---------------------------------------------------------------------------

/// The tag keyword used to identify font-spec vectors: `:font-spec`.
const FONT_SPEC_TAG: &str = "font-spec";
const FONT_ENTITY_TAG: &str = "font-entity";
const FONT_OBJECT_TAG: &str = "font-object";

fn is_tagged_font_vector(val: &Value, tag: &str) -> bool {
    match val {
        Value::Vector(v) => {
            let elems = with_heap(|h| h.get_vector(*v).clone());
            matches!(&elems.first(), Some(Value::Keyword(k)) if resolve_sym(*k) == tag)
        }
        _ => false,
    }
}

/// Check whether a Value is a font-spec (a vector whose first element is
/// the keyword `:font-spec`).
fn is_font_spec(val: &Value) -> bool {
    is_tagged_font_vector(val, FONT_SPEC_TAG)
}

/// Check whether a value is represented as a font-object vector.
fn is_font_object(val: &Value) -> bool {
    is_tagged_font_vector(val, FONT_OBJECT_TAG)
}

/// Check whether a value is represented as a font-entity vector.
fn is_font_entity(val: &Value) -> bool {
    is_tagged_font_vector(val, FONT_ENTITY_TAG)
}

fn is_font(val: &Value) -> bool {
    is_font_spec(val) || is_font_entity(val) || is_font_object(val)
}

/// Extract a property from a tagged font vector.
///
/// Property lookup is strict: keys only match if they are exactly equal to
/// `prop` (keyword vs symbol distinction is preserved).
fn font_vector_get(vec_elems: &[Value], prop: &Value) -> Value {
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

/// Get a property from a tagged font vector while accepting both `family` and `:family`
/// style keys, and both keyword and symbol keys.
fn font_vector_get_flexible(vec_elems: &[Value], prop: &str) -> Option<Value> {
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

fn xlfd_fields_from_font_vector(
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
    let foundry = font_vector_get_flexible(v, "foundry")
        .map(|value| font_spec_field_to_string(&value))
        .unwrap_or_else(|| "*".to_string());
    let family = font_vector_get_flexible(v, "family")
        .map(|value| font_spec_field_to_string(&value))
        .unwrap_or_else(|| "*".to_string());
    let weight = font_vector_get_flexible(v, "weight")
        .map(|value| sanitize_style_field(&value))
        .unwrap_or_else(|| "*".to_string());
    let slant = font_vector_get_flexible(v, "slant")
        .map(|value| sanitize_style_field(&value))
        .unwrap_or_else(|| "*".to_string());
    let set_width = font_vector_get_flexible(v, "set-width")
        .or_else(|| font_vector_get_flexible(v, "setwidth"))
        .or_else(|| font_vector_get_flexible(v, "width"))
        .map(|value| font_spec_field_to_string(&value))
        .unwrap_or_else(|| "*".to_string());
    let adstyle = font_vector_get_flexible(v, "adstyle")
        .map(|value| font_spec_field_to_string(&value))
        .unwrap_or_else(|| "*".to_string());

    let size = font_vector_get_flexible(v, "size");
    let dpi = font_vector_get_flexible(v, "dpi");
    let spacing = font_vector_get_flexible(v, "spacing");
    let avg_width = font_vector_get_flexible(v, "average_width")
        .or_else(|| font_vector_get_flexible(v, "avg_width"))
        .or_else(|| font_vector_get_flexible(v, "avg-width"));
    let registry = font_vector_get_flexible(v, "registry");

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
    let object = &args[0];
    let extra_type = args.get(1).copied().unwrap_or(Value::Nil);
    let value = if extra_type.is_nil() {
        is_font(object)
    } else if extra_type.is_symbol_named("font-spec") {
        is_font_spec(object)
    } else if extra_type.is_symbol_named("font-object") {
        is_font_object(object)
    } else if extra_type.is_symbol_named("font-entity") {
        is_font_entity(object)
    } else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("font-extra-type"), extra_type],
        ));
    };
    Ok(Value::bool(value))
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
    if !is_font(&args[0]) {
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
            let exact = font_vector_get(&elems, &args[1]);
            if !exact.is_nil() {
                return Ok(exact);
            }

            if let Value::Keyword(id) = args[1] {
                return Ok(font_vector_get_flexible(&elems, resolve_sym(id)).unwrap_or(Value::Nil));
            }

            Ok(Value::Nil)
        }
        _ => unreachable!("font check above guarantees vector"),
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

/// Context-aware variant of `list-fonts`.
///
/// Accepts live frame designators in the optional FRAME slot.
pub(crate) fn builtin_list_fonts_eval(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_list_fonts_in_state(&eval.frames, args)
}

fn font_weight_from_value(value: Value) -> Option<FontWeight> {
    match value {
        Value::Int(weight) if (0..=u16::MAX as i64).contains(&weight) => {
            Some(FontWeight(weight as u16))
        }
        Value::Symbol(id) | Value::Keyword(id) => FontWeight::from_symbol(resolve_sym(id)),
        _ => None,
    }
}

fn font_slant_from_value(value: Value) -> Option<FontSlant> {
    match value {
        Value::Symbol(id) | Value::Keyword(id) => FontSlant::from_symbol(resolve_sym(id)),
        _ => None,
    }
}

fn find_font_frame_id(
    eval: &mut super::eval::Context,
    frame: Option<&Value>,
) -> Result<FrameId, Flow> {
    match frame {
        None | Some(Value::Nil) => Ok(super::window_cmds::ensure_selected_frame_id(eval)),
        Some(value) if live_frame_designator_in_state(&eval.frames, value) => {
            frame_id_from_designator(value).ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("frame-live-p"), *value],
                )
            })
        }
        Some(other) => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), *other],
        )),
    }
}

fn font_spec_resolve_request(
    eval: &mut super::eval::Context,
    font_spec: &Value,
    frame: Option<&Value>,
) -> Result<super::eval::FontSpecResolveRequest, Flow> {
    let Value::Vector(id) = font_spec else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("font-spec"), *font_spec],
        ));
    };

    let elems = with_heap(|heap| heap.get_vector(*id).clone());
    let family =
        font_vector_get_flexible(&elems, "family").and_then(|value| font_value_text(&value));
    let registry =
        font_vector_get_flexible(&elems, "registry").and_then(|value| font_value_text(&value));
    let lang = font_vector_get_flexible(&elems, "lang").and_then(|value| font_value_text(&value));
    let weight = font_vector_get_flexible(&elems, "weight").and_then(font_weight_from_value);
    let slant = font_vector_get_flexible(&elems, "slant").and_then(font_slant_from_value);

    Ok(super::eval::FontSpecResolveRequest {
        frame_id: find_font_frame_id(eval, frame)?,
        family,
        registry,
        lang,
        weight,
        slant,
    })
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

/// Context-aware variant of `find-font`.
///
/// Accepts live frame designators in the optional FRAME slot.
pub(crate) fn builtin_find_font_eval(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("find-font", &args, 1)?;
    expect_max_args("find-font", &args, 2)?;
    if !is_font_spec(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("font-spec"), args[0]],
        ));
    }

    let request = font_spec_resolve_request(eval, &args[0], args.get(1))?;
    let Some(host) = eval.display_host.as_mut() else {
        return Ok(Value::Nil);
    };
    let matched = host
        .resolve_font_for_spec(request)
        .map_err(|err| signal("error", vec![Value::string(err)]))?;
    let Some(matched) = matched else {
        return Ok(Value::Nil);
    };
    Ok(build_font_entity_for_spec_match(&matched))
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

/// Context-aware variant of `font-family-list`.
///
/// Accepts live frame designators in the optional FRAME slot.
pub(crate) fn builtin_font_family_list_eval(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_font_family_list_in_state(&eval.frames, args)
}

/// `(font-xlfd-name FONT &optional FOLD-WILDCARDS)` -- render font-spec fields
/// into an XLFD string; wildcard folding is supported in compatibility mode.
pub(crate) fn builtin_font_xlfd_name(args: Vec<Value>) -> EvalResult {
    expect_min_args("font-xlfd-name", &args, 1)?;
    expect_max_args("font-xlfd-name", &args, 3)?;
    if !is_font(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("font"), args[0]],
        ));
    }

    let fields = match &args[0] {
        Value::Vector(v) => {
            let elems = with_heap(|h| h.get_vector(*v).clone());
            if is_font_object(&args[0])
                && let Some(Value::Str(id)) = font_vector_get_flexible(&elems, "name")
            {
                let font_name = with_heap(|h| h.get_string(id).to_owned());
                if font_name.starts_with('-') {
                    return Ok(Value::string(
                        if args.get(1).is_some_and(Value::is_truthy) {
                            fold_xlfd_wildcards(font_name)
                        } else {
                            font_name
                        },
                    ));
                }
            }
            xlfd_fields_from_font_vector(&elems)
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

#[derive(Clone, Debug)]
enum FaceLayer {
    Named(Vec<String>),
    Inline(RuntimeFace),
}

fn window_id_from_designator(value: &Value) -> Option<WindowId> {
    match value {
        Value::Window(id) => Some(WindowId(*id)),
        Value::Int(n) if *n >= 0 => Some(WindowId(*n as u64)),
        _ => None,
    }
}

fn resolve_live_window_for_font_at(
    eval: &mut super::eval::Context,
    value: Option<&Value>,
) -> Result<(FrameId, WindowId), Flow> {
    match value {
        None | Some(Value::Nil) => {
            let frame_id = super::window_cmds::ensure_selected_frame_id(eval);
            let frame = eval
                .frames
                .get(frame_id)
                .ok_or_else(|| signal("error", vec![Value::string("No selected frame")]))?;
            Ok((frame_id, frame.selected_window))
        }
        Some(other) => {
            let Some(window_id) = window_id_from_designator(other) else {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("window-live-p"), *other],
                ));
            };
            let Some(frame_id) = eval.frames.find_window_frame_id(window_id) else {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("window-live-p"), *other],
                ));
            };
            Ok((frame_id, window_id))
        }
    }
}

fn resolve_face_layers_from_value(value: &Value) -> Vec<FaceLayer> {
    match value {
        Value::Nil => Vec::new(),
        Value::Symbol(_) | Value::Keyword(_) => value
            .as_symbol_name()
            .filter(|name| *name != "nil")
            .map(|name| vec![FaceLayer::Named(vec![name.to_string()])])
            .unwrap_or_default(),
        Value::Cons(_) => {
            let Some(items) = list_to_vec(value) else {
                return Vec::new();
            };
            if items
                .first()
                .is_some_and(|item| matches!(item, Value::Keyword(_)))
            {
                vec![FaceLayer::Inline(RuntimeFace::from_plist(
                    "--font-at--",
                    &items,
                ))]
            } else {
                let names = items
                    .iter()
                    .filter_map(|item| {
                        item.as_symbol_name()
                            .filter(|name| *name != "nil")
                            .map(|name| name.to_string())
                    })
                    .collect::<Vec<_>>();
                if names.is_empty() {
                    Vec::new()
                } else {
                    vec![FaceLayer::Named(names)]
                }
            }
        }
        _ => Vec::new(),
    }
}

fn apply_face_layers(face_table: &crate::face::FaceTable, layers: &[FaceLayer]) -> RuntimeFace {
    let mut face = face_table.resolve("default");
    for layer in layers {
        match layer {
            FaceLayer::Named(names) => {
                let refs = names.iter().map(String::as_str).collect::<Vec<_>>();
                let merged = face_table.merge_faces(&refs);
                face = face.merge(&merged);
            }
            FaceLayer::Inline(inline_face) => {
                face = face.merge(inline_face);
            }
        }
    }
    face
}

fn resolved_face_at_buffer_byte(
    eval: &super::eval::Context,
    buffer: &Buffer,
    bytepos: usize,
) -> RuntimeFace {
    let mut layers = Vec::new();

    if let Some(value) = buffer.text_props.get_property(bytepos, "face") {
        layers.extend(resolve_face_layers_from_value(value));
    }
    if let Some(value) = buffer.text_props.get_property(bytepos, "font-lock-face") {
        layers.extend(resolve_face_layers_from_value(value));
    }

    let mut overlay_layers = Vec::new();
    for overlay_id in buffer.overlays.overlays_at(bytepos) {
        let priority = buffer
            .overlays
            .overlay_get(overlay_id, "priority")
            .and_then(Value::as_int)
            .unwrap_or(0);
        if let Some(value) = buffer.overlays.overlay_get(overlay_id, "face") {
            let resolved = resolve_face_layers_from_value(value);
            if !resolved.is_empty() {
                overlay_layers.push((priority, resolved));
            }
        }
    }
    overlay_layers.sort_by_key(|(priority, _)| *priority);
    for (_, resolved) in overlay_layers {
        layers.extend(resolved);
    }

    apply_face_layers(&eval.face_table, &layers)
}

fn resolved_face_at_string_byte(
    eval: &super::eval::Context,
    str_id: crate::gc::types::ObjId,
    bytepos: usize,
) -> RuntimeFace {
    let mut layers = Vec::new();
    if let Some(table) = get_string_text_properties_table(str_id) {
        if let Some(value) = table.get_property(bytepos, "face") {
            layers.extend(resolve_face_layers_from_value(value));
        }
        if let Some(value) = table.get_property(bytepos, "font-lock-face") {
            layers.extend(resolve_face_layers_from_value(value));
        }
    }
    apply_face_layers(&eval.face_table, &layers)
}

fn face_height_to_font_value(height: &FaceHeight) -> Value {
    match height {
        FaceHeight::Absolute(n) => Value::Int(*n as i64),
        FaceHeight::Relative(f) => Value::Float(*f, next_float_id()),
    }
}

fn font_weight_symbol(weight: FontWeight) -> &'static str {
    match weight.0 {
        0..=150 => "thin",
        151..=250 => "ultra-light",
        251..=350 => "light",
        351..=450 => "normal",
        451..=550 => "medium",
        551..=650 => "semi-bold",
        651..=750 => "bold",
        751..=850 => "extra-bold",
        _ => "black",
    }
}

fn font_slant_symbol(slant: FontSlant) -> &'static str {
    match slant {
        FontSlant::Normal => "normal",
        FontSlant::Italic => "italic",
        FontSlant::Oblique => "oblique",
        FontSlant::ReverseItalic => "reverse-italic",
        FontSlant::ReverseOblique => "reverse-oblique",
    }
}

fn font_width_symbol(width: FontWidth) -> &'static str {
    match width {
        FontWidth::UltraCondensed => "ultra-condensed",
        FontWidth::ExtraCondensed => "extra-condensed",
        FontWidth::Condensed => "condensed",
        FontWidth::SemiCondensed => "semi-condensed",
        FontWidth::Normal => "normal",
        FontWidth::SemiExpanded => "semi-expanded",
        FontWidth::Expanded => "expanded",
        FontWidth::ExtraExpanded => "extra-expanded",
        FontWidth::UltraExpanded => "ultra-expanded",
    }
}

fn build_font_object(face: &RuntimeFace) -> Value {
    let mut elems = vec![Value::keyword(FONT_OBJECT_TAG)];

    let mut push_field = |name: &str, value: Value| {
        elems.push(Value::keyword(name));
        elems.push(value);
    };

    if let Some(foundry) = &face.foundry {
        push_field("foundry", Value::string(foundry.clone()));
    }
    if let Some(family) = &face.family {
        push_field("family", Value::string(family.clone()));
    }
    if let Some(weight) = face.weight {
        push_field("weight", Value::symbol(font_weight_symbol(weight)));
    }
    if let Some(slant) = face.slant {
        push_field("slant", Value::symbol(font_slant_symbol(slant)));
    }
    if let Some(width) = face.width {
        push_field("width", Value::symbol(font_width_symbol(width)));
    }
    if let Some(height) = &face.height {
        let value = face_height_to_font_value(height);
        push_field("height", value);
        push_field("size", value);
    }

    let font_object = Value::vector(elems);
    let xlfd = builtin_font_xlfd_name(vec![font_object]).unwrap_or_else(|_| Value::Nil);
    if let Value::Vector(id) = font_object {
        with_heap_mut(|heap| {
            let items = heap.get_vector_mut(id);
            items.push(Value::keyword("name"));
            items.push(if xlfd.is_nil() { Value::Nil } else { xlfd });
        });
    }
    font_object
}

fn build_font_entity_for_spec_match(matched: &super::eval::ResolvedFontSpecMatch) -> Value {
    let mut elems = vec![Value::keyword(FONT_ENTITY_TAG)];

    let mut push_field = |name: &str, value: Value| {
        elems.push(Value::keyword(name));
        elems.push(value);
    };

    push_field("family", Value::string(matched.family.clone()));
    if let Some(registry) = &matched.registry {
        push_field("registry", Value::string(registry.clone()));
    }
    if let Some(weight) = matched.weight {
        push_field("weight", Value::symbol(font_weight_symbol(weight)));
    }
    if let Some(slant) = matched.slant {
        push_field("slant", Value::symbol(font_slant_symbol(slant)));
    }
    if let Some(width) = matched.width {
        push_field("width", Value::symbol(font_width_symbol(width)));
    }
    if let Some(spacing) = matched.spacing {
        push_field("spacing", Value::Int(spacing as i64));
    }
    if let Some(postscript_name) = &matched.postscript_name {
        push_field("postscript-name", Value::string(postscript_name.clone()));
    }

    Value::vector(elems)
}

fn build_font_object_for_match(
    face: &RuntimeFace,
    matched: &super::eval::ResolvedFontMatch,
) -> Value {
    let mut selected = face.clone();
    selected.family = Some(matched.family.clone());
    selected.foundry = matched.foundry.clone().or_else(|| face.foundry.clone());
    selected.weight = Some(matched.weight);
    selected.slant = Some(matched.slant);
    selected.width = Some(matched.width);
    build_font_object(&selected)
}

fn font_name_value(font_like: &Value) -> Option<Value> {
    match font_like {
        Value::Str(_) => Some(*font_like),
        Value::Vector(id) if is_font(font_like) => {
            let elems = with_heap(|h| h.get_vector(*id).clone());
            if let Some(value) = font_vector_get_flexible(&elems, "name") {
                return match value {
                    Value::Str(_) => Some(value),
                    Value::Symbol(sym) | Value::Keyword(sym) => {
                        Some(Value::string(resolve_sym(sym).to_owned()))
                    }
                    _ => None,
                };
            }
            match builtin_font_xlfd_name(vec![*font_like]) {
                Ok(Value::Str(_)) => builtin_font_xlfd_name(vec![*font_like]).ok(),
                _ => None,
            }
        }
        _ => None,
    }
}

fn font_value_matches_frame_font_parameter(
    frame: &crate::window::Frame,
    requested: &Value,
) -> bool {
    let Some(frame_font) = frame.parameters.get("font") else {
        return false;
    };
    match (frame_font, requested) {
        (Value::Str(expected), Value::Str(actual)) => {
            with_heap(|heap| heap.get_string(*expected) == heap.get_string(*actual))
        }
        _ => false,
    }
}

fn resolved_live_frame_font_value(
    eval: &super::eval::Context,
    frame_id: FrameId,
    requested: &Value,
) -> Value {
    if is_font(requested) {
        return *requested;
    }

    let Some(frame) = eval.frames.get(frame_id) else {
        return *requested;
    };
    if !font_value_matches_frame_font_parameter(frame, requested) {
        return *requested;
    }

    frame
        .parameters
        .get("font-parameter")
        .copied()
        .filter(is_font)
        .unwrap_or(*requested)
}

fn public_live_frame_font_value(font_value: Value) -> Value {
    let Value::Vector(id) = font_value else {
        return font_value;
    };
    if !is_font(&font_value) {
        return font_value;
    }

    let elems = with_heap(|heap| heap.get_vector(id).clone());
    let mut filtered = Vec::with_capacity(elems.len());
    let mut idx = 0;
    while idx < elems.len() {
        if idx == 0 {
            filtered.push(elems[idx]);
            idx += 1;
            continue;
        }

        if idx + 1 >= elems.len() {
            filtered.push(elems[idx]);
            break;
        }

        let keep = !matches!(&elems[idx], Value::Keyword(sym) | Value::Symbol(sym) if {
            resolve_sym(*sym).trim_start_matches(':') == "height"
        });
        if keep {
            filtered.push(elems[idx]);
            filtered.push(elems[idx + 1]);
        }
        idx += 2;
    }

    Value::vector(filtered)
}

fn live_frame_font_attribute_fallback(
    eval: &super::eval::Context,
    frame_id: FrameId,
    attr_name: &str,
) -> Option<Value> {
    let frame = eval.frames.get(frame_id)?;
    let font_value = frame.parameters.get("font-parameter").copied()?;
    if !is_font(&font_value) {
        return None;
    }

    if attr_name == ":font" {
        return Some(public_live_frame_font_value(font_value));
    }

    derived_face_attrs_from_font_value(&font_value)
        .into_iter()
        .find_map(|(derived_attr, derived_value)| {
            (derived_attr == attr_name).then_some(derived_value)
        })
}

fn font_info_vector_for_runtime_font(font_like: &Value, frame: &crate::window::Frame) -> Value {
    let opened_name = font_name_value(font_like).unwrap_or_else(|| Value::string(""));
    let full_name = opened_name;
    let size = frame.font_pixel_size.max(1.0).round() as i64;
    let height = frame.char_height.max(1.0).round() as i64;
    let average_width = frame.char_width.max(1.0).round() as i64;
    let space_width = average_width;
    let max_width = average_width;
    let ascent = ((height as f32) * 0.75).round() as i64;
    let descent = (height - ascent).max(0);
    let default_ascent = ascent;

    Value::vector(vec![
        opened_name,
        full_name,
        Value::Int(size),
        Value::Int(height),
        Value::Int(0),
        Value::Int(0),
        Value::Int(default_ascent),
        Value::Int(max_width),
        Value::Int(ascent),
        Value::Int(descent),
        Value::Int(space_width),
        Value::Int(average_width),
        Value::Nil,
        Value::Nil,
    ])
}

fn resolve_font_match(
    eval: &mut super::eval::Context,
    frame_id: FrameId,
    character: char,
    face: &RuntimeFace,
) -> Option<super::eval::ResolvedFontMatch> {
    eval.display_host
        .as_mut()
        .and_then(|host| {
            host.resolve_font_for_char(super::eval::FontResolveRequest {
                frame_id,
                character,
                face: face.clone(),
            })
            .ok()
        })
        .flatten()
}

/// `(font-at POSITION &optional WINDOW STRING)` -- resolve the effective font
/// object for the target buffer or string position.
pub(crate) fn builtin_font_at_eval(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("font-at", &args, 1)?;
    expect_max_args("font-at", &args, 3)?;

    let (frame_id, window_id) = resolve_live_window_for_font_at(eval, args.get(1))?;
    let window = eval
        .frames
        .get(frame_id)
        .and_then(|frame| frame.find_window(window_id))
        .ok_or_else(|| signal("error", vec![Value::string("Window not found")]))?;

    if let Some(string_value) = args.get(2) {
        if !string_value.is_nil() {
            let Value::Str(str_id) = *string_value else {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("stringp"), *string_value],
                ));
            };
            let pos = match args[0] {
                Value::Int(n) => n,
                Value::Char(c) => c as i64,
                other => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("fixnump"), other],
                    ));
                }
            };
            let string = with_heap(|heap| heap.get_string(str_id).to_owned());
            let char_len = string.chars().count() as i64;
            if !(0 <= pos && pos < char_len) {
                return Err(signal(
                    "args-out-of-range",
                    vec![Value::string(string), Value::Int(pos)],
                ));
            }
            let bytepos = string_elisp_pos_to_byte(&string, pos);
            let face = resolved_face_at_string_byte(eval, str_id, bytepos);
            let character = string.chars().nth(pos as usize).ok_or_else(|| {
                signal(
                    "args-out-of-range",
                    vec![Value::string(string), Value::Int(pos)],
                )
            })?;
            if let Some(matched) = resolve_font_match(eval, frame_id, character, &face) {
                return Ok(build_font_object_for_match(&face, &matched));
            }
            return Ok(build_font_object(&face));
        }
    }

    let current_buffer_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    if window.buffer_id() != Some(current_buffer_id) {
        return Err(signal(
            "error",
            vec![Value::string(
                "Specified window is not displaying the current buffer",
            )],
        ));
    }

    let pos =
        crate::emacs_core::builtins::expect_integer_or_marker_in_buffers(&eval.buffers, &args[0])?;
    let buffer = eval
        .buffers
        .get(current_buffer_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let beg = buffer.point_min_char() as i64 + 1;
    let end = buffer.point_max_char() as i64 + 1;
    if !(beg <= pos && pos < end) {
        return Err(signal(
            "args-out-of-range",
            vec![args[0], Value::Int(beg), Value::Int(end)],
        ));
    }

    let bytepos = buffer.lisp_pos_to_accessible_byte(pos);
    let face = resolved_face_at_buffer_byte(eval, buffer, bytepos);
    let character = buffer.text.char_at(bytepos).ok_or_else(|| {
        signal(
            "args-out-of-range",
            vec![args[0], Value::Int(beg), Value::Int(end)],
        )
    })?;
    if let Some(matched) = resolve_font_match(eval, frame_id, character, &face) {
        return Ok(build_font_object_for_match(&face, &matched));
    }
    Ok(build_font_object(&face))
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
    "thin",
    "ultra-light",
    "ultralight",
    "extra-light",
    "extralight",
    "light",
    "semi-light",
    "semilight",
    "demilight",
    "regular",
    "normal",
    "unspecified",
    "book",
    "medium",
    "semi-bold",
    "semibold",
    "demibold",
    "demi-bold",
    "demi",
    "bold",
    "extra-bold",
    "extrabold",
    "ultra-bold",
    "ultrabold",
    "black",
    "heavy",
    "ultra-heavy",
    "ultraheavy",
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
    "ultracondensed",
    "extra-condensed",
    "extracondensed",
    "condensed",
    "compressed",
    "narrow",
    "semi-condensed",
    "semicondensed",
    "demicondensed",
    "normal",
    "medium",
    "regular",
    "unspecified",
    "semi-expanded",
    "semiexpanded",
    "demiexpanded",
    "expanded",
    "extra-expanded",
    "extraexpanded",
    "ultra-expanded",
    "ultraexpanded",
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

/// Restore the `CREATED_LISP_FACES` set from an evaluator's face table.
/// Called after pdump load to re-populate the thread-local face name set
/// that was lost during serialization.
pub(crate) fn restore_created_faces_from_table(face_names: &[String]) {
    CREATED_LISP_FACES.with(|slot| {
        let mut set = slot.borrow_mut();
        for name in face_names {
            if !KNOWN_FACES.contains(&name.as_str()) {
                set.insert(name.clone());
            }
        }
    });
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

pub(crate) fn face_id_for_name(name: &str) -> Option<i64> {
    if let Some(id) = known_face_id(name) {
        return Some(id);
    }
    if KNOWN_FACES.contains(&name) {
        ensure_dynamic_face_id(name);
    }
    dynamic_face_id(name)
}

pub(crate) fn all_defined_face_names_sorted_by_id_desc() -> Vec<String> {
    let mut names: Vec<String> = KNOWN_FACES.iter().map(|name| (*name).to_string()).collect();
    CREATED_LISP_FACES.with(|slot| {
        for name in slot.borrow().iter() {
            if !names.iter().any(|known| known == name) {
                names.push(name.clone());
            }
        }
    });
    names.sort_by(|left, right| {
        let left_id = face_id_for_name(left).unwrap_or(i64::MAX);
        let right_id = face_id_for_name(right).unwrap_or(i64::MAX);
        right_id.cmp(&left_id).then_with(|| left.cmp(right))
    });
    names
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

pub(crate) fn clear_created_lisp_face(name: &str) {
    CREATED_LISP_FACES.with(|slot| {
        slot.borrow_mut().remove(name);
    });
    FACE_ATTR_STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        state.selected_created.remove(name);
        state.defaults_overrides.remove(name);
        state.selected_overrides.remove(name);
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

pub(crate) fn make_lisp_face_vector() -> Value {
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

fn is_reset_like_face_attr_value(value: &Value) -> bool {
    matches!(value, Value::Symbol(id) if {
        let s = resolve_sym(*id);
        s == "unspecified" || s == ":ignore-defface" || s == "reset"
    })
}

fn derived_face_attrs_from_font_value(value: &Value) -> Vec<(String, Value)> {
    let Value::Vector(id) = value else {
        return Vec::new();
    };
    if !is_font(value) {
        return Vec::new();
    }

    let elems = with_heap(|h| h.get_vector(*id).clone());
    let mut derived = Vec::new();

    for (field, attr) in [
        ("family", ":family"),
        ("foundry", ":foundry"),
        ("weight", ":weight"),
        ("slant", ":slant"),
        ("width", ":width"),
    ] {
        if let Some(v) = font_vector_get_flexible(&elems, field) {
            derived.push((attr.to_string(), v));
        }
    }

    if let Some(v) = font_vector_get_flexible(&elems, "height")
        .or_else(|| font_vector_get_flexible(&elems, "size"))
    {
        derived.push((":height".to_string(), v));
    }

    derived
}

fn apply_derived_font_face_overrides(
    face_name: &str,
    font_value: &Value,
    defaults_frame: bool,
) -> Result<(), Flow> {
    for (attr_name, attr_value) in derived_face_attrs_from_font_value(font_value) {
        let (canonical_attr, canonical_value) =
            normalize_face_attr_for_set(face_name, &attr_name, attr_value)?;
        set_face_override(face_name, &canonical_attr, canonical_value, defaults_frame);
    }
    Ok(())
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
    let is_reset_like = is_reset_like_face_attr_value(&normalized);

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

/// Eval-backed version of `internal-make-lisp-face` that also ensures the face
/// exists in the evaluator's `FaceTable`.
pub(crate) fn builtin_internal_make_lisp_face_eval(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let result = builtin_internal_make_lisp_face(args.clone())?;
    if let Ok(face_name) = require_symbol_face_name(&args[0]) {
        crate::emacs_core::xfaces::ensure_face_new_frame_defaults_entry(eval, &face_name);
        eval.face_table.ensure_face(&face_name);
        eval.face_change_count += 1;
    }
    Ok(result)
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

/// Eval-backed version of `internal-copy-lisp-face` that also mirrors the
/// copied face into the evaluator's `FaceTable`.
pub(crate) fn builtin_internal_copy_lisp_face_eval(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let result = builtin_internal_copy_lisp_face(args.clone())?;
    let from_name = resolve_copy_source_face_symbol(&args[0])?;
    let to_name = require_symbol_face_name(&args[1])?;

    let copied = eval
        .face_table
        .get(&from_name)
        .cloned()
        .unwrap_or_else(|| eval.face_table.resolve(&from_name));
    let mut copied = copied;
    copied.name = to_name.clone();
    eval.face_table.define(copied);
    eval.face_change_count += 1;

    Ok(result)
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
        if canonical_attr == ":font" && !is_reset_like_face_attr_value(&canonical_value) {
            apply_derived_font_face_overrides(&face_name, &canonical_value, defaults_frame)?;
        }
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

/// Eval-backed version of `internal-set-lisp-face-attribute` that also
/// updates the evaluator's `FaceTable`, making the face attributes
/// available to the Rust layout engine for rendering.
pub(crate) fn builtin_internal_set_lisp_face_attribute_eval(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    // First, do the existing pure logic (FACE_ATTR_STATE storage + validation)
    let result = builtin_internal_set_lisp_face_attribute(args.clone())?;

    // Now also update the evaluator's FaceTable
    if args.len() >= 3 {
        let face_name = require_symbol_face_name(&args[0]).unwrap_or_default();
        let attr_name = normalize_set_face_attribute_name(&args[1]).unwrap_or_default();
        let value = args[2];

        if !face_name.is_empty() && !attr_name.is_empty() {
            let effective_value = if attr_name == ":font" {
                live_frame_id_for_face_update(eval, args.get(3))?
                    .map(|frame_id| resolved_live_frame_font_value(eval, frame_id, &value))
                    .unwrap_or(value)
            } else {
                value
            };
            let public_effective_value = if attr_name == ":font" {
                public_live_frame_font_value(effective_value)
            } else {
                effective_value
            };

            if attr_name == ":font" && effective_value != value {
                set_face_override(&face_name, &attr_name, public_effective_value, false);
            }

            let face_attr = lisp_value_to_face_attr(&attr_name, public_effective_value);
            if let Some(fav) = face_attr {
                eval.set_face_attribute(&face_name, &attr_name, fav);
            }
            if attr_name == ":font" {
                for (derived_attr, derived_value) in
                    derived_face_attrs_from_font_value(&effective_value)
                {
                    set_face_override(&face_name, &derived_attr, derived_value, false);
                    if let Some(fav) = lisp_value_to_face_attr(&derived_attr, derived_value) {
                        eval.set_face_attribute(&face_name, &derived_attr, fav);
                    }
                }
            }

            if let Some(frame_id) = live_frame_id_for_face_update(eval, args.get(3))? {
                mirror_runtime_face_into_frame(eval, frame_id, &face_name);
            }
        }
    }

    Ok(result)
}

/// Convert a Lisp face attribute value to `FaceAttrValue` for `FaceTable`.
fn lisp_value_to_face_attr(attr_name: &str, value: Value) -> Option<crate::face::FaceAttrValue> {
    use crate::face::{
        BoxBorder, BoxStyle, Color, FaceAttrValue, FaceHeight, FontSlant, FontWeight, FontWidth,
        Underline, UnderlineStyle,
    };

    // "unspecified" symbol = reset the attribute
    if value.is_symbol_named("unspecified") {
        return Some(FaceAttrValue::Unspecified);
    }

    match attr_name {
        ":foreground" | ":background" | ":distant-foreground" => {
            let s = value.as_str()?;
            let c = Color::from_name(s).or_else(|| Color::from_hex(s))?;
            Some(FaceAttrValue::Color(c))
        }
        ":weight" => {
            let name = value.as_symbol_name()?;
            Some(FaceAttrValue::Weight(FontWeight::from_symbol(name)?))
        }
        ":slant" => {
            let name = value.as_symbol_name()?;
            Some(FaceAttrValue::Slant(FontSlant::from_symbol(name)?))
        }
        ":width" => {
            let name = value.as_symbol_name()?;
            Some(FaceAttrValue::Width(FontWidth::from_symbol(name)?))
        }
        ":height" => match value {
            Value::Int(n) => Some(FaceAttrValue::Height(FaceHeight::Absolute(n as i32))),
            Value::Float(f, _) => Some(FaceAttrValue::Height(FaceHeight::Relative(f))),
            _ => None,
        },
        ":family" | ":foundry" => {
            let s = value.as_str()?;
            Some(FaceAttrValue::Str(s.to_string()))
        }
        ":underline" => {
            if value.is_nil() {
                return Some(FaceAttrValue::Unspecified);
            }
            if matches!(value, Value::True) {
                return Some(FaceAttrValue::Bool(true));
            }
            if let Some(s) = value.as_str() {
                let color = Color::from_name(s).or_else(|| Color::from_hex(s));
                return Some(FaceAttrValue::Underline(Underline {
                    style: UnderlineStyle::Line,
                    color,
                    position: None,
                }));
            }
            // Plist form: (:style STYLE :color COLOR :position POS)
            if let Some(plist) = super::value::list_to_vec(&value) {
                let mut style = UnderlineStyle::Line;
                let mut color = None;
                let mut position = None;
                let mut i = 0;
                while i + 1 < plist.len() {
                    let key = plist[i].as_symbol_name().unwrap_or("");
                    let val = &plist[i + 1];
                    match key {
                        ":style" => {
                            style = match val.as_symbol_name().unwrap_or("line") {
                                "wave" => UnderlineStyle::Wave,
                                "dot" | "dots" => UnderlineStyle::Dot,
                                "dash" | "dashes" => UnderlineStyle::Dash,
                                "double-line" => UnderlineStyle::DoubleLine,
                                _ => UnderlineStyle::Line,
                            };
                        }
                        ":color" => {
                            if let Some(s) = val.as_str().or_else(|| val.as_symbol_name()) {
                                color = Color::from_name(s).or_else(|| Color::from_hex(s));
                            }
                        }
                        ":position" => {
                            if let Value::Int(n) = val {
                                position = Some(*n as i32);
                            }
                        }
                        _ => {}
                    }
                    i += 2;
                }
                return Some(FaceAttrValue::Underline(Underline {
                    style,
                    color,
                    position,
                }));
            }
            Some(FaceAttrValue::Bool(true))
        }
        ":overline" | ":strike-through" => {
            if value.is_nil() {
                return Some(FaceAttrValue::Bool(false));
            }
            if matches!(value, Value::True) {
                return Some(FaceAttrValue::Bool(true));
            }
            if let Some(s) = value.as_str() {
                let c = Color::from_name(s).or_else(|| Color::from_hex(s))?;
                return Some(FaceAttrValue::Color(c));
            }
            Some(FaceAttrValue::Bool(value.is_truthy()))
        }
        ":box" => {
            if value.is_nil() {
                return Some(FaceAttrValue::Unspecified);
            }
            if matches!(value, Value::True) {
                return Some(FaceAttrValue::Box(BoxBorder {
                    color: None,
                    width: 1,
                    style: BoxStyle::Flat,
                }));
            }
            if let Value::Int(n) = value {
                return Some(FaceAttrValue::Box(BoxBorder {
                    color: None,
                    width: n as i32,
                    style: BoxStyle::Flat,
                }));
            }
            // Color string shorthand
            if let Some(s) = value.as_str() {
                let color = Color::from_name(s).or_else(|| Color::from_hex(s));
                return Some(FaceAttrValue::Box(BoxBorder {
                    color,
                    width: 1,
                    style: BoxStyle::Flat,
                }));
            }
            // Plist form: (:line-width WIDTH :color COLOR :style STYLE)
            if let Some(plist) = super::value::list_to_vec(&value) {
                let mut border = BoxBorder {
                    color: None,
                    width: 1,
                    style: BoxStyle::Flat,
                };
                let mut i = 0;
                while i + 1 < plist.len() {
                    let key = plist[i].as_symbol_name().unwrap_or("");
                    let val = &plist[i + 1];
                    match key {
                        ":line-width" => {
                            if let Value::Int(n) = val {
                                border.width = *n as i32;
                            }
                        }
                        ":color" => {
                            if let Some(s) = val.as_str().or_else(|| val.as_symbol_name()) {
                                border.color = Color::from_name(s).or_else(|| Color::from_hex(s));
                            }
                        }
                        ":style" => {
                            border.style = match val.as_symbol_name().unwrap_or("flat") {
                                "released-button" => BoxStyle::Raised,
                                "pressed-button" => BoxStyle::Pressed,
                                _ => BoxStyle::Flat,
                            };
                        }
                        _ => {}
                    }
                    i += 2;
                }
                return Some(FaceAttrValue::Box(border));
            }
            Some(FaceAttrValue::Box(BoxBorder {
                color: None,
                width: 1,
                style: BoxStyle::Flat,
            }))
        }
        ":inverse-video" | ":extend" => Some(FaceAttrValue::Bool(value.is_truthy())),
        ":inherit" => {
            if value.is_nil() {
                return Some(FaceAttrValue::Inherit(Vec::new()));
            }
            if let Some(name) = value.as_symbol_name() {
                if name != "nil" {
                    return Some(FaceAttrValue::Inherit(vec![name.to_string()]));
                }
                return Some(FaceAttrValue::Inherit(Vec::new()));
            }
            if let Some(items) = super::value::list_to_vec(&value) {
                let names: Vec<String> = items
                    .iter()
                    .filter_map(|v| v.as_symbol_name().map(|s| s.to_string()))
                    .filter(|s| s != "nil")
                    .collect();
                return Some(FaceAttrValue::Inherit(names));
            }
            None
        }
        _ => None,
    }
}

fn runtime_color_to_lisp_value(color: &Color) -> Value {
    match (color.r, color.g, color.b) {
        (0, 0, 0) => Value::string("black"),
        (255, 255, 255) => Value::string("white"),
        (r, g, b) if r == g && g == b => {
            let percent = ((r as i32 * 100) + 127) / 255;
            Value::string(format!("grey{percent}"))
        }
        _ => Value::string(color.to_hex()),
    }
}

fn runtime_weight_to_lisp_value(weight: FontWeight) -> Value {
    let name = if weight == FontWeight::THIN {
        "thin"
    } else if weight == FontWeight::EXTRA_LIGHT {
        "ultra-light"
    } else if weight == FontWeight::LIGHT {
        "light"
    } else if weight == FontWeight::NORMAL {
        "normal"
    } else if weight == FontWeight::MEDIUM {
        "medium"
    } else if weight == FontWeight::SEMI_BOLD {
        "semi-bold"
    } else if weight == FontWeight::BOLD {
        "bold"
    } else if weight == FontWeight::EXTRA_BOLD {
        "extra-bold"
    } else if weight == FontWeight::BLACK {
        "black"
    } else {
        "normal"
    };
    Value::symbol(name)
}

fn runtime_slant_to_lisp_value(slant: FontSlant) -> Value {
    Value::symbol(match slant {
        FontSlant::Normal => "normal",
        FontSlant::Italic => "italic",
        FontSlant::Oblique => "oblique",
        FontSlant::ReverseItalic => "reverse-italic",
        FontSlant::ReverseOblique => "reverse-oblique",
    })
}

fn runtime_width_to_lisp_value(width: FontWidth) -> Value {
    Value::symbol(match width {
        FontWidth::UltraCondensed => "ultra-condensed",
        FontWidth::ExtraCondensed => "extra-condensed",
        FontWidth::Condensed => "condensed",
        FontWidth::SemiCondensed => "semi-condensed",
        FontWidth::Normal => "normal",
        FontWidth::SemiExpanded => "semi-expanded",
        FontWidth::Expanded => "expanded",
        FontWidth::ExtraExpanded => "extra-expanded",
        FontWidth::UltraExpanded => "ultra-expanded",
    })
}

pub(crate) fn runtime_face_attribute_value(face: &RuntimeFace, attr_name: &str) -> Value {
    match attr_name {
        ":family" => face
            .family
            .as_ref()
            .map(|value| Value::string(value.clone()))
            .unwrap_or_else(|| Value::symbol("unspecified")),
        ":foundry" => face
            .foundry
            .as_ref()
            .map(|value| Value::string(value.clone()))
            .unwrap_or_else(|| Value::symbol("unspecified")),
        ":height" => match face.height {
            Some(FaceHeight::Absolute(n)) => Value::Int(n as i64),
            Some(FaceHeight::Relative(f)) => Value::Float(f, next_float_id()),
            None => Value::symbol("unspecified"),
        },
        ":weight" => face
            .weight
            .map(runtime_weight_to_lisp_value)
            .unwrap_or_else(|| Value::symbol("unspecified")),
        ":slant" => face
            .slant
            .map(runtime_slant_to_lisp_value)
            .unwrap_or_else(|| Value::symbol("unspecified")),
        ":width" => face
            .width
            .map(runtime_width_to_lisp_value)
            .unwrap_or_else(|| Value::symbol("unspecified")),
        ":underline" => match &face.underline {
            None => Value::symbol("unspecified"),
            Some(underline)
                if underline.color.is_none()
                    && underline.position.is_none()
                    && underline.style == UnderlineStyle::Line =>
            {
                Value::True
            }
            Some(underline) => {
                let mut plist = Vec::new();
                plist.push(Value::keyword(":style"));
                plist.push(Value::symbol(match underline.style {
                    UnderlineStyle::Line => "line",
                    UnderlineStyle::Wave => "wave",
                    UnderlineStyle::DoubleLine => "double-line",
                    UnderlineStyle::Dot => "dot",
                    UnderlineStyle::Dash => "dash",
                }));
                if let Some(color) = underline.color {
                    plist.push(Value::keyword(":color"));
                    plist.push(runtime_color_to_lisp_value(&color));
                }
                if let Some(position) = underline.position {
                    plist.push(Value::keyword(":position"));
                    plist.push(Value::Int(position as i64));
                }
                Value::list(plist)
            }
        },
        ":overline" => match (face.overline, face.overline_color) {
            (Some(true), Some(color)) => runtime_color_to_lisp_value(&color),
            (Some(value), None) => Value::bool(value),
            _ => Value::symbol("unspecified"),
        },
        ":strike-through" => match (face.strike_through, face.strike_through_color) {
            (Some(true), Some(color)) => runtime_color_to_lisp_value(&color),
            (Some(value), None) => Value::bool(value),
            _ => Value::symbol("unspecified"),
        },
        ":box" => match &face.box_border {
            None => Value::symbol("unspecified"),
            Some(border) => Value::list({
                let mut plist = Vec::new();
                plist.push(Value::keyword(":line-width"));
                plist.push(Value::Int(border.width as i64));
                if let Some(color) = border.color {
                    plist.push(Value::keyword(":color"));
                    plist.push(runtime_color_to_lisp_value(&color));
                }
                plist.push(Value::keyword(":style"));
                plist.push(Value::symbol(match border.style {
                    BoxStyle::Flat => "flat",
                    BoxStyle::Raised => "released-button",
                    BoxStyle::Pressed => "pressed-button",
                }));
                plist
            }),
        },
        ":inverse-video" => face
            .inverse_video
            .map(Value::bool)
            .unwrap_or_else(|| Value::symbol("unspecified")),
        ":foreground" => face
            .foreground
            .as_ref()
            .map(runtime_color_to_lisp_value)
            .unwrap_or_else(|| Value::symbol("unspecified")),
        ":distant-foreground" => face
            .distant_foreground
            .as_ref()
            .map(runtime_color_to_lisp_value)
            .unwrap_or_else(|| Value::symbol("unspecified")),
        ":background" => face
            .background
            .as_ref()
            .map(runtime_color_to_lisp_value)
            .unwrap_or_else(|| Value::symbol("unspecified")),
        ":stipple" | ":font" | ":fontset" => Value::symbol("unspecified"),
        ":inherit" => {
            if face.inherit.is_empty() {
                Value::Nil
            } else if face.inherit.len() == 1 {
                Value::symbol(face.inherit[0].as_str())
            } else {
                Value::list(
                    face.inherit
                        .iter()
                        .map(|name| Value::symbol(name.as_str()))
                        .collect(),
                )
            }
        }
        ":extend" => face
            .extend
            .map(Value::bool)
            .unwrap_or_else(|| Value::symbol("unspecified")),
        _ => Value::symbol("unspecified"),
    }
}

pub(crate) fn runtime_face_to_lisp_vector(face: &RuntimeFace) -> Value {
    let mut values = Vec::with_capacity(LISP_FACE_VECTOR_LEN);
    values.push(Value::symbol("face"));
    values.extend(
        LISP_FACE_VECTOR_ATTRIBUTES
            .iter()
            .map(|attr| runtime_face_attribute_value(face, attr)),
    );
    Value::vector(values)
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

pub(crate) fn builtin_internal_get_lisp_face_attribute_eval(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
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

    if defaults_frame {
        return Ok(lisp_face_attribute_value(&face_name, &attr_name, true));
    }

    let frame_id = match args.get(2) {
        None | Some(Value::Nil) => Some(super::window_cmds::ensure_selected_frame_id(eval)),
        Some(frame) if frame_device_designator_p(frame) => frame_id_from_designator(frame),
        _ => None,
    };

    if face_name == "default"
        && get_face_override(&face_name, &attr_name, false).is_none()
        && matches!(
            attr_name.as_str(),
            ":font" | ":family" | ":foundry" | ":weight" | ":slant" | ":width" | ":height"
        )
    {
        if let Some(frame_id) = frame_id {
            if let Some(fallback) = live_frame_font_attribute_fallback(eval, frame_id, &attr_name) {
                return Ok(fallback);
            }
        }
    }

    let lisp_value = lisp_face_attribute_value(&face_name, &attr_name, false);
    let lisp_value_unspecified = matches!(
        &lisp_value,
        Value::Symbol(id) if resolve_sym(*id) == "unspecified"
    ) || matches!(
        (&*attr_name, &lisp_value),
        (":foreground", Value::Str(id)) if with_heap(|h| h.get_string(*id) == "unspecified-fg")
    ) || matches!(
        (&*attr_name, &lisp_value),
        (":background", Value::Str(id)) if with_heap(|h| h.get_string(*id) == "unspecified-bg")
    );
    if !lisp_value_unspecified {
        return Ok(lisp_value);
    }

    if let Some(face) = eval.face_table().get(&face_name) {
        return Ok(runtime_face_attribute_value(face, &attr_name));
    }

    Ok(lisp_value)
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

pub(crate) fn builtin_internal_merge_in_global_face_eval(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    let result = builtin_internal_merge_in_global_face(args.clone())?;
    let face_name = resolve_face_name_for_merge(&args[0])?;
    FACE_ATTR_STATE.with(|slot| {
        let state = slot.borrow();
        if let Some(attrs) = state.defaults_overrides.get(&face_name) {
            for (attr_name, value) in attrs {
                if let Some(face_attr) = lisp_value_to_face_attr(attr_name, *value) {
                    eval.set_face_attribute(&face_name, attr_name, face_attr);
                }
                if attr_name == ":font" {
                    for (derived_attr, derived_value) in derived_face_attrs_from_font_value(value) {
                        if let Some(face_attr) =
                            lisp_value_to_face_attr(&derived_attr, derived_value)
                        {
                            eval.set_face_attribute(&face_name, &derived_attr, face_attr);
                        }
                    }
                }
            }
        }
    });
    if let Some(frame_id) = live_frame_id_for_face_update(eval, args.get(1))? {
        mirror_runtime_face_into_frame(eval, frame_id, &face_name);
    }
    Ok(result)
}

/// `(face-attribute-relative-p ATTRIBUTE VALUE)` -- return t if VALUE is the
/// value is a relative form for ATTRIBUTE.
pub(crate) fn builtin_face_attribute_relative_p(args: Vec<Value>) -> EvalResult {
    expect_args("face-attribute-relative-p", &args, 2)?;
    let value_is_relative_reset = matches!(&args[1], Value::Symbol(id) | Value::Keyword(id) if {
        matches!(resolve_sym(*id), "unspecified" | ":ignore-defface" | "ignore-defface")
    });
    if value_is_relative_reset {
        return Ok(Value::True);
    }

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
    let value1_is_relative_reset = matches!(&args[1], Value::Symbol(id) | Value::Keyword(id) if {
        matches!(resolve_sym(*id), "unspecified" | ":ignore-defface" | "ignore-defface")
    });
    if value1_is_relative_reset {
        return Ok(args[2]);
    }

    let height_attr = matches!(&args[0], Value::Keyword(id) | Value::Symbol(id) if {
        matches!(resolve_sym(*id), "height" | ":height")
    });
    if height_attr {
        return Ok(match (&args[1], &args[2]) {
            (Value::Int(_), _) | (Value::Char(_), _) => args[1],
            (Value::Float(scale, _), Value::Int(height)) => {
                Value::Int((*scale * *height as f64) as i64)
            }
            (Value::Float(scale, _), Value::Char(height)) => {
                Value::Int((*scale * *height as u32 as f64) as i64)
            }
            (Value::Float(scale, _), Value::Float(other_scale, _)) => {
                Value::Float(*scale * *other_scale, next_float_id())
            }
            (Value::Float(_, _), _) => args[1],
            _ => args[1],
        });
    }

    Ok(args[1])
}

/// `(face-list &optional FRAME)` -- return list of known face names.
pub(crate) fn builtin_face_list(args: Vec<Value>) -> EvalResult {
    expect_max_args("face-list", &args, 1)?;
    Ok(Value::list(
        all_defined_face_names_sorted_by_id_desc()
            .into_iter()
            .map(Value::symbol)
            .collect(),
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

pub(crate) fn builtin_face_font_eval(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("face-font", &args, 1)?;
    expect_max_args("face-font", &args, 3)?;

    let defaults_frame = matches!(args.get(1), Some(Value::True));
    if defaults_frame {
        let face_name = resolve_face_name_for_domain(&args[0], true)?;
        let mut styles = Vec::new();
        let weight = lisp_face_attribute_value(&face_name, ":weight", true);
        if matches!(weight.as_symbol_name(), Some(name) if name != "normal" && name != "unspecified")
        {
            styles.push(Value::symbol("bold"));
        }
        let slant = lisp_face_attribute_value(&face_name, ":slant", true);
        if matches!(slant.as_symbol_name(), Some(name) if name != "normal" && name != "unspecified")
        {
            styles.push(Value::symbol("italic"));
        }
        return if styles.is_empty() {
            Ok(Value::Nil)
        } else {
            Ok(Value::list(styles))
        };
    }

    let frame_id = match args.get(1) {
        None | Some(Value::Nil) => super::window_cmds::ensure_selected_frame_id(eval),
        Some(frame) if live_frame_designator_in_state(&eval.frames, frame) => {
            frame_id_from_designator(frame)
                .expect("live frame designator should decode to frame id")
        }
        Some(other) => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *other],
            ));
        }
    };
    let frame = eval
        .frames
        .get(frame_id)
        .ok_or_else(|| signal("error", vec![Value::string("No selected frame")]))?;
    if frame.window_system.is_none() {
        return builtin_face_font(args);
    }

    let face_name = resolve_face_name_for_domain(&args[0], false)?;
    let face = eval.face_table.resolve(&face_name);
    if let Some(character) = args.get(2).filter(|value| !value.is_nil()) {
        let ch = match character {
            Value::Char(ch) => *ch,
            Value::Int(code) => char::from_u32(*code as u32).ok_or_else(|| {
                signal(
                    "wrong-type-argument",
                    vec![Value::symbol("characterp"), *character],
                )
            })?,
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("characterp"), *character],
                ));
            }
        };
        if let Some(matched) = resolve_font_match(eval, frame_id, ch, &face) {
            return Ok(
                font_name_value(&build_font_object_for_match(&face, &matched))
                    .unwrap_or(Value::Nil),
            );
        }
    }

    Ok(font_name_value(&build_font_object(&face)).unwrap_or(Value::Nil))
}

pub(crate) fn builtin_font_info_eval(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("font-info", &args, 1)?;
    expect_max_args("font-info", &args, 2)?;

    let frame_id = match args.get(1) {
        None | Some(Value::Nil) => super::window_cmds::ensure_selected_frame_id(eval),
        Some(frame) if live_frame_designator_in_state(&eval.frames, frame) => {
            frame_id_from_designator(frame)
                .expect("live frame designator should decode to frame id")
        }
        Some(other) => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("frame-live-p"), *other],
            ));
        }
    };
    let frame = eval
        .frames
        .get(frame_id)
        .ok_or_else(|| signal("error", vec![Value::string("No selected frame")]))?;
    if frame.window_system.is_none() {
        return crate::emacs_core::builtins::builtin_font_info(args);
    }

    match &args[0] {
        Value::Str(_) => Ok(font_info_vector_for_runtime_font(&args[0], frame)),
        value if is_font(value) => Ok(font_info_vector_for_runtime_font(value, frame)),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
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
    let mut alist = Vec::with_capacity(entries.len());
    for entry in entries {
        let members = proper_list_to_vec_or_listp_error(&entry)?;
        let mut converted = Vec::with_capacity(members.len());
        let mut names = Vec::with_capacity(members.len());
        for member in members {
            match member {
                Value::Str(id) => {
                    let name = with_heap(|h| h.get_string(id).to_owned());
                    converted.push(Value::symbol(name.clone()));
                    names.push(name);
                }
                other => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("stringp"), other],
                    ));
                }
            }
        }
        if let Some(name) = names.first() {
            alist.push((name.to_ascii_lowercase(), names));
        }
        normalized.push(Value::list(converted));
    }
    if let Ok(mut state) = alternative_font_family_alist().write() {
        *state = alist;
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
