//! Extended hash-table and obarray builtins.
//!
//! Supplements the basic hash-table operations in `builtins.rs` with:
//! - `maphash`
//! - `hash-table-test`, `hash-table-size`, `hash-table-rehash-size`,
//!   `hash-table-rehash-threshold`, `hash-table-weakness`
//! - `copy-hash-table`
//! - `mapatoms`, `unintern`

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::print::print_value;
use super::value::*;
use std::collections::{BTreeMap, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};

const SXHASH_MAX_DEPTH: usize = 3;
const SXHASH_MAX_LEN: usize = 7;
const SXHASH_FIXNUM_SHIFT: u32 = 2;
const SXHASH_FIXNUM_BITS: u32 = 62;
const SXHASH_INTMASK: u64 = (1_u64 << SXHASH_FIXNUM_BITS) - 1;
const SXHASH_FALLBACK_NONNEG_MASK: u64 = (1_u64 << (SXHASH_FIXNUM_BITS - 1)) - 1;
const LISP_TYPE_INT0: u64 = 2;
const LISP_TYPE_INT1: u64 = 6;
const KNUTH_ALPHA: u32 = 2_654_435_769;

// ---------------------------------------------------------------------------
// Argument helpers
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

fn validate_optional_obarray_arg(args: &[Value]) -> Result<(), Flow> {
    if let Some(obarray) = args.get(1) {
        if !obarray.is_nil() && !matches!(obarray, Value::Vector(_)) {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("obarrayp"), *obarray],
            ));
        }
    }
    Ok(())
}

fn is_global_obarray_proxy_in_state(
    obarray: &crate::emacs_core::symbol::Obarray,
    value: &Value,
) -> bool {
    obarray
        .symbol_value("obarray")
        .is_some_and(|proxy| *proxy == *value)
}

/// Convert a `HashKey` back into a `Value`.
fn hash_key_to_value(key: &HashKey) -> Value {
    match key {
        HashKey::Nil => Value::Nil,
        HashKey::True => Value::True,
        HashKey::Int(n) => Value::Int(*n),
        HashKey::Float(bits) => Value::Float(f64::from_bits(*bits), next_float_id()),
        HashKey::FloatEq(bits, id) => Value::Float(f64::from_bits(*bits), *id),
        HashKey::Symbol(id) => Value::Symbol(*id),
        HashKey::Keyword(id) => Value::Keyword(*id),
        HashKey::Str(id) => Value::Str(*id),
        HashKey::Char(c) => Value::Char(*c),
        HashKey::Window(id) => Value::Window(*id),
        HashKey::Frame(id) => Value::Frame(*id),
        HashKey::Ptr(_) => Value::Nil, // can't reconstruct from pointer
        HashKey::ObjId(_, _) => Value::Nil, // can't reconstruct from ObjId alone
        HashKey::EqualCons(car, cdr) => Value::cons(hash_key_to_value(car), hash_key_to_value(cdr)),
        HashKey::EqualVec(items) => {
            let vals: Vec<Value> = items.iter().map(hash_key_to_value).collect();
            Value::vector(vals)
        }
        HashKey::Cycle(index) => Value::string(format!("#{}", index)),
        HashKey::Text(text) => Value::string(text.clone()),
    }
}

pub(crate) fn hash_key_to_visible_value(table: &LispHashTable, key: &HashKey) -> Value {
    table
        .key_snapshots
        .get(key)
        .cloned()
        .unwrap_or_else(|| hash_key_to_value(key))
}

fn sxhash_combine(x: u64, y: u64) -> u64 {
    x.rotate_left(4).wrapping_add(y)
}

fn read_u16_ne(bytes: &[u8]) -> u16 {
    let mut arr = [0_u8; 2];
    arr.copy_from_slice(bytes);
    u16::from_ne_bytes(arr)
}

fn read_u32_ne(bytes: &[u8]) -> u32 {
    let mut arr = [0_u8; 4];
    arr.copy_from_slice(bytes);
    u32::from_ne_bytes(arr)
}

fn read_u64_ne(bytes: &[u8]) -> u64 {
    let mut arr = [0_u8; 8];
    arr.copy_from_slice(bytes);
    u64::from_ne_bytes(arr)
}

