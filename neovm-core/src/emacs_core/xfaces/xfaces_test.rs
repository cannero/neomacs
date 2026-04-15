use super::*;
#[test]
fn register_bootstrap_vars_matches_gnu_defaults() {
    crate::test_utils::init_test_tracing();
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
    let test = table.as_hash_table().unwrap().test.clone();
    assert_eq!(test, HashTableTest::Eq);
}

#[test]
fn frame_face_hash_table_eval_is_empty_before_any_face_realization() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    let out = builtin_frame_face_hash_table(&mut eval, vec![Value::NIL])
        .expect("live frame face hash table");
    if !out.is_hash_table() {
        panic!("expected hash table");
    };
    let len = out.as_hash_table().unwrap().data.len();
    assert_eq!(len, 0);
}

#[test]
fn frame_face_hash_table_eval_returns_stable_frame_owned_table() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    let first = builtin_frame_face_hash_table(&mut eval, vec![Value::NIL])
        .expect("first face hash table");
    let second = builtin_frame_face_hash_table(&mut eval, vec![Value::NIL])
        .expect("second face hash table");
    assert_eq!(first, second);
}

#[test]
fn mirror_runtime_face_into_frame_uses_symbol_keys() {
    crate::test_utils::init_test_tracing();
    let mut eval = crate::emacs_core::eval::Context::new();
    let frame_id = crate::emacs_core::window_cmds::ensure_selected_frame_id(&mut eval);
    let face = eval.face_table().resolve("default");

    let frame = eval.frames.get_mut(frame_id).expect("selected frame");
    mirror_runtime_face_into_frame(frame, "default", &face);

    assert!(frame.realized_faces.contains_key(&Value::symbol("default")));
    assert!(frame.realized_face("default").is_some());
}

#[test]
fn ensure_startup_compat_variables_backfills_missing_xfaces_state() {
    crate::test_utils::init_test_tracing();
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
    let has_seeded_faces = {
        let hash_table = table.as_hash_table().unwrap();
        hash_table
            .data
            .contains_key(&HashKey::Symbol(crate::emacs_core::intern::intern(
                "default",
            )))
            && hash_table.data.contains_key(&HashKey::Symbol(
                crate::emacs_core::intern::intern("mode-line"),
            ))
    };
    assert!(
        has_seeded_faces,
        "face--new-frame-defaults should be preseeded with GNU face entries"
    );
}
