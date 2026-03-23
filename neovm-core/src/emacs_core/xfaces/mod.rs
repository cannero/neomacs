//! Bootstrap-facing subset of GNU Emacs's `xfaces.c`.
//!
//! Face-related builtins are still mostly implemented in `face.rs` and
//! `font.rs`, but GNU startup also relies on a small set of C-level
//! variables from `xfaces.c` being bound before Lisp runs. Keep those
//! defaults here so Rust startup matches the same ownership boundary.

use crate::emacs_core::error::{EvalResult, signal};
use crate::emacs_core::symbol::Obarray;
use crate::emacs_core::value::{HashKey, HashTableTest, Value, with_heap_mut};

/// Register bootstrap variables owned by the face subsystem.
pub fn register_bootstrap_vars(obarray: &mut Obarray) {
    obarray.set_symbol_value("face-filters-always-match", Value::Nil);
    let face_new_frame_defaults = Value::hash_table(HashTableTest::Eq);
    crate::emacs_core::font::seed_face_new_frame_defaults_table(face_new_frame_defaults);
    obarray.set_symbol_value("face--new-frame-defaults", face_new_frame_defaults);
    obarray.set_symbol_value("face-default-stipple", Value::string("gray3"));
    obarray.set_symbol_value("tty-defined-color-alist", Value::Nil);
    obarray.set_symbol_value("scalable-fonts-allowed", Value::Nil);
    obarray.set_symbol_value("face-ignored-fonts", Value::Nil);
    obarray.set_symbol_value("face-remapping-alist", Value::Nil);
    obarray.set_symbol_value("face-font-rescale-alist", Value::Nil);
    obarray.set_symbol_value("face-near-same-color-threshold", Value::Int(30_000));
    obarray.set_symbol_value("face-font-lax-matched-attributes", Value::True);
}

pub(crate) fn builtin_frame_face_hash_table_eval(
    eval: &mut crate::emacs_core::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    crate::emacs_core::display::expect_range_args("frame--face-hash-table", &args, 0, 1)?;
    let frame_id = crate::emacs_core::window_cmds::resolve_frame_id_in_state(
        &mut eval.frames,
        &mut eval.buffers,
        args.first(),
        "frame-live-p",
    )?;

    let table = Value::hash_table(HashTableTest::Eq);
    let Value::HashTable(table_id) = table else {
        unreachable!("hash table constructor must return a hash table");
    };

    let face_entries: Vec<(Value, Value)> = eval
        .frames
        .get(frame_id)
        .into_iter()
        .flat_map(|frame| frame.realized_faces.iter())
        .into_iter()
        .map(|(name, face)| {
            (
                Value::symbol(name.as_str()),
                crate::emacs_core::font::runtime_face_to_lisp_vector(face),
            )
        })
        .collect();

    with_heap_mut(|heap| {
        let hash_table = heap.get_hash_table_mut(table_id);
        for (key, value) in face_entries {
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
        }
    });

    Ok(table)
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
        let mut eval = crate::emacs_core::eval::Evaluator::new();
        let out = builtin_frame_face_hash_table_eval(&mut eval, vec![Value::Nil])
            .expect("live frame face hash table");
        let Value::HashTable(id) = out else {
            panic!("expected hash table");
        };
        let len = with_heap(|heap| heap.get_hash_table(id).data.len());
        assert_eq!(len, 0);
    }
}