fn emacs_hash_char_array(bytes: &[u8]) -> u64 {
    let mut hash = bytes.len() as u64;
    let word_bytes = std::mem::size_of::<u64>();
    if bytes.len() >= word_bytes {
        let mut p = 0_usize;
        let step = word_bytes.max(bytes.len() >> 3);
        while p + word_bytes <= bytes.len() {
            let chunk = read_u64_ne(&bytes[p..p + word_bytes]);
            hash = sxhash_combine(hash, chunk);
            p = p.saturating_add(step);
        }
        let tail = read_u64_ne(&bytes[bytes.len() - word_bytes..]);
        hash = sxhash_combine(hash, tail);
    } else {
        let mut tail = 0_u64;
        let mut p = 0_usize;
        if bytes.len().saturating_sub(p) >= 4 {
            let chunk = read_u32_ne(&bytes[p..p + 4]) as u64;
            tail = (tail << 32).wrapping_add(chunk);
            p += 4;
        }
        if bytes.len().saturating_sub(p) >= 2 {
            let chunk = read_u16_ne(&bytes[p..p + 2]) as u64;
            tail = (tail << 16).wrapping_add(chunk);
            p += 2;
        }
        if p < bytes.len() {
            tail = (tail << 8).wrapping_add(bytes[p] as u64);
        }
        hash = sxhash_combine(hash, tail);
    }
    hash
}

fn fallback_sxhash_emacs_uint(value: &Value, test: HashTableTest) -> u64 {
    let mut hasher = DefaultHasher::new();
    match test {
        HashTableTest::Equal => hash_value_for_equal(value, &mut hasher, 0),
        _ => value.to_hash_key(&test).hash(&mut hasher),
    }
    // Keep fallback hashes in the non-negative fixnum lane. Exact Emacs
    // parity is only guaranteed for covered immediate/sequence fast paths.
    hasher.finish() & SXHASH_FALLBACK_NONNEG_MASK
}

fn emacs_sxhash_obj_with_fallback(value: &Value, depth: usize) -> u64 {
    emacs_sxhash_obj(value, depth)
        .unwrap_or_else(|| fallback_sxhash_emacs_uint(value, HashTableTest::Equal))
}

fn emacs_sxhash_list(value: &Value, depth: usize) -> u64 {
    let mut hash = 0_u64;
    let mut cursor = *value;
    if depth < SXHASH_MAX_DEPTH {
        for _ in 0..SXHASH_MAX_LEN {
            let Value::Cons(cell) = cursor else {
                break;
            };
            let pair = read_cons(cell);
            hash = sxhash_combine(hash, emacs_sxhash_obj_with_fallback(&pair.car, depth + 1));
            cursor = pair.cdr;
        }
    }
    if !cursor.is_nil() {
        hash = sxhash_combine(hash, emacs_sxhash_obj_with_fallback(&cursor, depth + 1));
    }
    hash
}

fn emacs_sxhash_vector(vec: &crate::gc::types::ObjId, depth: usize) -> u64 {
    let items = with_heap(|h| h.get_vector(*vec).clone());
    let mut hash = items.len() as u64;
    let count = items.len().min(SXHASH_MAX_LEN);
    for item in items.iter().take(count) {
        hash = sxhash_combine(hash, emacs_sxhash_obj_with_fallback(item, depth + 1));
    }
    hash
}

fn emacs_sxhash_obj(value: &Value, depth: usize) -> Option<u64> {
    if depth > SXHASH_MAX_DEPTH {
        return Some(0);
    }
    match value {
        Value::Int(n) => Some(*n as u64),
        Value::Char(c) => Some((*c as u32) as u64),
        Value::Float(f, _) => Some(f.to_bits()),
        Value::Str(id) => Some(with_heap(|h| {
            emacs_hash_char_array(h.get_string(*id).as_bytes())
        })),
        Value::Cons(_) => Some(emacs_sxhash_list(value, depth)),
        Value::Vector(vec) => Some(emacs_sxhash_vector(vec, depth)),
        _ => None,
    }
}

fn reduce_emacs_uint_to_fixnum(x: u64) -> i64 {
    let reduced = (x ^ (x >> SXHASH_FIXNUM_SHIFT)) & SXHASH_INTMASK;
    let sign_bit = 1_u64 << (SXHASH_FIXNUM_BITS - 1);
    if (reduced & sign_bit) != 0 {
        (reduced as i64) - ((1_i64) << SXHASH_FIXNUM_BITS)
    } else {
        reduced as i64
    }
}

