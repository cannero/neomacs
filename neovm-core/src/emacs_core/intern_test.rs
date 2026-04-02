use super::*;

#[test]
fn intern_dedup() {
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
    // Uses the process-wide runtime interner.
    let a = intern("hello");
    let b = intern("hello");
    let c = intern("world");
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_eq!(resolve_sym(a), "hello");
    assert_eq!(resolve_sym(c), "world");
}

#[test]
fn intern_empty_string() {
    crate::test_utils::init_test_tracing();
    let mut interner = StringInterner::new();
    let id = interner.intern("");
    assert_eq!(interner.resolve(id), "");
    // Interning again returns the same id
    assert_eq!(interner.intern(""), id);
}

#[test]
fn intern_many_strings() {
    crate::test_utils::init_test_tracing();
    let mut interner = StringInterner::new();
    let ids: Vec<SymId> = (0..1000)
        .map(|i| interner.intern(&format!("sym-{i}")))
        .collect();
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(interner.resolve(*id), format!("sym-{i}"));
    }
    // All ids are unique
    let unique: std::collections::HashSet<SymId> = ids.iter().copied().collect();
    assert_eq!(unique.len(), 1000);
}

#[test]
fn intern_idempotent() {
    crate::test_utils::init_test_tracing();
    let mut interner = StringInterner::new();
    let first = interner.intern("repeated");
    for _ in 0..100 {
        assert_eq!(interner.intern("repeated"), first);
    }
}

#[test]
fn symid_copy_eq_hash() {
    crate::test_utils::init_test_tracing();
    let mut interner = StringInterner::new();
    let a = interner.intern("x");
    let b = a; // Copy
    assert_eq!(a, b);

    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(a);
    assert!(set.contains(&b));
}

#[test]
fn resolve_sym_stable_across_growth() {
    crate::test_utils::init_test_tracing();
    let mut interner = StringInterner::new();
    let early = interner.intern("early");
    assert_eq!(interner.resolve(early), "early");
    // Intern many more strings to force internal Vec growth
    for i in 0..500 {
        interner.intern(&format!("growth-{i}"));
    }
    // Early id still resolves correctly
    assert_eq!(interner.resolve(early), "early");
}
