//! Bootstrap-facing subset of GNU Emacs's `xfaces.c`.
//!
//! Face-related builtins are still mostly implemented in `face.rs` and
//! `font.rs`, but GNU startup also relies on a small set of C-level
//! variables from `xfaces.c` being bound before Lisp runs. Keep those
//! defaults here so Rust startup matches the same ownership boundary.

use crate::emacs_core::error::{EvalResult, signal};
use crate::emacs_core::symbol::Obarray;
use crate::emacs_core::value::{HashKey, HashTableTest, Value, with_heap_mut};
use crate::face::Face as RuntimeFace;

/// Register bootstrap variables owned by the face subsystem.
pub fn register_bootstrap_vars(obarray: &mut Obarray) {
    obarray.set_symbol_value("face-filters-always-match", Value::Nil);
    obarray.set_symbol_value(
        "face--new-frame-defaults",
        bootstrap_face_new_frame_defaults_table(),
    );
    obarray.set_symbol_value("face-default-stipple", Value::string("gray3"));
    obarray.set_symbol_value("tty-defined-color-alist", Value::Nil);
    obarray.set_symbol_value("scalable-fonts-allowed", Value::Nil);
    obarray.set_symbol_value("face-ignored-fonts", Value::Nil);
    obarray.set_symbol_value("face-remapping-alist", Value::Nil);
    obarray.set_symbol_value("face-font-rescale-alist", Value::Nil);
    obarray.set_symbol_value("face-near-same-color-threshold", Value::Int(30_000));
    obarray.set_symbol_value("face-font-lax-matched-attributes", Value::True);
}

/// Backfill xfaces-owned bootstrap variables after loading a dump or partial
/// source bootstrap. GNU owns these in xfaces.c, so load/bootstrap glue should
/// delegate here instead of duplicating the values itself.
pub(crate) fn ensure_startup_compat_variables(eval: &mut crate::emacs_core::eval::Context) {
    let defaults = [
        ("face-filters-always-match", Value::Nil),
        (
            "face--new-frame-defaults",
            bootstrap_face_new_frame_defaults_table(),
        ),
        ("face-default-stipple", Value::string("gray3")),
        ("scalable-fonts-allowed", Value::Nil),
        ("face-ignored-fonts", Value::Nil),
        ("face-remapping-alist", Value::Nil),
        ("face-font-rescale-alist", Value::Nil),
        ("face-near-same-color-threshold", Value::Int(30_000)),
        ("face-font-lax-matched-attributes", Value::True),
    ];
    for (name, value) in defaults {
        if eval.obarray().symbol_value(name).is_none() {
            eval.set_variable(name, value);
        }
    }
}

pub(crate) fn builtin_frame_face_hash_table_eval(
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
                    Value::Int(face_id),
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
            Value::Int(face_id),
            crate::emacs_core::font::make_lisp_face_vector(),
        ),
    );
    Some(table)
}

fn upsert_frame_face_hash_entry(table: Value, key: Value, value: Value) {
    let Value::HashTable(table_id) = table else {
        unreachable!("frame face hash table must be a hash table");
    };
    with_heap_mut(|heap| {
        let hash_table = heap.get_hash_table_mut(table_id);
        let hash_key = match key {
            Value::Symbol(id) => HashKey::Symbol(id),
            Value::Keyword(id) => HashKey::Keyword(id),
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
            Some(Value::Int(30_000))
        );
        assert_eq!(
            obarray
                .symbol_value("face-font-lax-matched-attributes")
                .copied(),
            Some(Value::True)
        );

        let table = obarray
            .symbol_value("face--new-frame-defaults")
            .copied()
            .expect("face--new-frame-defaults");
        let Value::HashTable(id) = table else {
            panic!("face--new-frame-defaults must be a hash table");
        };
        let test = with_heap(|heap| heap.get_hash_table(id).test.clone());
        assert_eq!(test, HashTableTest::Eq);
    }

    #[test]
    fn frame_face_hash_table_eval_is_empty_before_any_face_realization() {
        let mut eval = crate::emacs_core::eval::Context::new();
        let out = builtin_frame_face_hash_table_eval(&mut eval, vec![Value::Nil])
            .expect("live frame face hash table");
        let Value::HashTable(id) = out else {
            panic!("expected hash table");
        };
        let len = with_heap(|heap| heap.get_hash_table(id).data.len());
        assert_eq!(len, 0);
    }

    #[test]
    fn frame_face_hash_table_eval_returns_stable_frame_owned_table() {
        let mut eval = crate::emacs_core::eval::Context::new();
        let first = builtin_frame_face_hash_table_eval(&mut eval, vec![Value::Nil])
            .expect("first face hash table");
        let second = builtin_frame_face_hash_table_eval(&mut eval, vec![Value::Nil])
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
        let Value::HashTable(id) = table else {
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