fn reduce_emacs_uint_to_hash_hash(x: u64) -> u32 {
    (x ^ (x >> 32)) as u32
}

fn sxhash_eq_fixnum_uint(raw: u64) -> u64 {
    let raw = raw & SXHASH_INTMASK;
    // GNU Emacs `sxhash-eq` uses `XHASH ^ XTYPE` for fixnums/chars.
    let xtype = if (raw & 1) == 0 {
        LISP_TYPE_INT0
    } else {
        LISP_TYPE_INT1
    };
    raw ^ xtype
}

fn knuth_hash_index(hash: u32, bits: u32) -> usize {
    if bits == 0 {
        return 0;
    }
    let product = hash.wrapping_mul(KNUTH_ALPHA);
    ((product as u64) >> (32 - bits)) as usize
}

fn hash_value_for_equal(value: &Value, hasher: &mut DefaultHasher, depth: usize) {
    if depth > 4096 {
        0_u8.hash(hasher);
        return;
    }
    match value {
        Value::Nil => 0_u8.hash(hasher),
        Value::True => 1_u8.hash(hasher),
        Value::Int(n) => {
            // `equal` treats chars and ints with same codepoint as equal.
            2_u8.hash(hasher);
            n.hash(hasher);
        }
        Value::Char(c) => {
            2_u8.hash(hasher);
            (*c as i64).hash(hasher);
        }
        Value::Float(f, _) => {
            3_u8.hash(hasher);
            f.to_bits().hash(hasher);
        }
        Value::Symbol(id) => {
            4_u8.hash(hasher);
            resolve_sym(*id).hash(hasher);
        }
        Value::Keyword(id) => {
            5_u8.hash(hasher);
            resolve_sym(*id).hash(hasher);
        }
        Value::Str(id) => {
            6_u8.hash(hasher);
            with_heap(|h| h.get_string(*id).hash(hasher));
        }
        Value::Cons(cons) => {
            7_u8.hash(hasher);
            let pair = read_cons(*cons);
            hash_value_for_equal(&pair.car, hasher, depth + 1);
            hash_value_for_equal(&pair.cdr, hasher, depth + 1);
        }
        Value::Vector(vec) | Value::Record(vec) => {
            8_u8.hash(hasher);
            let items = with_heap(|h| h.get_vector(*vec).clone());
            items.len().hash(hasher);
            for item in items.iter() {
                hash_value_for_equal(item, hasher, depth + 1);
            }
        }
        Value::Window(id) => {
            9_u8.hash(hasher);
            id.hash(hasher);
        }
        Value::Frame(id) => {
            10_u8.hash(hasher);
            id.hash(hasher);
        }
        Value::Buffer(id) => {
            11_u8.hash(hasher);
            id.0.hash(hasher);
        }
        Value::Timer(id) => {
            12_u8.hash(hasher);
            id.hash(hasher);
        }
        Value::Subr(id) => {
            13_u8.hash(hasher);
            resolve_sym(*id).hash(hasher);
        }
        Value::Lambda(id) => {
            14_u8.hash(hasher);
            id.hash(hasher);
        }
        Value::Macro(id) => {
            15_u8.hash(hasher);
            id.hash(hasher);
        }
        Value::HashTable(table) => {
            16_u8.hash(hasher);
            table.index.hash(hasher);
            table.generation.hash(hasher);
        }
        Value::ByteCode(id) => {
            17_u8.hash(hasher);
            id.hash(hasher);
        }
        Value::Overlay(id) => {
            18_u8.hash(hasher);
            id.hash(hasher);
        }
    }
}

fn sxhash_emacs_uint_for(value: &Value, test: HashTableTest) -> u64 {
    match test {
        HashTableTest::Equal => emacs_sxhash_obj_with_fallback(value, 0),
        HashTableTest::Eq | HashTableTest::Eql => match value {
            Value::Int(n) => sxhash_eq_fixnum_uint(*n as u64),
            Value::Char(c) => sxhash_eq_fixnum_uint((*c as u32) as u64),
            Value::Float(f, _) if matches!(test, HashTableTest::Eql) => f.to_bits(),
            _ => fallback_sxhash_emacs_uint(value, test),
        },
    }
}

