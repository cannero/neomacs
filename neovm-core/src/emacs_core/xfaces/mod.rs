//! Bootstrap-facing subset of GNU Emacs's `xfaces.c`.
//!
//! Face-related builtins are still mostly implemented in `face.rs` and
//! `font.rs`, but GNU startup also relies on a small set of C-level
//! variables from `xfaces.c` being bound before Lisp runs. Keep those
//! defaults here so Rust startup matches the same ownership boundary.

use crate::emacs_core::error::{EvalResult, signal};
use crate::emacs_core::intern::resolve_sym;
use crate::emacs_core::symbol::Obarray;
use crate::emacs_core::value::{HashKey, HashTableTest, Value, list_to_vec, with_heap_mut, ValueKind};
use crate::face::Face as RuntimeFace;

const FACE_ATTRIBUTES_VECTOR_LEN: usize = 20;

/// Register bootstrap variables owned by the face subsystem.
pub fn register_bootstrap_vars(obarray: &mut Obarray) {
    obarray.set_symbol_value("face-filters-always-match", Value::NIL);
    obarray.set_symbol_value(
        "face--new-frame-defaults",
        bootstrap_face_new_frame_defaults_table(),
    );
    obarray.set_symbol_value("face-default-stipple", Value::string("gray3"));
    obarray.set_symbol_value("tty-defined-color-alist", Value::NIL);
    obarray.set_symbol_value("scalable-fonts-allowed", Value::NIL);
    obarray.set_symbol_value("face-ignored-fonts", Value::NIL);
    obarray.set_symbol_value("face-remapping-alist", Value::NIL);
    obarray.set_symbol_value("face-font-rescale-alist", Value::NIL);
    obarray.set_symbol_value("face-near-same-color-threshold", Value::fixnum(30_000));
    obarray.set_symbol_value("face-font-lax-matched-attributes", Value::T);
}

/// Backfill xfaces-owned bootstrap variables after loading a dump or partial
/// source bootstrap. GNU owns these in xfaces.c, so load/bootstrap glue should
/// delegate here instead of duplicating the values itself.
pub(crate) fn ensure_startup_compat_variables(eval: &mut crate::emacs_core::eval::Context) {
    let defaults = [
        ("face-filters-always-match", Value::NIL),
        (
            "face--new-frame-defaults",
            bootstrap_face_new_frame_defaults_table(),
        ),
        ("face-default-stipple", Value::string("gray3")),
        ("scalable-fonts-allowed", Value::NIL),
        ("face-ignored-fonts", Value::NIL),
        ("face-remapping-alist", Value::NIL),
        ("face-font-rescale-alist", Value::NIL),
        ("face-near-same-color-threshold", Value::fixnum(30_000)),
        ("face-font-lax-matched-attributes", Value::T),
    ];
    for (name, value) in defaults {
        if eval.obarray().symbol_value(name).is_none() {
            eval.set_variable(name, value);
        }
    }
}

pub(crate) fn builtin_frame_face_hash_table(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    crate::emacs_core::display::expect_range_args("frame--face-hash-table", &args, 0, 1)?;
    let frame_id = crate::emacs_core::window_cmds::resolve_frame_id_in_state(
        &mut eval.frames,
        &mut eval.buffers,
        args.first(),
        "frame-live-p",
    )?;

    Ok(eval
        .frames
        .get(frame_id)
        .map(|frame| frame.face_hash_table())
        .unwrap_or(Value::hash_table(HashTableTest::Eq)))
}

fn unspecified_face_attributes_vector() -> Value {
    Value::vector(vec![
        Value::symbol("unspecified");
        FACE_ATTRIBUTES_VECTOR_LEN
    ])
}

