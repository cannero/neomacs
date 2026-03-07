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
    let car = Value::Int(1);
    let cdr = Value::Int(2);
    let obj = HeapObject::Cons { car, cdr };
    let traced = obj.trace_values();
    assert_eq!(traced.len(), 2);
    assert_eq!(traced[0], Value::Int(1));
    assert_eq!(traced[1], Value::Int(2));
}

#[test]
fn trace_values_vector() {
    let items = vec![Value::Int(10), Value::Int(20), Value::Int(30)];
    let obj = HeapObject::Vector(items);
    let traced = obj.trace_values();
    assert_eq!(traced.len(), 3);
    assert_eq!(traced[0], Value::Int(10));
    assert_eq!(traced[1], Value::Int(20));
    assert_eq!(traced[2], Value::Int(30));
}

#[test]
fn trace_values_hash_table() {
    use crate::emacs_core::value::HashTableTest;
    let mut ht = LispHashTable::new(HashTableTest::Equal);
    // Insert a key/value pair via the data map directly
    use crate::emacs_core::value::HashKey;
    let key = HashKey::Int(1);
    ht.data.insert(key.clone(), Value::Int(42));
    ht.insertion_order.push(key);
    let obj = HeapObject::HashTable(ht);
    let traced = obj.trace_values();
    // At minimum the data value should be traced
    assert!(traced.contains(&Value::Int(42)));
}

#[test]
fn trace_values_str_empty() {
    let obj = HeapObject::Str(LispString {
        text: "hello".to_string(),
        multibyte: false,
    });
    let traced = obj.trace_values();
    assert!(traced.is_empty());
}

#[test]
fn trace_values_free_empty() {
    let obj = HeapObject::Free;
    let traced = obj.trace_values();
    assert!(traced.is_empty());
}
