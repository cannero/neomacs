use super::*;

fn unibyte_name(bytes: &[u8]) -> crate::heap_types::LispString {
    crate::heap_types::LispString::from_unibyte(bytes.to_vec())
}

#[test]
fn name_interner_dedup() {
    crate::test_utils::init_test_tracing();
    let mut interner = StringInterner::new();
    let a = interner.intern("foo");
    let b = interner.intern("foo");
    let c = interner.intern("bar");
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_eq!(interner.resolve(a), "foo");
    assert_eq!(interner.resolve(c), "bar");
}

#[test]
fn runtime_intern() {
    crate::test_utils::init_test_tracing();
    let a = intern("hello");
    let b = intern("hello");
    let c = intern("world");
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_eq!(resolve_sym(a), "hello");
    assert_eq!(resolve_sym(c), "world");
}

#[test]
fn runtime_symbol_name_id_stable_across_growth() {
    crate::test_utils::init_test_tracing();
    let early = intern("early-runtime-name");
    let early_name = symbol_name_id(early);
    for i in 0..500 {
        intern(&format!("growth-runtime-{i}"));
    }
    assert_eq!(symbol_name_id(early), early_name);
    assert_eq!(resolve_name(early_name), "early-runtime-name");
}

#[test]
fn name_interner_empty_string() {
    crate::test_utils::init_test_tracing();
    let mut interner = StringInterner::new();
    let id = interner.intern("");
    assert_eq!(interner.resolve(id), "");
    assert_eq!(interner.intern(""), id);
}

#[test]
fn name_interner_many_strings() {
    crate::test_utils::init_test_tracing();
    let mut interner = StringInterner::new();
    let ids: Vec<NameId> = (0..1000)
        .map(|i| interner.intern(&format!("sym-{i}")))
        .collect();
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(interner.resolve(*id), format!("sym-{i}"));
    }
    let unique: std::collections::HashSet<NameId> = ids.iter().copied().collect();
    assert_eq!(unique.len(), 1000);
}

#[test]
fn name_interner_idempotent() {
    crate::test_utils::init_test_tracing();
    let mut interner = StringInterner::new();
    let first = interner.intern("repeated");
    for _ in 0..100 {
        assert_eq!(interner.intern("repeated"), first);
    }
}

#[test]
fn name_interner_canonicalizes_ascii_multibyte_names_to_unibyte_atoms() {
    crate::test_utils::init_test_tracing();
    let mut interner = StringInterner::new();
    let multibyte = crate::heap_types::LispString::from_utf8("batch-byte-compile");
    let unibyte = crate::heap_types::LispString::from_unibyte(b"batch-byte-compile".to_vec());

    let from_multibyte = interner.intern_lisp_string(&multibyte);
    let from_unibyte = interner.intern_lisp_string(&unibyte);

    assert_eq!(from_multibyte, from_unibyte);
    let resolved = interner.resolve_lisp_string(from_multibyte);
    assert_eq!(resolved.as_bytes(), b"batch-byte-compile");
    assert!(!resolved.is_multibyte());
}

#[test]
fn name_interner_lookup_reuses_ascii_multibyte_canonical_atom() {
    crate::test_utils::init_test_tracing();
    let mut interner = StringInterner::new();
    let multibyte = crate::heap_types::LispString::from_utf8("symbol-name");

    let id = interner.intern_lisp_string(&multibyte);

    assert_eq!(interner.lookup("symbol-name"), Some(id));
    assert_eq!(interner.intern("symbol-name"), id);
}

#[test]
fn symid_copy_eq_hash() {
    crate::test_utils::init_test_tracing();
    let mut registry = SymbolRegistry::new();
    let a = registry.intern("x");
    let b = a;
    assert_eq!(a, b);

    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(a);
    assert!(set.contains(&b));
}