fn sxhash_for(value: &Value, test: HashTableTest) -> i64 {
    reduce_emacs_uint_to_fixnum(sxhash_emacs_uint_for(value, test))
}

fn next_pow2_saturating(value: usize) -> usize {
    value.checked_next_power_of_two().unwrap_or(usize::MAX)
}

fn internal_hash_table_index_size(table: &LispHashTable) -> usize {
    if table.size <= 0 {
        1
    } else {
        next_pow2_saturating((table.size as usize).saturating_add(1)).max(1)
    }
}

fn internal_hash_table_diagnostic_hash(key: &HashKey, test: HashTableTest) -> u32 {
    match test {
        HashTableTest::Eq => match key {
            // Eq tables carry pointer-identity keys; preserve that identity in
            // diagnostic hash output instead of collapsing through `nil`.
            HashKey::Ptr(ptr) => reduce_emacs_uint_to_hash_hash(*ptr as u64),
            HashKey::ObjId(index, generation) => {
                reduce_emacs_uint_to_hash_hash((*index as u64) ^ ((*generation as u64) << 32))
            }
            _ => {
                let value = hash_key_to_value(key);
                reduce_emacs_uint_to_hash_hash(sxhash_emacs_uint_for(&value, HashTableTest::Eq))
            }
        },
        HashTableTest::Eql => match key {
            HashKey::Ptr(ptr) => reduce_emacs_uint_to_hash_hash(*ptr as u64),
            HashKey::ObjId(index, generation) => {
                reduce_emacs_uint_to_hash_hash((*index as u64) ^ ((*generation as u64) << 32))
            }
            _ => {
                let value = hash_key_to_value(key);
                reduce_emacs_uint_to_hash_hash(sxhash_emacs_uint_for(&value, HashTableTest::Eql))
            }
        },
        HashTableTest::Equal => match key {
            HashKey::Float(bits) => reduce_emacs_uint_to_hash_hash(*bits),
            HashKey::Int(n) => reduce_emacs_uint_to_hash_hash(*n as u64),
            HashKey::Char(c) => reduce_emacs_uint_to_hash_hash((*c as u32) as u64),
            HashKey::Str(id) => {
                let hash = with_heap(|h| emacs_hash_char_array(h.get_string(*id).as_bytes()));
                reduce_emacs_uint_to_hash_hash(hash)
            }
            _ => {
                let value = hash_key_to_value(key);
                reduce_emacs_uint_to_hash_hash(sxhash_emacs_uint_for(&value, HashTableTest::Equal))
            }
        },
    }
}

fn internal_hash_table_nonempty_buckets(table: &LispHashTable) -> Vec<Vec<(Value, i64)>> {
    if table.data.is_empty() {
        return Vec::new();
    }
    let bucket_count = internal_hash_table_index_size(table).max(1);
    let index_bits = bucket_count.trailing_zeros();
    let test = table.test.clone();
    let mut buckets: Vec<Vec<(Value, i64)>> = vec![Vec::new(); bucket_count];
    for key in &table.insertion_order {
        if table.data.contains_key(key) {
            let hash = internal_hash_table_diagnostic_hash(key, test.clone());
            let index = knuth_hash_index(hash, index_bits);
            buckets[index].push((hash_key_to_visible_value(table, key), hash as i64));
        }
    }
    for bucket in &mut buckets {
        bucket.sort_by_key(|(key, hash)| (print_value(key), *hash));
    }
    buckets
        .into_iter()
        .filter(|bucket| !bucket.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// Pure builtins
// ---------------------------------------------------------------------------

/// (hash-table-test TABLE) -> symbol
pub(crate) fn builtin_hash_table_test(args: Vec<Value>) -> EvalResult {
    expect_args("hash-table-test", &args, 1)?;
    match &args[0] {
        Value::HashTable(ht) => {
            let table = with_heap(|h| h.get_hash_table(*ht).clone());
            if let Some(id) = table.test_name {
                Ok(Value::Symbol(id))
            } else {
                let sym = match table.test {
                    HashTableTest::Eq => "eq",
                    HashTableTest::Eql => "eql",
                    HashTableTest::Equal => "equal",
                };
                Ok(Value::symbol(sym))
            }
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), *other],
        )),
    }
}

