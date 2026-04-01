use super::*;

#[test]
fn objid_copy_eq_hash() {
    let a = ObjId {
        index: 1,
        generation: 0,
    };
    let b = a; // Copy
    assert_eq!(a, b);

    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(a);
    assert!(set.contains(&b));
}

#[test]
fn trace_values_cons() {
    let car = Value::fixnum(1);
    let cdr = Value::fixnum(2);
    let obj = HeapObject::Cons { car, cdr };
    let traced = obj.trace_values();
    assert_eq!(traced.len(), 2);
    assert_val_eq!(traced[0], Value::fixnum(1));
    assert_val_eq!(traced[1], Value::fixnum(2));
}

#[test]
fn trace_values_vector() {
    let items = vec![Value::fixnum(10), Value::fixnum(20), Value::fixnum(30)];
    let obj = HeapObject::Vector(items);
    let traced = obj.trace_values();
    assert_eq!(traced.len(), 3);
    assert_val_eq!(traced[0], Value::fixnum(10));
    assert_val_eq!(traced[1], Value::fixnum(20));
    assert_val_eq!(traced[2], Value::fixnum(30));
}

#[test]
fn trace_values_hash_table() {
    use crate::emacs_core::value::HashTableTest;
    let mut ht = LispHashTable::new(HashTableTest::Equal);
    // Insert a key/value pair via the data map directly
    use crate::emacs_core::value::HashKey;
    let key = HashKey::Int(1);
    ht.data.insert(key.clone(), Value::fixnum(42));
    ht.insertion_order.push(key);
    let obj = HeapObject::HashTable(ht);
    let traced = obj.trace_values();
    // At minimum the data value should be traced
    assert!(traced.contains(&Value::fixnum(42)));
}

#[test]
fn trace_values_str_empty() {
    let obj = HeapObject::Str(LispString::new("hello".to_string(), false));
    let traced = obj.trace_values();
    assert!(traced.is_empty());
}

#[test]
fn trace_values_free_empty() {
    let obj = HeapObject::Free;
    let traced = obj.trace_values();
    assert!(traced.is_empty());
}

#[test]
fn slice_make_mut_detaches_from_original() {
    let original = LispString::new("hello world".to_string(), false);
    let mut slice = original.slice(6, 11).expect("valid slice");

    slice.make_mut().push('!');

    assert_eq!(original.as_str(), "hello world");
    assert_eq!(slice.as_str(), "world!");
}

#[test]
fn concat_string_slices_across_segment_boundaries() {
    let left = LispString::new("hello".to_string(), false);
    let right = LispString::new("world".to_string(), false);
    let mut parts = Vec::new();
    left.append_parts_to(&mut parts);
    right.append_parts_to(&mut parts);
    let combined = LispString::from_parts(parts, false);

    assert_eq!(combined.as_str(), "helloworld");
    assert_eq!(
        combined.slice(3, 8).expect("cross-segment slice").as_str(),
        "lowor"
    );
}