#[test]
fn resolve_sym_stable_across_growth() {
    crate::test_utils::init_test_tracing();
    let mut registry = SymbolRegistry::new();
    let early = registry.intern("early");
    assert_eq!(registry.resolve(early), "early");
    for i in 0..500 {
        registry.intern(&format!("growth-{i}"));
    }
    assert_eq!(registry.resolve(early), "early");
}

#[test]
fn canonical_id_distinguishes_interned_from_uninterned_duplicates() {
    crate::test_utils::init_test_tracing();
    let mut registry = SymbolRegistry::new();
    let canonical = registry.intern("dup");
    let uninterned = registry.intern_uninterned("dup");

    assert!(registry.is_canonical_id(canonical));
    assert!(!registry.is_canonical_id(uninterned));
    assert_eq!(registry.lookup("dup"), Some(canonical));
}

#[test]
fn runtime_registry_canonicalizes_ascii_multibyte_and_unibyte_names() {
    crate::test_utils::init_test_tracing();
    let mut registry = SymbolRegistry::new();
    let multibyte = crate::heap_types::LispString::from_utf8("foo");
    let unibyte = crate::heap_types::LispString::from_unibyte(b"foo".to_vec());

    let from_multibyte = registry.intern_lisp_string(&multibyte);
    let from_unibyte = registry.intern_lisp_string(&unibyte);

    assert_eq!(from_multibyte, from_unibyte);
    let resolved = registry.resolve_lisp_string(from_multibyte);
    assert_eq!(resolved.as_bytes(), b"foo");
    assert!(!resolved.is_multibyte());
}

#[test]
fn canonical_id_survives_dump_style_reconstruction() {
    crate::test_utils::init_test_tracing();
    let mut registry = SymbolRegistry::new();
    let remap = registry
        .restore_dump_symbol_table(
            &[
                unibyte_name(b"nil"),
                unibyte_name(b"t"),
                unibyte_name(b"dup"),
            ],
            &[0, 1, 2, 2],
            None,
        )
        .expect("dump symbol table should restore");

    assert!(registry.is_canonical_id(remap.symbols[2]));
    assert!(!registry.is_canonical_id(remap.symbols[3]));
    assert_eq!(registry.lookup("dup"), Some(remap.symbols[2]));
}

#[test]
fn restore_dump_slots_remaps_reordered_layout() {
    crate::test_utils::init_test_tracing();
    let mut registry = SymbolRegistry::new();
    let runtime_bar = registry.intern("bar");
    let runtime_foo = registry.intern("foo");

    let remap = registry
        .restore_dump_symbol_table(
            &[
                unibyte_name(b"nil"),
                unibyte_name(b"t"),
                unibyte_name(b"bar"),
                unibyte_name(b"foo"),
            ],
            &[0, 1, 3, 2],
            Some(&[true, true, true, true]),
        )
        .expect("dump symbol table should restore");

    assert_eq!(
        remap.symbols,
        vec![NIL_SYM_ID, T_SYM_ID, runtime_foo, runtime_bar]
    );
}

#[test]
fn restore_dump_slots_preserves_lone_uninterned_slot() {
    crate::test_utils::init_test_tracing();
    let mut registry = SymbolRegistry::new();
    let remap = registry
        .restore_dump_symbol_table(
            &[
                unibyte_name(b"nil"),
                unibyte_name(b"t"),
                unibyte_name(b"solo"),
            ],
            &[0, 1, 2],
            Some(&[true, true, false]),
        )
        .expect("dump symbol table should restore");

    assert_eq!(registry.resolve(remap.symbols[2]), "solo");
    assert!(!registry.is_canonical_id(remap.symbols[2]));
    assert_eq!(registry.lookup("solo"), None);
}