fn face_attributes_vector_slot(attr_name: &str) -> Option<usize> {
    match attr_name {
        ":family" => Some(1),
        ":foundry" => Some(2),
        ":width" => Some(3),
        ":height" => Some(4),
        ":weight" => Some(5),
        ":slant" => Some(6),
        ":underline" => Some(7),
        ":inverse-video" => Some(8),
        ":foreground" => Some(9),
        ":background" => Some(10),
        ":stipple" => Some(11),
        ":overline" => Some(12),
        ":strike-through" => Some(13),
        ":box" => Some(14),
        ":font" => Some(15),
        ":inherit" => Some(16),
        ":fontset" => Some(17),
        ":distant-foreground" => Some(18),
        ":extend" => Some(19),
        _ => None,
    }
}

fn face_attr_key_name(value: &Value) -> Option<&str> {
    match value.kind() {
        ValueKind::Keyword(id) | ValueKind::Symbol(id) => Some(resolve_sym(id)),
        _ => None,
    }
}

pub(crate) fn builtin_face_attributes_as_vector(args: Vec<Value>) -> EvalResult {
    crate::emacs_core::display::expect_args("face-attributes-as-vector", &args, 1)?;

    let mut attrs = vec![Value::symbol("unspecified"); FACE_ATTRIBUTES_VECTOR_LEN];
    let Some(plist) = list_to_vec(&args[0]) else {
        return Ok(Value::vector(attrs));
    };

    let mut i = 0;
    while i + 1 < plist.len() {
        let Some(attr_name) = face_attr_key_name(&plist[i]) else {
            i += 2;
            continue;
        };
        let Some(slot) = face_attributes_vector_slot(attr_name) else {
            i += 2;
            continue;
        };

        let value = plist[i + 1];
        match attr_name {
            ":foreground" | ":background" | ":distant-foreground" if value.is_nil() => {}
            ":stipple" | ":font" | ":inherit" | ":fontset" => {}
            ":box" if value.is_t() => attrs[slot] = Value::fixnum(1),
            _ => attrs[slot] = value,
        }

        i += 2;
    }

    Ok(Value::vector(attrs))
}

pub(crate) fn mirror_runtime_face_into_frame(
    frame: &mut crate::window::Frame,
    face_name: &str,
    face: &RuntimeFace,
) {
    frame.set_realized_face(face_name.to_string(), face.clone());
    upsert_frame_face_hash_entry(
        frame.face_hash_table(),
        Value::symbol(face_name),
        crate::emacs_core::font::runtime_face_to_lisp_vector(face),
    );
}

pub(crate) fn seed_face_new_frame_defaults_table(table: Value) {
    let face_names = crate::emacs_core::font::all_defined_face_names_sorted_by_id_desc();
    let face_entries: Vec<(Value, Value)> = face_names
        .into_iter()
        .filter_map(|face_name| {
            let face_id = crate::emacs_core::font::face_id_for_name(&face_name)?;
            Some((
                Value::symbol(face_name.as_str()),
                Value::cons(
                    Value::fixnum(face_id),
                    crate::emacs_core::font::make_lisp_face_vector(),
                ),
            ))
        })
        .collect();

    for (key, value) in face_entries {
        upsert_frame_face_hash_entry(table, key, value);
    }
}

fn bootstrap_face_new_frame_defaults_table() -> Value {
    let table = Value::hash_table(HashTableTest::Eq);
    seed_face_new_frame_defaults_table(table);
    table
}

pub(crate) fn ensure_face_new_frame_defaults_entry(
    eval: &mut crate::emacs_core::eval::Context,
    face_name: &str,
) -> Option<Value> {
    let table = eval
        .obarray()
        .symbol_value("face--new-frame-defaults")
        .copied()?;
    seed_face_new_frame_defaults_table(table);
    let face_id = crate::emacs_core::font::face_id_for_name(face_name)?;
    upsert_frame_face_hash_entry(
        table,
        Value::symbol(face_name),
        Value::cons(
            Value::fixnum(face_id),
            crate::emacs_core::font::make_lisp_face_vector(),
        ),
    );
    Some(table)
}