/// (hash-table-size TABLE) -> integer
pub(crate) fn builtin_hash_table_size(args: Vec<Value>) -> EvalResult {
    expect_args("hash-table-size", &args, 1)?;
    match &args[0] {
        Value::HashTable(ht) => {
            let table = with_heap(|h| h.get_hash_table(*ht).clone());
            Ok(Value::Int(table.size))
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), *other],
        )),
    }
}

/// (hash-table-rehash-size TABLE) -> float
pub(crate) fn builtin_hash_table_rehash_size(args: Vec<Value>) -> EvalResult {
    expect_args("hash-table-rehash-size", &args, 1)?;
    match &args[0] {
        Value::HashTable(ht) => {
            let table = with_heap(|h| h.get_hash_table(*ht).clone());
            Ok(Value::Float(table.rehash_size, next_float_id()))
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), *other],
        )),
    }
}

/// (hash-table-rehash-threshold TABLE) -> float
pub(crate) fn builtin_hash_table_rehash_threshold(args: Vec<Value>) -> EvalResult {
    expect_args("hash-table-rehash-threshold", &args, 1)?;
    match &args[0] {
        Value::HashTable(ht) => {
            let table = with_heap(|h| h.get_hash_table(*ht).clone());
            Ok(Value::Float(table.rehash_threshold, next_float_id()))
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), *other],
        )),
    }
}

/// (hash-table-weakness TABLE) -> nil | symbol
pub(crate) fn builtin_hash_table_weakness(args: Vec<Value>) -> EvalResult {
    expect_args("hash-table-weakness", &args, 1)?;
    match &args[0] {
        Value::HashTable(ht) => {
            let table = with_heap(|h| h.get_hash_table(*ht).clone());
            Ok(match table.weakness {
                None => Value::Nil,
                Some(HashTableWeakness::Key) => Value::symbol("key"),
                Some(HashTableWeakness::Value) => Value::symbol("value"),
                Some(HashTableWeakness::KeyOrValue) => Value::symbol("key-or-value"),
                Some(HashTableWeakness::KeyAndValue) => Value::symbol("key-and-value"),
            })
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), *other],
        )),
    }
}

/// (copy-hash-table TABLE) -> new hash table with same entries
pub(crate) fn builtin_copy_hash_table(args: Vec<Value>) -> EvalResult {
    expect_args("copy-hash-table", &args, 1)?;
    match &args[0] {
        Value::HashTable(ht) => {
            let new_table = with_heap(|h| h.get_hash_table(*ht).clone());
            let id = with_heap_mut(|h| h.alloc_hash_table_raw(new_table));
            Ok(Value::HashTable(id))
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), *other],
        )),
    }
}

/// (hash-table-keys TABLE) -> list of keys
#[cfg(test)]
pub(crate) fn builtin_hash_table_keys(args: Vec<Value>) -> EvalResult {
    expect_args("hash-table-keys", &args, 1)?;
    match &args[0] {
        Value::HashTable(ht) => {
            let table = with_heap(|h| h.get_hash_table(*ht).clone());
            let keys: Vec<Value> = table
                .insertion_order
                .iter()
                .filter(|k| table.data.contains_key(k))
                .map(|key| hash_key_to_visible_value(&table, key))
                .collect();
            Ok(Value::list(keys))
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), *other],
        )),
    }
}

/// (hash-table-values TABLE) -> list of values
#[cfg(test)]
pub(crate) fn builtin_hash_table_values(args: Vec<Value>) -> EvalResult {
    expect_args("hash-table-values", &args, 1)?;
    match &args[0] {
        Value::HashTable(ht) => {
            let table = with_heap(|h| h.get_hash_table(*ht).clone());
            let values: Vec<Value> = table
                .insertion_order
                .iter()
                .filter_map(|k| table.data.get(k).cloned())
                .collect();
            Ok(Value::list(values))
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), *other],
        )),
    }
}

/// `(sxhash-eq OBJECT)` -- hash OBJECT according to `eq` semantics.
pub(crate) fn builtin_sxhash_eq(args: Vec<Value>) -> EvalResult {
    expect_args("sxhash-eq", &args, 1)?;
    Ok(Value::Int(sxhash_for(&args[0], HashTableTest::Eq)))
}

