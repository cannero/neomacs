//! Extended hash-table and obarray builtins.
//!
//! Supplements the basic hash-table operations in `builtins.rs` with:
//! - `maphash`
//! - `hash-table-test`, `hash-table-size`, `hash-table-rehash-size`,
//!   `hash-table-rehash-threshold`, `hash-table-weakness`
//! - `copy-hash-table`
//! - `mapatoms`, `unintern`

use super::error::{signal, EvalResult, Flow};
use super::intern::resolve_sym;
use super::print::print_value;
use super::value::*;
use std::collections::{hash_map::DefaultHasher, BTreeMap};
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
        HashKey::EqualCons(car, cdr) => {
            Value::cons(hash_key_to_value(car), hash_key_to_value(cdr))
        }
        HashKey::EqualVec(items) => {
            let vals: Vec<Value> = items.iter().map(hash_key_to_value).collect();
            Value::vector(vals)
        }
    }
}

fn hash_key_to_visible_value(table: &LispHashTable, key: &HashKey) -> Value {
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
        Value::Str(id) => Some(with_heap(|h| emacs_hash_char_array(h.get_string(*id).as_bytes()))),
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
    for key in table.data.keys() {
        let hash = internal_hash_table_diagnostic_hash(key, test.clone());
        let index = knuth_hash_index(hash, index_bits);
        buckets[index].push((hash_key_to_visible_value(table, key), hash as i64));
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
                .data
                .keys()
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
            let values: Vec<Value> = table.data.values().cloned().collect();
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
pub(crate) fn builtin_maphash(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_args("maphash", &args, 2)?;
    let func = args[0];
    let entries: Vec<(Value, Value)> = match &args[1] {
        Value::HashTable(ht) => {
            let table = with_heap(|h| h.get_hash_table(*ht).clone());
            table
                .data
                .iter()
                .map(|(k, v)| (hash_key_to_visible_value(&table, k), *v))
                .collect()
        }
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("hash-table-p"), *other],
            ));
        }
    };
    for (key, val) in entries {
        eval.apply(func, vec![key, val])?;
    }
    Ok(Value::Nil)
}

/// (mapatoms FUNCTION &optional OBARRAY) — call FUNCTION with each interned symbol.
pub(crate) fn builtin_mapatoms(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("mapatoms", &args, 1)?;
    expect_max_args("mapatoms", &args, 2)?;
    validate_optional_obarray_arg(&args)?;
    let func = args[0];
    // Collect symbol names to avoid borrowing obarray during eval
    let symbols: Vec<String> = eval
        .obarray
        .all_symbols()
        .iter()
        .map(|s| s.to_string())
        .collect();
    for sym in symbols {
        eval.apply(func, vec![Value::symbol(sym)])?;
    }
    Ok(Value::Nil)
}