fn upsert_frame_face_hash_entry(table: Value, key: Value, value: Value) {
    if !table.is_hash_table() {
        unreachable!("frame face hash table must be a hash table");
    };
    with_heap_mut(|heap| {
        let hash_table = heap.get_hash_table_mut(table_id);
        let hash_key = match key.kind() {
            ValueKind::Symbol(id) => HashKey::Symbol(id),
            ValueKind::Keyword(id) => HashKey::Keyword(id),
            _ => unreachable!("face hash keys are symbols"),
        };
        if !hash_table.data.contains_key(&hash_key) {
            hash_table.insertion_order.push(hash_key.clone());
        }
        hash_table.key_snapshots.insert(hash_key.clone(), key);
        hash_table.data.insert(hash_key, value);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emacs_core::value::with_heap;

    #[test]
    fn register_bootstrap_vars_matches_gnu_defaults() {
        let mut obarray = Obarray::new();
        register_bootstrap_vars(&mut obarray);

        assert_eq!(
            obarray.symbol_value("face-default-stipple").copied(),
            Some(Value::string("gray3"))
        );
        assert_eq!(
            obarray
                .symbol_value("face-near-same-color-threshold")
                .copied(),
            Some(Value::fixnum(30_000))
        );
        assert_eq!(
            obarray
                .symbol_value("face-font-lax-matched-attributes")
                .copied(),
            Some(Value::T)
        );

        let table = obarray
            .symbol_value("face--new-frame-defaults")
            .copied()
            .expect("face--new-frame-defaults");
        if !table.is_hash_table() {
            panic!("face--new-frame-defaults must be a hash table");
        };
        let test = with_heap(|heap| heap.get_hash_table(id).test.clone());
        assert_eq!(test, HashTableTest::Eq);
    }

    #[test]
    fn frame_face_hash_table_eval_is_empty_before_any_face_realization() {
        let mut eval = crate::emacs_core::eval::Context::new();
        let out = builtin_frame_face_hash_table(&mut eval, vec![Value::NIL])
            .expect("live frame face hash table");
        if !out.is_hash_table() {
            panic!("expected hash table");
        };
        let len = with_heap(|heap| heap.get_hash_table(id).data.len());
        assert_eq!(len, 0);
    }

    #[test]
    fn frame_face_hash_table_eval_returns_stable_frame_owned_table() {
        let mut eval = crate::emacs_core::eval::Context::new();
        let first = builtin_frame_face_hash_table(&mut eval, vec![Value::NIL])
            .expect("first face hash table");
        let second = builtin_frame_face_hash_table(&mut eval, vec![Value::NIL])
            .expect("second face hash table");
        assert_eq!(first, second);
    }

    #[test]
    fn ensure_startup_compat_variables_backfills_missing_xfaces_state() {
        let mut eval = crate::emacs_core::eval::Context::new();
        for name in [
            "face-filters-always-match",
            "face--new-frame-defaults",
            "face-default-stipple",
            "scalable-fonts-allowed",
            "face-ignored-fonts",
            "face-remapping-alist",
            "face-font-rescale-alist",
            "face-near-same-color-threshold",
            "face-font-lax-matched-attributes",
        ] {
            eval.obarray_mut().makunbound(name);
        }

        ensure_startup_compat_variables(&mut eval);

        assert_eq!(
            eval.obarray().symbol_value("face-default-stipple").copied(),
            Some(Value::string("gray3"))
        );
        let table = eval
            .obarray()
            .symbol_value("face--new-frame-defaults")
            .copied()
            .expect("face hash table backfilled");
        if !table.is_hash_table() {
            panic!("face--new-frame-defaults must be a hash table");
        };
        let has_seeded_faces = with_heap(|heap| {
            let hash_table = heap.get_hash_table(id);
            hash_table
                .data
                .contains_key(&HashKey::Symbol(crate::emacs_core::intern::intern(
                    "default",
                )))
                && hash_table.data.contains_key(&HashKey::Symbol(
                    crate::emacs_core::intern::intern("mode-line"),
                ))
        });
        assert!(
            has_seeded_faces,
            "face--new-frame-defaults should be preseeded with GNU face entries"
        );
    }
}