/// `(sxhash-eql OBJECT)` -- hash OBJECT according to `eql` semantics.
pub(crate) fn builtin_sxhash_eql(args: Vec<Value>) -> EvalResult {
    expect_args("sxhash-eql", &args, 1)?;
    Ok(Value::Int(sxhash_for(&args[0], HashTableTest::Eql)))
}

/// `(sxhash-equal OBJECT)` -- hash OBJECT according to `equal` semantics.
pub(crate) fn builtin_sxhash_equal(args: Vec<Value>) -> EvalResult {
    expect_args("sxhash-equal", &args, 1)?;
    Ok(Value::Int(sxhash_for(&args[0], HashTableTest::Equal)))
}

/// `(sxhash-equal-including-properties OBJECT)` -- hash OBJECT like
/// `equal-including-properties`. NeoVM currently has no text properties, so
/// this matches `sxhash-equal`.
pub(crate) fn builtin_sxhash_equal_including_properties(args: Vec<Value>) -> EvalResult {
    expect_args("sxhash-equal-including-properties", &args, 1)?;
    Ok(Value::Int(sxhash_for(&args[0], HashTableTest::Equal)))
}

/// `(internal--hash-table-index-size TABLE)` -- report hash index width.
pub(crate) fn builtin_internal_hash_table_index_size(args: Vec<Value>) -> EvalResult {
    expect_args("internal--hash-table-index-size", &args, 1)?;
    match &args[0] {
        Value::HashTable(ht) => {
            let table = with_heap(|h| h.get_hash_table(*ht).clone());
            Ok(Value::Int(
                internal_hash_table_index_size(&table).min(i64::MAX as usize) as i64,
            ))
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), *other],
        )),
    }
}

/// `(internal--hash-table-buckets TABLE)` -- return non-empty bucket alists.
pub(crate) fn builtin_internal_hash_table_buckets(args: Vec<Value>) -> EvalResult {
    expect_args("internal--hash-table-buckets", &args, 1)?;
    match &args[0] {
        Value::HashTable(ht) => {
            let table = with_heap(|h| h.get_hash_table(*ht).clone());
            let buckets = internal_hash_table_nonempty_buckets(&table);
            if buckets.is_empty() {
                return Ok(Value::Nil);
            }
            let rendered = buckets
                .into_iter()
                .map(|bucket| {
                    let alist_items: Vec<Value> = bucket
                        .into_iter()
                        .map(|(key, hash)| Value::cons(key, Value::Int(hash)))
                        .collect();
                    Value::list(alist_items)
                })
                .collect();
            Ok(Value::list(rendered))
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), *other],
        )),
    }
}

/// `(internal--hash-table-histogram TABLE)` -- return (bucket-size . count)
/// alist for non-empty buckets.
pub(crate) fn builtin_internal_hash_table_histogram(args: Vec<Value>) -> EvalResult {
    expect_args("internal--hash-table-histogram", &args, 1)?;
    match &args[0] {
        Value::HashTable(ht) => {
            let table = with_heap(|h| h.get_hash_table(*ht).clone());
            let buckets = internal_hash_table_nonempty_buckets(&table);
            if buckets.is_empty() {
                return Ok(Value::Nil);
            }
            let mut histogram: BTreeMap<i64, i64> = BTreeMap::new();
            for bucket in buckets {
                let size = bucket.len() as i64;
                *histogram.entry(size).or_insert(0) += 1;
            }
            let entries: Vec<Value> = histogram
                .into_iter()
                .map(|(size, count)| Value::cons(Value::Int(size), Value::Int(count)))
                .collect();
            Ok(Value::list(entries))
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), *other],
        )),
    }
}

// ---------------------------------------------------------------------------
// Eval-dependent builtins
// ---------------------------------------------------------------------------

/// (maphash FUNCTION TABLE) — call FUNCTION with each (KEY VALUE) pair.
pub(crate) fn builtin_maphash(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let (func, entries) = collect_maphash_entries(args)?;
    for (key, val) in entries {
        eval.apply(func, vec![key, val])?;
    }
    Ok(Value::Nil)
}