/// (unintern NAME &optional OBARRAY) — remove symbol from obarray.
pub(crate) fn builtin_unintern(eval: &mut super::eval::Evaluator, args: Vec<Value>) -> EvalResult {
    expect_min_args("unintern", &args, 1)?;
    expect_max_args("unintern", &args, 2)?;
    validate_optional_obarray_arg(&args)?;
    let name = match &args[0] {
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        Value::Str(id) => with_heap(|h| h.get_string(*id).clone()),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ))
        }
    };
    let removed = eval.obarray.unintern(&name);
    Ok(if removed { Value::True } else { Value::Nil })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emacs_core::builtins::{
        builtin_gethash, builtin_hash_table_count, builtin_make_hash_table, builtin_puthash,
    };

    #[test]
    fn hash_table_keys_values_basics() {
        let table = Value::hash_table(HashTableTest::Equal);
        if let Value::HashTable(ht) = &table {
            with_heap_mut(|h| {
                let raw = h.get_hash_table_mut(*ht);
                let test = raw.test.clone();
                raw.data
                    .insert(Value::symbol("alpha").to_hash_key(&test), Value::Int(1));
                raw.data
                    .insert(Value::symbol("beta").to_hash_key(&test), Value::Int(2));
            });
        } else {
            panic!("expected hash table");
        }

        let keys = builtin_hash_table_keys(vec![table]).unwrap();
        let keys = list_to_vec(&keys).expect("proper list");
        assert_eq!(keys.len(), 2);
        assert!(keys.iter().any(|v| v.as_symbol_name() == Some("alpha")));
        assert!(keys.iter().any(|v| v.as_symbol_name() == Some("beta")));

        let values = builtin_hash_table_values(vec![table]).unwrap();
        let values = list_to_vec(&values).expect("proper list");
        assert_eq!(values.len(), 2);
        assert!(values.iter().any(|v| v.as_int() == Some(1)));
        assert!(values.iter().any(|v| v.as_int() == Some(2)));
    }

    #[test]
    fn hash_table_keys_values_errors() {
        assert!(builtin_hash_table_keys(vec![]).is_err());
        assert!(builtin_hash_table_values(vec![]).is_err());
        assert!(builtin_hash_table_keys(vec![Value::Nil]).is_err());
        assert!(builtin_hash_table_values(vec![Value::Nil]).is_err());
    }

    #[test]
    fn hash_table_rehash_defaults() {
        let table = builtin_make_hash_table(vec![]).unwrap();
        let size = builtin_hash_table_rehash_size(vec![table]).unwrap();
        let threshold = builtin_hash_table_rehash_threshold(vec![table]).unwrap();

        assert_eq!(size, Value::Float(1.5, next_float_id()));
        assert_eq!(threshold, Value::Float(0.8125, next_float_id()));
    }

    #[test]
    fn hash_table_rehash_options_are_ignored() {
        let table = builtin_make_hash_table(vec![
            Value::keyword(":rehash-size"),
            Value::Float(2.0, next_float_id()),
            Value::keyword(":rehash-threshold"),
            Value::Float(0.9, next_float_id()),
        ])
        .unwrap();

        let size = builtin_hash_table_rehash_size(vec![table]).unwrap();
        let threshold = builtin_hash_table_rehash_threshold(vec![table]).unwrap();

        assert_eq!(size, Value::Float(1.5, next_float_id()));
        assert_eq!(threshold, Value::Float(0.8125, next_float_id()));

        assert!(builtin_make_hash_table(vec![
            Value::keyword(":rehash-size"),
            Value::string("x"),
            Value::keyword(":rehash-threshold"),
            Value::Float(1.5, next_float_id()),
        ])
        .is_ok());
        assert!(builtin_make_hash_table(vec![
            Value::keyword(":rehash-threshold"),
            Value::string("x"),
            Value::keyword(":rehash-size"),
            Value::Float(1.5, next_float_id()),
        ])
        .is_ok());
    }

    #[test]
    fn sxhash_variants_return_fixnums_and_preserve_hash_contracts() {
        assert!(matches!(
            builtin_sxhash_eq(vec![Value::symbol("foo")]),
            Ok(Value::Int(_))
        ));
        assert!(matches!(
            builtin_sxhash_eql(vec![Value::symbol("foo")]),
            Ok(Value::Int(_))
        ));
        assert!(matches!(
            builtin_sxhash_equal(vec![Value::symbol("foo")]),
            Ok(Value::Int(_))
        ));
        assert!(matches!(
            builtin_sxhash_equal_including_properties(vec![Value::symbol("foo")]),
            Ok(Value::Int(_))
        ));

        let left = Value::string("x");
        let right = Value::string("x");
        assert_eq!(
            builtin_sxhash_equal(vec![left]).unwrap(),
            builtin_sxhash_equal(vec![right]).unwrap()
        );
        assert_eq!(
            builtin_sxhash_equal_including_properties(vec![left]).unwrap(),
            builtin_sxhash_equal_including_properties(vec![right]).unwrap()
        );
        assert_eq!(
            builtin_sxhash_equal(vec![Value::list(vec![Value::Int(1), Value::Int(2)])]).unwrap(),
            builtin_sxhash_equal(vec![Value::list(vec![Value::Int(1), Value::Int(2)])]).unwrap()
        );
    }

    #[test]
    fn sxhash_equal_matches_oracle_for_small_int_and_string_values() {
        assert_eq!(
            builtin_sxhash_equal(vec![Value::string("a")]).unwrap(),
            Value::Int(109)
        );
        assert_eq!(
            builtin_sxhash_equal(vec![Value::string("b")]).unwrap(),
            Value::Int(110)
        );
        assert_eq!(
            builtin_sxhash_equal(vec![Value::string("ab")]).unwrap(),
            Value::Int(31265)
        );
        assert_eq!(
            builtin_sxhash_equal(vec![Value::Int(1)]).unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            builtin_sxhash_equal(vec![Value::Int(2)]).unwrap(),
            Value::Int(2)
        );
    }

    #[test]
    fn sxhash_eq_eql_fixnum_and_char_match_oracle_values() {
        assert_eq!(
            builtin_sxhash_eq(vec![Value::Int(1)]).unwrap(),
            Value::Int(6)
        );
        assert_eq!(
            builtin_sxhash_eq(vec![Value::Int(2)]).unwrap(),
            Value::Int(0)
        );
        assert_eq!(
            builtin_sxhash_eq(vec![Value::Int(3)]).unwrap(),
            Value::Int(4)
        );
        assert_eq!(
            builtin_sxhash_eq(vec![Value::Int(65)]).unwrap(),
            Value::Int(86)
        );
        assert_eq!(
            builtin_sxhash_eq(vec![Value::Int(97)]).unwrap(),
            Value::Int(126)
        );
        assert_eq!(
            builtin_sxhash_eq(vec![Value::Int(-1)]).unwrap(),
            Value::Int(-1_152_921_504_606_846_969)
        );
        assert_eq!(
            builtin_sxhash_eq(vec![Value::Int(-2)]).unwrap(),
            Value::Int(-1_152_921_504_606_846_973)
        );

        assert_eq!(
            builtin_sxhash_eql(vec![Value::Int(65)]).unwrap(),
            Value::Int(86)
        );
        assert_eq!(
            builtin_sxhash_eql(vec![Value::Char('A')]).unwrap(),
            Value::Int(86)
        );
        assert_eq!(
            builtin_sxhash_equal(vec![Value::Int(65)]).unwrap(),
            Value::Int(81)
        );
    }

    #[test]
    fn sxhash_float_matches_oracle_fixnum_values() {
        assert_eq!(
            builtin_sxhash_eql(vec![Value::Float(1.0, next_float_id())]).unwrap(),
            Value::Int(-1_149_543_804_886_319_104)
        );
        assert_eq!(
            builtin_sxhash_eql(vec![Value::Float(2.0, next_float_id())]).unwrap(),
            Value::Int(1_152_921_504_606_846_976)
        );
        assert_eq!(
            builtin_sxhash_equal(vec![Value::Float(1.0, next_float_id())]).unwrap(),
            Value::Int(-1_149_543_804_886_319_104)
        );
        assert_eq!(
            builtin_sxhash_equal(vec![Value::Float(2.0, next_float_id())]).unwrap(),
            Value::Int(1_152_921_504_606_846_976)
        );
    }

    #[test]
    fn sxhash_float_signed_zero_and_nan_semantics_match_oracle() {
        assert_eq!(
            builtin_sxhash_eql(vec![Value::Float(0.0, next_float_id())]).unwrap(),
            Value::Int(0)
        );
        assert_eq!(
            builtin_sxhash_eql(vec![Value::Float(-0.0, next_float_id())]).unwrap(),
            Value::Int(-2_305_843_009_213_693_952)
        );
        assert_eq!(
            builtin_sxhash_equal(vec![Value::Float(0.0, next_float_id())]).unwrap(),
            Value::Int(0)
        );
        assert_eq!(
            builtin_sxhash_equal(vec![Value::Float(-0.0, next_float_id())]).unwrap(),
            Value::Int(-2_305_843_009_213_693_952)
        );

        let nan = Value::Float(0.0_f64 / 0.0_f64, next_float_id());
        let nan_eql = builtin_sxhash_eql(vec![nan]).unwrap();
        let nan_equal = builtin_sxhash_equal(vec![nan]).unwrap();
        assert_eq!(nan_eql, nan_equal);

        for test_name in ["eql", "equal"] {
            let table =
                builtin_make_hash_table(vec![Value::keyword(":test"), Value::symbol(test_name)])
                    .expect("hash table");
            let _ = builtin_puthash(vec![
                Value::Float(0.0, next_float_id()),
                Value::symbol("zero"),
                table,
            ])
            .expect("puthash zero");
            assert_eq!(
                builtin_gethash(vec![
                    Value::Float(-0.0, next_float_id()),
                    table,
                    Value::symbol("miss")
                ])
                .expect("gethash -0.0"),
                Value::symbol("miss")
            );

            let _ = builtin_puthash(vec![nan, Value::symbol("nan"), table])
                .expect("puthash nan");
            assert_eq!(
                builtin_gethash(vec![nan, table, Value::symbol("miss")])
                    .expect("gethash nan"),
                Value::symbol("nan")
            );
        }
    }

    #[test]
    fn hash_table_nan_payloads_remain_distinct_for_eql_and_equal() {
        let nan_a = Value::Float(f64::from_bits(0x7ff8_0000_0000_0000), next_float_id());
        let nan_b = Value::Float(f64::from_bits(0x7ff8_0000_0000_0001), next_float_id());
        assert_eq!(
            builtin_sxhash_eql(vec![nan_a]).unwrap(),
            builtin_sxhash_equal(vec![nan_a]).unwrap()
        );
        assert_eq!(
            builtin_sxhash_eql(vec![nan_b]).unwrap(),
            builtin_sxhash_equal(vec![nan_b]).unwrap()
        );
        assert_ne!(
            builtin_sxhash_eql(vec![nan_a]).unwrap(),
            builtin_sxhash_eql(vec![nan_b]).unwrap()
        );

        for test_name in ["eql", "equal"] {
            let table = builtin_make_hash_table(vec![
                Value::keyword(":test"),
                Value::symbol(test_name),
                Value::keyword(":size"),
                Value::Int(5),
            ])
            .expect("hash table");
            let _ = builtin_puthash(vec![nan_a, Value::symbol("a"), table])
                .expect("puthash nan-a");
            let _ = builtin_puthash(vec![nan_b, Value::symbol("b"), table])
                .expect("puthash nan-b");
            assert_eq!(
                builtin_hash_table_count(vec![table]).expect("hash-table-count"),
                Value::Int(2)
            );
            assert_eq!(
                builtin_gethash(vec![nan_a, table, Value::symbol("miss")])
                    .expect("gethash nan-a"),
                Value::symbol("a")
            );
            assert_eq!(
                builtin_gethash(vec![nan_b, table, Value::symbol("miss")])
                    .expect("gethash nan-b"),
                Value::symbol("b")
            );

            let buckets =
                builtin_internal_hash_table_buckets(vec![table]).expect("bucket diagnostics");
            let outer = list_to_vec(&buckets).expect("outer list");
            let mut hashes = Vec::new();
            for bucket in outer {
                let entries = list_to_vec(&bucket).expect("bucket alist");
                for entry in entries {
                    let Value::Cons(cell) = entry else {
                        panic!("expected alist cons entry");
                    };
                    let pair = read_cons(cell);
                    hashes.push(pair.cdr.as_int().expect("diagnostic hash integer"));
                }
            }
            hashes.sort_unstable();
            assert_eq!(hashes.len(), 2);
            assert_ne!(hashes[0], hashes[1]);
        }
    }

    #[test]
    fn internal_hash_table_introspection_empty_defaults() {
        let table = builtin_make_hash_table(vec![]).unwrap();
        assert_eq!(
            builtin_internal_hash_table_buckets(vec![table]).unwrap(),
            Value::Nil
        );
        assert_eq!(
            builtin_internal_hash_table_histogram(vec![table]).unwrap(),
            Value::Nil
        );
        assert_eq!(
            builtin_internal_hash_table_index_size(vec![table]).unwrap(),
            Value::Int(1)
        );
    }

    #[test]
    fn internal_hash_table_index_size_uses_declared_size() {
        let table_one = builtin_make_hash_table(vec![Value::keyword(":size"), Value::Int(1)])
            .expect("size 1 table");
        assert_eq!(
            builtin_internal_hash_table_index_size(vec![table_one]).unwrap(),
            Value::Int(2)
        );

        let table_mid = builtin_make_hash_table(vec![Value::keyword(":size"), Value::Int(37)])
            .expect("size 37 table");
        assert_eq!(
            builtin_internal_hash_table_index_size(vec![table_mid]).unwrap(),
            Value::Int(64)
        );
    }

    #[test]
    fn internal_hash_table_index_size_tracks_growth_boundaries() {
        let tiny = builtin_make_hash_table(vec![Value::keyword(":size"), Value::Int(1)])
            .expect("size 1 table");
        let _ = builtin_puthash(vec![Value::Int(1), Value::symbol("x"), tiny])
            .expect("puthash for first tiny entry");
        assert_eq!(
            builtin_internal_hash_table_index_size(vec![tiny]).unwrap(),
            Value::Int(2)
        );
        let _ = builtin_puthash(vec![Value::Int(2), Value::symbol("y"), tiny])
            .expect("puthash for second tiny entry");
        assert_eq!(
            builtin_internal_hash_table_index_size(vec![tiny]).unwrap(),
            Value::Int(32)
        );

        let default_table = builtin_make_hash_table(vec![]).expect("default table");
        let _ = builtin_puthash(vec![
            Value::Int(1),
            Value::symbol("x"),
            default_table,
        ])
        .expect("puthash for default table");
        assert_eq!(
            builtin_internal_hash_table_index_size(vec![default_table]).unwrap(),
            Value::Int(8)
        );

        let mid = builtin_make_hash_table(vec![Value::keyword(":size"), Value::Int(10)])
            .expect("size 10 table");
        for i in 0..10 {
            let i = i as i64;
            let _ = builtin_puthash(vec![Value::Int(i), Value::Int(i), mid])
                .expect("puthash while filling size 10 table");
        }
        assert_eq!(
            builtin_internal_hash_table_index_size(vec![mid]).unwrap(),
            Value::Int(16)
        );
        let _ = builtin_puthash(vec![Value::Int(10), Value::Int(10), mid])
            .expect("puthash crossing size 10 threshold");
        assert_eq!(
            builtin_internal_hash_table_index_size(vec![mid]).unwrap(),
            Value::Int(64)
        );
    }

    #[test]
    fn hash_table_size_tracks_growth_boundaries() {
        let tiny = builtin_make_hash_table(vec![Value::keyword(":size"), Value::Int(1)])
            .expect("size 1 table");
        let _ = builtin_puthash(vec![Value::Int(1), Value::symbol("x"), tiny])
            .expect("puthash for first tiny entry");
        assert_eq!(
            builtin_hash_table_size(vec![tiny]).unwrap(),
            Value::Int(1)
        );
        let _ = builtin_puthash(vec![Value::Int(2), Value::symbol("y"), tiny])
            .expect("puthash for second tiny entry");
        assert_eq!(builtin_hash_table_size(vec![tiny]).unwrap(), Value::Int(24));

        let default_table = builtin_make_hash_table(vec![]).expect("default table");
        let _ = builtin_puthash(vec![
            Value::Int(1),
            Value::symbol("default-value"),
            default_table,
        ])
        .expect("puthash for default table");
        assert_eq!(
            builtin_hash_table_size(vec![default_table]).unwrap(),
            Value::Int(6)
        );

        let mid = builtin_make_hash_table(vec![Value::keyword(":size"), Value::Int(10)])
            .expect("size 10 table");
        for i in 0..11 {
            let i = i as i64;
            let _ = builtin_puthash(vec![Value::Int(i), Value::Int(i), mid])
                .expect("puthash while filling size 10 table");
        }
        assert_eq!(builtin_hash_table_size(vec![mid]).unwrap(), Value::Int(40));
    }

    #[test]
    fn internal_hash_table_buckets_report_hash_diagnostics() {
        let table = builtin_make_hash_table(vec![
            Value::keyword(":test"),
            Value::symbol("equal"),
            Value::keyword(":size"),
            Value::Int(3),
        ])
        .expect("hash table");
        if let Value::HashTable(ht) = &table {
            with_heap_mut(|h| {
                let raw = h.get_hash_table_mut(*ht);
                let test = raw.test.clone();
                raw.data.insert(
                    Value::string("a").to_hash_key(&test),
                    Value::symbol("value-a"),
                );
                raw.data.insert(
                    Value::string("b").to_hash_key(&test),
                    Value::symbol("value-b"),
                );
            });
        } else {
            panic!("expected hash table");
        }

        let buckets = builtin_internal_hash_table_buckets(vec![table]).expect("bucket alists");
        let outer = list_to_vec(&buckets).expect("outer list");
        let mut seen = std::collections::BTreeMap::new();
        for bucket in outer {
            let entries = list_to_vec(&bucket).expect("bucket alist");
            for entry in entries {
                let Value::Cons(cell) = entry else {
                    panic!("expected alist cons entry");
                };
                let pair = read_cons(cell);
                let key = pair.car.as_str().expect("string key").to_string();
                let hash = pair.cdr.as_int().expect("diagnostic hash integer");
                seen.insert(key, hash);
            }
        }

        assert_eq!(seen.len(), 2);
        assert!(seen.contains_key("a"));
        assert!(seen.contains_key("b"));
    }

    #[test]
    fn internal_hash_table_buckets_match_oracle_small_string_hashes() {
        let table = builtin_make_hash_table(vec![
            Value::keyword(":test"),
            Value::symbol("equal"),
            Value::keyword(":size"),
            Value::Int(3),
        ])
        .expect("hash table");
        let _ = builtin_puthash(vec![Value::string("a"), Value::Int(1), table])
            .expect("puthash a");
        let _ = builtin_puthash(vec![Value::string("b"), Value::Int(2), table])
            .expect("puthash b");

        assert_eq!(
            builtin_internal_hash_table_buckets(vec![table]).expect("bucket alists"),
            Value::list(vec![
                Value::list(vec![Value::cons(Value::string("b"), Value::Int(114))]),
                Value::list(vec![Value::cons(Value::string("a"), Value::Int(113))]),
            ])
        );
    }

    #[test]
    fn internal_hash_table_buckets_match_oracle_eq_eql_fixnum_hashes() {
        for test_name in ["eq", "eql"] {
            let table = builtin_make_hash_table(vec![
                Value::keyword(":test"),
                Value::symbol(test_name),
                Value::keyword(":size"),
                Value::Int(3),
            ])
            .expect("hash table");
            let _ = builtin_puthash(vec![Value::Char('A'), Value::symbol("char"), table])
                .expect("puthash char");
            assert_eq!(
                builtin_gethash(vec![Value::Int(65), table, Value::symbol("miss")])
                    .expect("gethash int"),
                Value::symbol("char")
            );
            assert_eq!(
                builtin_gethash(vec![Value::Char('A'), table, Value::symbol("miss")])
                    .expect("gethash char"),
                Value::symbol("char")
            );
            assert_eq!(
                builtin_internal_hash_table_buckets(vec![table]).expect("bucket alists"),
                Value::list(vec![Value::list(vec![Value::cons(
                    Value::Int(65),
                    Value::Int(71)
                )])])
            );
        }

        let table = builtin_make_hash_table(vec![
            Value::keyword(":test"),
            Value::symbol("equal"),
            Value::keyword(":size"),
            Value::Int(3),
        ])
        .expect("hash table");
        let _ = builtin_puthash(vec![Value::Char('A'), Value::symbol("char"), table])
            .expect("puthash char");
        assert_eq!(
            builtin_internal_hash_table_buckets(vec![table]).expect("bucket alists"),
            Value::list(vec![Value::list(vec![Value::cons(
                Value::Int(65),
                Value::Int(65)
            )])])
        );
    }

    #[test]
    fn internal_hash_table_buckets_eq_pointer_keys_keep_distinct_hashes() {
        let table = builtin_make_hash_table(vec![
            Value::keyword(":test"),
            Value::symbol("eq"),
            Value::keyword(":size"),
            Value::Int(5),
        ])
        .expect("hash table");
        let key_a = Value::string("x");
        let key_b = Value::string("x");
        let _ = builtin_puthash(vec![key_a, Value::symbol("a"), table])
            .expect("puthash key-a");
        let _ = builtin_puthash(vec![key_b, Value::symbol("b"), table])
            .expect("puthash key-b");
        assert_eq!(
            builtin_hash_table_count(vec![table]).expect("hash-table-count"),
            Value::Int(2)
        );
        assert_eq!(
            builtin_gethash(vec![key_a, table, Value::symbol("miss")]).expect("gethash a"),
            Value::symbol("a")
        );
        assert_eq!(
            builtin_gethash(vec![key_b, table, Value::symbol("miss")]).expect("gethash b"),
            Value::symbol("b")
        );

        let buckets = builtin_internal_hash_table_buckets(vec![table]).expect("bucket alists");
        let outer = list_to_vec(&buckets).expect("outer list");
        let mut hashes = Vec::new();
        let mut keys = Vec::new();
        for bucket in outer {
            let entries = list_to_vec(&bucket).expect("bucket alist");
            for entry in entries {
                let Value::Cons(cell) = entry else {
                    panic!("expected alist cons entry");
                };
                let pair = read_cons(cell);
                keys.push(pair.car);
                hashes.push(pair.cdr.as_int().expect("diagnostic hash integer"));
            }
        }
        assert_eq!(keys.len(), 2);
        assert!(keys.iter().all(|key| key.as_str().is_some()));
        hashes.sort_unstable();
        assert_eq!(hashes.len(), 2);
        assert_ne!(hashes[0], hashes[1]);
    }

    #[test]
    fn internal_hash_table_buckets_equal_preserve_first_key_identity_on_overwrite() {
        let table = builtin_make_hash_table(vec![
            Value::keyword(":test"),
            Value::symbol("equal"),
            Value::keyword(":size"),
            Value::Int(5),
        ])
        .expect("hash table");
        let key_a = Value::string("x");
        let key_b = Value::string("x");
        let _ = builtin_puthash(vec![key_a, Value::symbol("a"), table])
            .expect("puthash key-a");
        let _ = builtin_puthash(vec![key_b, Value::symbol("b"), table])
            .expect("puthash key-b overwrite");
        assert_eq!(
            builtin_hash_table_count(vec![table]).expect("hash-table-count"),
            Value::Int(1)
        );
        assert_eq!(
            builtin_gethash(vec![
                Value::string("x"),
                table,
                Value::symbol("miss")
            ])
            .expect("gethash x"),
            Value::symbol("b")
        );

        let buckets = builtin_internal_hash_table_buckets(vec![table]).expect("bucket alists");
        let outer = list_to_vec(&buckets).expect("outer list");
        assert_eq!(outer.len(), 1);
        let entries = list_to_vec(&outer[0]).expect("bucket alist");
        assert_eq!(entries.len(), 1);
        let Value::Cons(cell) = &entries[0] else {
            panic!("expected alist cons entry");
        };
        let pair = read_cons(*cell);
        assert_eq!(pair.car.as_str(), Some("x"));
        assert!(eq_value(&pair.car, &key_a));
        assert!(!eq_value(&pair.car, &key_b));
    }

    #[test]
    fn internal_hash_table_buckets_match_oracle_small_float_hashes() {
        fn collect_float_hashes(table: Value) -> std::collections::BTreeMap<u64, i64> {
            let buckets = builtin_internal_hash_table_buckets(vec![table]).expect("bucket alists");
            let outer = list_to_vec(&buckets).expect("outer list");
            let mut seen = std::collections::BTreeMap::new();
            for bucket in outer {
                let entries = list_to_vec(&bucket).expect("bucket alist");
                for entry in entries {
                    let Value::Cons(cell) = entry else {
                        panic!("expected alist cons entry");
                    };
                    let pair = read_cons(cell);
                    let key_bits = pair.car.as_float().expect("float key").to_bits();
                    let hash = pair.cdr.as_int().expect("diagnostic hash integer");
                    seen.insert(key_bits, hash);
                }
            }
            seen
        }

        let expected = std::collections::BTreeMap::from([
            (1.0_f64.to_bits(), 1_072_693_248_i64),
            (2.0_f64.to_bits(), 1_073_741_824_i64),
        ]);

        for test_name in ["eql", "equal"] {
            let table = builtin_make_hash_table(vec![
                Value::keyword(":test"),
                Value::symbol(test_name),
                Value::keyword(":size"),
                Value::Int(3),
            ])
            .expect("hash table");
            let _ = builtin_puthash(vec![Value::Float(1.0, next_float_id()), Value::Int(1), table])
                .expect("puthash 1.0");
            let _ = builtin_puthash(vec![Value::Float(2.0, next_float_id()), Value::Int(2), table])
                .expect("puthash 2.0");

            assert_eq!(collect_float_hashes(table), expected);
        }
    }

    #[test]
    fn internal_hash_table_buckets_match_oracle_float_special_hashes() {
        fn collect_hashes(table: Value) -> Vec<i64> {
            let buckets = builtin_internal_hash_table_buckets(vec![table]).expect("bucket alists");
            let outer = list_to_vec(&buckets).expect("outer list");
            let mut seen = Vec::new();
            for bucket in outer {
                let entries = list_to_vec(&bucket).expect("bucket alist");
                for entry in entries {
                    let Value::Cons(cell) = entry else {
                        panic!("expected alist cons entry");
                    };
                    let pair = read_cons(cell);
                    let hash = pair.cdr.as_int().expect("diagnostic hash integer");
                    seen.push(hash);
                }
            }
            seen.sort_unstable();
            seen
        }

        let expected = vec![0_i64, 2_146_959_360_i64, 2_147_483_648_i64];
        for test_name in ["eql", "equal"] {
            let table = builtin_make_hash_table(vec![
                Value::keyword(":test"),
                Value::symbol(test_name),
                Value::keyword(":size"),
                Value::Int(5),
            ])
            .expect("hash table");
            let _ = builtin_puthash(vec![
                Value::Float(-0.0, next_float_id()),
                Value::symbol("neg"),
                table,
            ])
            .expect("puthash -0.0");
            let _ = builtin_puthash(vec![Value::Float(0.0, next_float_id()), Value::symbol("pos"), table])
                .expect("puthash 0.0");
            let _ = builtin_puthash(vec![
                Value::Float(f64::NAN, next_float_id()),
                Value::symbol("nan"),
                table,
            ])
            .expect("puthash nan");
            assert_eq!(collect_hashes(table), expected);
        }
    }

    #[test]
    fn internal_hash_table_introspection_type_errors() {
        assert!(builtin_internal_hash_table_buckets(vec![Value::Nil]).is_err());
        assert!(builtin_internal_hash_table_histogram(vec![Value::Nil]).is_err());
        assert!(builtin_internal_hash_table_index_size(vec![Value::Nil]).is_err());
    }
}