#[test]
fn dump_symbol_table_separates_name_atoms_from_symbol_slots() {
    crate::test_utils::init_test_tracing();
    let mut registry = SymbolRegistry::new();
    let canonical = registry.intern("shared-name");
    let uninterned = registry.intern_uninterned("shared-name");

    let dumped = registry.dump_symbol_table();

    let shared_name_id = registry.name_id(canonical);
    assert_eq!(registry.name_id(uninterned), shared_name_id);
    assert_eq!(
        dumped.names[shared_name_id.0 as usize],
        unibyte_name(b"shared-name")
    );
    assert_eq!(dumped.symbol_names[canonical.0 as usize], shared_name_id.0);
    assert_eq!(dumped.symbol_names[uninterned.0 as usize], shared_name_id.0);
    assert!(dumped.canonical[canonical.0 as usize]);
    assert!(!dumped.canonical[uninterned.0 as usize]);
}

#[test]
fn restore_dump_symbol_table_reuses_existing_name_atoms() {
    crate::test_utils::init_test_tracing();
    let mut registry = SymbolRegistry::new();
    let existing = registry.intern("shared-name");
    let existing_name = registry.name_id(existing);

    let remap = registry
        .restore_dump_symbol_table(
            &[
                unibyte_name(b"nil"),
                unibyte_name(b"t"),
                unibyte_name(b"shared-name"),
            ],
            &[0, 1, 2, 2],
            Some(&[true, true, true, false]),
        )
        .expect("dump symbol table should restore");

    assert_eq!(registry.name_id(remap.symbols[2]), existing_name);
    assert_eq!(registry.name_id(remap.symbols[3]), existing_name);
    assert!(registry.is_canonical_id(remap.symbols[2]));
    assert!(!registry.is_canonical_id(remap.symbols[3]));
}

#[test]
fn restore_dump_symbol_table_supports_multiple_independent_layouts() {
    crate::test_utils::init_test_tracing();
    let mut registry = SymbolRegistry::new();

    let first = registry
        .restore_dump_symbol_table(
            &[
                unibyte_name(b"nil"),
                unibyte_name(b"t"),
                unibyte_name(b"foo"),
                unibyte_name(b"bar"),
            ],
            &[0, 1, 2, 3],
            Some(&[true, true, true, true]),
        )
        .expect("first dump symbol table should restore");

    let second = registry
        .restore_dump_symbol_table(
            &[
                unibyte_name(b"nil"),
                unibyte_name(b"t"),
                unibyte_name(b"bar"),
                unibyte_name(b"foo"),
            ],
            &[0, 1, 2, 3],
            Some(&[true, true, true, true]),
        )
        .expect("second dump symbol table should restore");

    assert_eq!(registry.resolve(first.symbols[2]), "foo");
    assert_eq!(registry.resolve(first.symbols[3]), "bar");
    assert_eq!(registry.resolve(second.symbols[2]), "bar");
    assert_eq!(registry.resolve(second.symbols[3]), "foo");
    assert_eq!(first.symbols[2], second.symbols[3]);
    assert_eq!(first.symbols[3], second.symbols[2]);
}

#[test]
fn restore_dump_symbol_table_rejects_duplicate_canonical_names() {
    crate::test_utils::init_test_tracing();
    let mut registry = SymbolRegistry::new();

    let err = registry
        .restore_dump_symbol_table(
            &[
                unibyte_name(b"nil"),
                unibyte_name(b"t"),
                unibyte_name(b"dup"),
            ],
            &[0, 1, 2, 2],
            Some(&[true, true, true, true]),
        )
        .expect_err("duplicate canonical names should be rejected");

    assert!(
        err.contains("canonical symbol slots"),
        "unexpected error: {err}"
    );
    assert!(err.contains("dup"), "unexpected error: {err}");
}

#[test]
fn symbol_registry_exposes_name_ids_separately() {
    crate::test_utils::init_test_tracing();
    let mut registry = SymbolRegistry::new();
    let canonical = registry.intern("shared-name");
    let uninterned = registry.intern_uninterned("shared-name");

    let canonical_name = registry.name_id(canonical);
    let uninterned_name = registry.name_id(uninterned);

    assert_eq!(canonical_name, uninterned_name);
    assert_eq!(registry.resolve_name(canonical_name), "shared-name");
    assert_ne!(canonical, uninterned);
}