/// (mapatoms FUNCTION &optional OBARRAY) — call FUNCTION with each interned symbol.
pub(crate) fn builtin_mapatoms(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let (func, symbols) = collect_mapatoms_symbols(eval.obarray(), args)?;
    for sym in symbols {
        eval.apply(func, vec![sym])?;
    }
    Ok(Value::Nil)
}

pub(crate) fn collect_maphash_entries(
    args: Vec<Value>,
) -> Result<(Value, Vec<(Value, Value)>), Flow> {
    expect_args("maphash", &args, 2)?;
    let func = args[0];
    let entries = match &args[1] {
        Value::HashTable(ht) => {
            let table = with_heap(|h| h.get_hash_table(*ht).clone());
            table
                .insertion_order
                .iter()
                .filter_map(|k| {
                    table
                        .data
                        .get(k)
                        .map(|v| (hash_key_to_visible_value(&table, k), *v))
                })
                .collect()
        }
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("hash-table-p"), *other],
            ));
        }
    };
    Ok((func, entries))
}

pub(crate) fn collect_mapatoms_symbols(
    obarray: &crate::emacs_core::symbol::Obarray,
    args: Vec<Value>,
) -> Result<(Value, Vec<Value>), Flow> {
    expect_min_args("mapatoms", &args, 1)?;
    expect_max_args("mapatoms", &args, 2)?;
    validate_optional_obarray_arg(&args)?;
    let func = args[0];

    if let Some(Value::Vector(vec_id)) = args
        .get(1)
        .filter(|v| !v.is_nil() && !is_global_obarray_proxy_in_state(obarray, v))
    {
        let all_slots = with_heap(|h| h.get_vector(*vec_id).clone());
        let mut symbols = Vec::new();
        for slot in &all_slots {
            let mut current = *slot;
            loop {
                match current {
                    Value::Nil => break,
                    Value::Cons(id) => {
                        let (car, cdr) = with_heap(|h| (h.cons_car(id), h.cons_cdr(id)));
                        symbols.push(car);
                        current = cdr;
                    }
                    _ => break,
                }
            }
        }
        return Ok((func, symbols));
    }

    let symbols = obarray
        .all_symbols()
        .iter()
        .map(|s| Value::symbol(s.to_string()))
        .collect();
    Ok((func, symbols))
}

/// (unintern NAME &optional OBARRAY) — remove symbol from obarray.
pub(crate) fn builtin_unintern(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("unintern", &args, 1)?;
    expect_max_args("unintern", &args, 2)?;
    validate_optional_obarray_arg(&args)?;
    let name = match &args[0] {
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };

    // Custom obarray path
    if let Some(Value::Vector(vec_id)) = args.get(1).filter(|v| !v.is_nil()) {
        let vec_id = *vec_id;
        let vec_len = with_heap(|h| h.get_vector(vec_id).len());
        if vec_len == 0 {
            return Ok(Value::Nil);
        }
        let bucket_idx =
            name.bytes()
                .fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64)) as usize
                % vec_len;
        let bucket = with_heap(|h| h.get_vector(vec_id)[bucket_idx]);

        // Walk the bucket chain and rebuild without the matching symbol
        let mut items = Vec::new();
        let mut found = false;
        let mut current = bucket;
        loop {
            match current {
                Value::Nil => break,
                Value::Cons(id) => {
                    let (car, cdr) = with_heap(|h| (h.cons_car(id), h.cons_cdr(id)));
                    if !found {
                        if let Some(sym_name) = car.as_symbol_name() {
                            if sym_name == name {
                                found = true;
                                current = cdr;
                                continue;
                            }
                        }
                    }
                    items.push(car);
                    current = cdr;
                }
                _ => break,
            }
        }

        if found {
            // Rebuild the bucket chain
            let new_bucket = items
                .into_iter()
                .rev()
                .fold(Value::Nil, |acc, sym| Value::cons(sym, acc));
            with_heap_mut(|h| {
                h.get_vector_mut(vec_id)[bucket_idx] = new_bucket;
            });
            return Ok(Value::True);
        }
        return Ok(Value::Nil);
    }

    // Global obarray path
    let removed = eval.obarray.unintern(&name);
    Ok(if removed { Value::True } else { Value::Nil })
}
#[cfg(test)]
#[path = "hashtab_test.rs"]
mod tests;
