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
use crate::tagged::gc::with_tagged_heap;
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
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_min_args(name: &str, args: &[Value], min: usize) -> Result<(), Flow> {
    if args.len() < min {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn expect_max_args(name: &str, args: &[Value], max: usize) -> Result<(), Flow> {
    if args.len() > max {
        Err(signal(
            "wrong-number-of-arguments",
            vec![Value::symbol(name), Value::fixnum(args.len() as i64)],
        ))
    } else {
        Ok(())
    }
}

fn validate_optional_obarray_arg(args: &[Value]) -> Result<(), Flow> {
    if let Some(obarray) = args.get(1) {
        if !obarray.is_nil() && !obarray.is_vector() {
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
        HashKey::Nil => Value::NIL,
        HashKey::True => Value::T,
        HashKey::Int(n) => Value::fixnum(*n),
        HashKey::Float(bits) => Value::make_float(f64::from_bits(*bits)),
        HashKey::FloatEq(bits, _id) => Value::make_float(f64::from_bits(*bits)),
        HashKey::Symbol(id) => Value::from_sym_id(*id),
        HashKey::Keyword(id) => Value::keyword(resolve_sym(*id)),
        HashKey::Text(text) => Value::string(text.clone()),
        HashKey::Char(c) => Value::char(*c),
        HashKey::Window(id) => Value::make_window(*id),
        HashKey::Frame(id) => Value::make_frame(*id),
        HashKey::Ptr(_) => Value::NIL, // can't reconstruct from pointer
        HashKey::EqualCons(car, cdr) => Value::cons(hash_key_to_value(car), hash_key_to_value(cdr)),
        HashKey::EqualVec(items) => {
            let vals: Vec<Value> = items.iter().map(hash_key_to_value).collect();
            Value::vector(vals)
        }
        HashKey::Cycle(index) => Value::string(format!("#{}", index)),
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
            if !cursor.is_cons() {
                break;
            };
            let pair_car = cursor.cons_car();
            let pair_cdr = cursor.cons_cdr();
            hash = sxhash_combine(hash, emacs_sxhash_obj_with_fallback(&pair_car, depth + 1));
            cursor = pair_cdr;
        }
    }
    if !cursor.is_nil() {
        hash = sxhash_combine(hash, emacs_sxhash_obj_with_fallback(&cursor, depth + 1));
    }
    hash
}

fn emacs_sxhash_vector(value: &Value, depth: usize) -> u64 {
    let items = value.as_vector_data().unwrap();
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
    match value.kind() {
        ValueKind::Fixnum(n) => Some(n as u64),
        ValueKind::Float => Some(value.xfloat().to_bits()),
        ValueKind::String => Some(emacs_hash_char_array(value.as_str().unwrap().as_bytes())),
        ValueKind::Cons => Some(emacs_sxhash_list(value, depth)),
        ValueKind::Veclike(VecLikeType::Vector) => Some(emacs_sxhash_vector(value, depth)),
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
    match value.kind() {
        ValueKind::Nil => 0_u8.hash(hasher),
        ValueKind::T => 1_u8.hash(hasher),
        ValueKind::Fixnum(n) => {
            // `equal` treats chars and ints with same codepoint as equal.
            2_u8.hash(hasher);
            n.hash(hasher);
        }
        ValueKind::Float => {
            3_u8.hash(hasher);
            value.xfloat().to_bits().hash(hasher);
        }
        ValueKind::Symbol(id) => {
            4_u8.hash(hasher);
            resolve_sym(id).hash(hasher);
        }
        ValueKind::Keyword(id) => {
            5_u8.hash(hasher);
            resolve_sym(id).hash(hasher);
        }
        ValueKind::String => {
            6_u8.hash(hasher);
            value.as_str().unwrap().hash(hasher);
        }
        ValueKind::Cons => {
            7_u8.hash(hasher);
            let pair_car = value.cons_car();
            let pair_cdr = value.cons_cdr();
            hash_value_for_equal(&pair_car, hasher, depth + 1);
            hash_value_for_equal(&pair_cdr, hasher, depth + 1);
        }
        ValueKind::Veclike(VecLikeType::Vector) | ValueKind::Veclike(VecLikeType::Record) => {
            8_u8.hash(hasher);
            let items = value.as_vector_data().unwrap().clone();
            items.len().hash(hasher);
            for item in items.iter() {
                hash_value_for_equal(item, hasher, depth + 1);
            }
        }
        ValueKind::Veclike(VecLikeType::Window) => {
            9_u8.hash(hasher);
            value.bits().hash(hasher);
        }
        ValueKind::Veclike(VecLikeType::Frame) => {
            10_u8.hash(hasher);
            value.bits().hash(hasher);
        }
        ValueKind::Veclike(VecLikeType::Buffer) => {
            11_u8.hash(hasher);
            value.bits().hash(hasher);
        }
        ValueKind::Veclike(VecLikeType::Timer) => {
            12_u8.hash(hasher);
            value.bits().hash(hasher);
        }
        ValueKind::Subr(id) => {
            13_u8.hash(hasher);
            resolve_sym(id).hash(hasher);
        }
        ValueKind::Veclike(VecLikeType::Lambda) => {
            14_u8.hash(hasher);
            value.bits().hash(hasher);
        }
        ValueKind::Veclike(VecLikeType::Macro) => {
            15_u8.hash(hasher);
            value.bits().hash(hasher);
        }
        ValueKind::Veclike(VecLikeType::HashTable) => {
            16_u8.hash(hasher);
            value.bits().hash(hasher);
        }
        ValueKind::Veclike(VecLikeType::ByteCode) => {
            17_u8.hash(hasher);
            value.bits().hash(hasher);
        }
        ValueKind::Veclike(VecLikeType::Marker) => {
            18_u8.hash(hasher);
            // Hash based on marker identity via pointer bits.
            value.bits().hash(hasher);
        }
        ValueKind::Veclike(VecLikeType::Overlay) => {
            19_u8.hash(hasher);
            value.bits().hash(hasher);
        }
        _ => {
            // Unknown or other types: hash by pointer bits
            20_u8.hash(hasher);
            value.bits().hash(hasher);
        }
    }
}

fn sxhash_emacs_uint_for(value: &Value, test: HashTableTest) -> u64 {
    match test {
        HashTableTest::Equal => emacs_sxhash_obj_with_fallback(value, 0),
        HashTableTest::Eq | HashTableTest::Eql => match value.kind() {
            ValueKind::Fixnum(n) => sxhash_eq_fixnum_uint(n as u64),
            ValueKind::Float if matches!(test, HashTableTest::Eql) => value.xfloat().to_bits(),
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
            _ => {
                let value = hash_key_to_value(key);
                reduce_emacs_uint_to_hash_hash(sxhash_emacs_uint_for(&value, HashTableTest::Eq))
            }
        },
        HashTableTest::Eql => match key {
            HashKey::Ptr(ptr) => reduce_emacs_uint_to_hash_hash(*ptr as u64),
            _ => {
                let value = hash_key_to_value(key);
                reduce_emacs_uint_to_hash_hash(sxhash_emacs_uint_for(&value, HashTableTest::Eql))
            }
        },
        HashTableTest::Equal => match key {
            HashKey::Float(bits) => reduce_emacs_uint_to_hash_hash(*bits),
            HashKey::Int(n) => reduce_emacs_uint_to_hash_hash(*n as u64),
            HashKey::Char(c) => reduce_emacs_uint_to_hash_hash((*c as u32) as u64),
            HashKey::Text(text) => {
                let hash = emacs_hash_char_array(text.as_bytes());
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
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let table = args[0].as_hash_table().unwrap().clone();
            if let Some(id) = table.test_name {
                Ok(Value::from_sym_id(id))
            } else {
                let sym = match table.test {
                    HashTableTest::Eq => "eq",
                    HashTableTest::Eql => "eql",
                    HashTableTest::Equal => "equal",
                };
                Ok(Value::symbol(sym))
            }
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[0]],
        )),
    }
}

/// (hash-table-size TABLE) -> integer
pub(crate) fn builtin_hash_table_size(args: Vec<Value>) -> EvalResult {
    expect_args("hash-table-size", &args, 1)?;
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let table = args[0].as_hash_table().unwrap().clone();
            Ok(Value::fixnum(table.size))
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[0]],
        )),
    }
}

/// (hash-table-rehash-size TABLE) -> float
pub(crate) fn builtin_hash_table_rehash_size(args: Vec<Value>) -> EvalResult {
    expect_args("hash-table-rehash-size", &args, 1)?;
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let table = args[0].as_hash_table().unwrap().clone();
            Ok(Value::make_float(table.rehash_size))
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[0]],
        )),
    }
}

/// (hash-table-rehash-threshold TABLE) -> float
pub(crate) fn builtin_hash_table_rehash_threshold(args: Vec<Value>) -> EvalResult {
    expect_args("hash-table-rehash-threshold", &args, 1)?;
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let table = args[0].as_hash_table().unwrap().clone();
            Ok(Value::make_float(table.rehash_threshold))
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[0]],
        )),
    }
}

/// (hash-table-weakness TABLE) -> nil | symbol
pub(crate) fn builtin_hash_table_weakness(args: Vec<Value>) -> EvalResult {
    expect_args("hash-table-weakness", &args, 1)?;
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let table = args[0].as_hash_table().unwrap().clone();
            Ok(match table.weakness {
                None => Value::NIL,
                Some(HashTableWeakness::Key) => Value::symbol("key"),
                Some(HashTableWeakness::Value) => Value::symbol("value"),
                Some(HashTableWeakness::KeyOrValue) => Value::symbol("key-or-value"),
                Some(HashTableWeakness::KeyAndValue) => Value::symbol("key-and-value"),
            })
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[0]],
        )),
    }
}

/// (copy-hash-table TABLE) -> new hash table with same entries
pub(crate) fn builtin_copy_hash_table(args: Vec<Value>) -> EvalResult {
    expect_args("copy-hash-table", &args, 1)?;
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let new_table = args[0].as_hash_table().unwrap().clone();
            Ok(with_tagged_heap(|h| h.alloc_hash_table(new_table)))
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[0]],
        )),
    }
}

/// (hash-table-keys TABLE) -> list of keys
#[cfg(test)]
pub(crate) fn builtin_hash_table_keys(args: Vec<Value>) -> EvalResult {
    expect_args("hash-table-keys", &args, 1)?;
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let table = args[0].as_hash_table().unwrap().clone();
            let keys: Vec<Value> = table
                .insertion_order
                .iter()
                .filter(|k| table.data.contains_key(k))
                .map(|key| hash_key_to_visible_value(&table, key))
                .collect();
            Ok(Value::list(keys))
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[0]],
        )),
    }
}

/// (hash-table-values TABLE) -> list of values
#[cfg(test)]
pub(crate) fn builtin_hash_table_values(args: Vec<Value>) -> EvalResult {
    expect_args("hash-table-values", &args, 1)?;
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let table = args[0].as_hash_table().unwrap().clone();
            let values: Vec<Value> = table
                .insertion_order
                .iter()
                .filter_map(|k| table.data.get(k).cloned())
                .collect();
            Ok(Value::list(values))
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[0]],
        )),
    }
}

/// `(sxhash-eq OBJECT)` -- hash OBJECT according to `eq` semantics.
pub(crate) fn builtin_sxhash_eq(args: Vec<Value>) -> EvalResult {
    expect_args("sxhash-eq", &args, 1)?;
    Ok(Value::fixnum(sxhash_for(&args[0], HashTableTest::Eq)))
}

/// `(sxhash-eql OBJECT)` -- hash OBJECT according to `eql` semantics.
pub(crate) fn builtin_sxhash_eql(args: Vec<Value>) -> EvalResult {
    expect_args("sxhash-eql", &args, 1)?;
    Ok(Value::fixnum(sxhash_for(&args[0], HashTableTest::Eql)))
}

/// `(sxhash-equal OBJECT)` -- hash OBJECT according to `equal` semantics.
pub(crate) fn builtin_sxhash_equal(args: Vec<Value>) -> EvalResult {
    expect_args("sxhash-equal", &args, 1)?;
    Ok(Value::fixnum(sxhash_for(&args[0], HashTableTest::Equal)))
}

/// `(sxhash-equal-including-properties OBJECT)` -- hash OBJECT like
/// `equal-including-properties`. NeoVM currently has no text properties, so
/// this matches `sxhash-equal`.
pub(crate) fn builtin_sxhash_equal_including_properties(args: Vec<Value>) -> EvalResult {
    expect_args("sxhash-equal-including-properties", &args, 1)?;
    Ok(Value::fixnum(sxhash_for(&args[0], HashTableTest::Equal)))
}

/// `(internal--hash-table-index-size TABLE)` -- report hash index width.
pub(crate) fn builtin_internal_hash_table_index_size(args: Vec<Value>) -> EvalResult {
    expect_args("internal--hash-table-index-size", &args, 1)?;
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let table = args[0].as_hash_table().unwrap().clone();
            Ok(Value::fixnum(
                internal_hash_table_index_size(&table).min(i64::MAX as usize) as i64,
            ))
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[0]],
        )),
    }
}

/// `(internal--hash-table-buckets TABLE)` -- return non-empty bucket alists.
pub(crate) fn builtin_internal_hash_table_buckets(args: Vec<Value>) -> EvalResult {
    expect_args("internal--hash-table-buckets", &args, 1)?;
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let table = args[0].as_hash_table().unwrap().clone();
            let buckets = internal_hash_table_nonempty_buckets(&table);
            if buckets.is_empty() {
                return Ok(Value::NIL);
            }
            let rendered = buckets
                .into_iter()
                .map(|bucket| {
                    let alist_items: Vec<Value> = bucket
                        .into_iter()
                        .map(|(key, hash)| Value::cons(key, Value::fixnum(hash)))
                        .collect();
                    Value::list(alist_items)
                })
                .collect();
            Ok(Value::list(rendered))
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[0]],
        )),
    }
}

/// `(internal--hash-table-histogram TABLE)` -- return (bucket-size . count)
/// alist for non-empty buckets.
pub(crate) fn builtin_internal_hash_table_histogram(args: Vec<Value>) -> EvalResult {
    expect_args("internal--hash-table-histogram", &args, 1)?;
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let table = args[0].as_hash_table().unwrap().clone();
            let buckets = internal_hash_table_nonempty_buckets(&table);
            if buckets.is_empty() {
                return Ok(Value::NIL);
            }
            let mut histogram: BTreeMap<i64, i64> = BTreeMap::new();
            for bucket in buckets {
                let size = bucket.len() as i64;
                *histogram.entry(size).or_insert(0) += 1;
            }
            let entries: Vec<Value> = histogram
                .into_iter()
                .map(|(size, count)| Value::cons(Value::fixnum(size), Value::fixnum(count)))
                .collect();
            Ok(Value::list(entries))
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("hash-table-p"), args[0]],
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
    Ok(Value::NIL)
}

/// (mapatoms FUNCTION &optional OBARRAY) — call FUNCTION with each interned symbol.
pub(crate) fn builtin_mapatoms(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    let (func, symbols) = collect_mapatoms_symbols(eval.obarray(), args)?;
    for sym in symbols {
        eval.apply(func, vec![sym])?;
    }
    Ok(Value::NIL)
}

pub(crate) fn collect_maphash_entries(
    args: Vec<Value>,
) -> Result<(Value, Vec<(Value, Value)>), Flow> {
    expect_args("maphash", &args, 2)?;
    let func = args[0];
    let entries = match args[1].kind() {
        ValueKind::Veclike(VecLikeType::HashTable) => {
            let table = args[1].as_hash_table().unwrap().clone();
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
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("hash-table-p"), args[1]],
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

    if let Some(custom_obarray) = args
        .get(1)
        .filter(|v| !v.is_nil() && !is_global_obarray_proxy_in_state(obarray, v))
    {
        if custom_obarray.is_vector() {
            let all_slots = custom_obarray.as_vector_data().unwrap().clone();
            let mut symbols = Vec::new();
            for slot in &all_slots {
                let mut current = *slot;
                loop {
                    match current.kind() {
                        ValueKind::Nil => break,
                        ValueKind::Cons => {
                            let car = current.cons_car();
                            let cdr = current.cons_cdr();
                            symbols.push(car);
                            current = cdr;
                        }
                        _ => break,
                    }
                }
            }
            return Ok((func, symbols));
        }
    }

    let symbols = obarray
        .all_symbols()
        .iter()
        .map(|s| Value::symbol(s.to_string()))
        .collect();
    Ok((func, symbols))
}

/// (unintern NAME OBARRAY) — remove symbol from obarray.
pub(crate) fn builtin_unintern(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_args("unintern", &args, 2)?;
    validate_optional_obarray_arg(&args)?;
    let name = match args[0].kind() {
        ValueKind::Symbol(id) => resolve_sym(id).to_owned(),
        ValueKind::String => args[0].as_str().unwrap().to_owned(),
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), args[0]],
            ));
        }
    };

    // Custom obarray path
    if let Some(custom_obarray) = args.get(1).filter(|v| !v.is_nil()) {
        if custom_obarray.is_vector() {
            let vec_data = custom_obarray.as_vector_data().unwrap();
            let vec_len = vec_data.len();
            if vec_len == 0 {
                return Ok(Value::NIL);
            }
            let bucket_idx = name
                .bytes()
                .fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64))
                as usize
                % vec_len;
            let bucket = vec_data[bucket_idx];

            // Walk the bucket chain and rebuild without the matching symbol
            let mut items = Vec::new();
            let mut found = false;
            let mut current = bucket;
            loop {
                match current.kind() {
                    ValueKind::Nil => break,
                    ValueKind::Cons => {
                        let car = current.cons_car();
                        let cdr = current.cons_cdr();
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
                    .fold(Value::NIL, |acc, sym| Value::cons(sym, acc));
                custom_obarray.as_vector_data_mut().unwrap()[bucket_idx] = new_bucket;
                return Ok(Value::T);
            }
            return Ok(Value::NIL);
        }
    }

    // Global obarray path
    let removed = eval.obarray.unintern(&name);
    Ok(if removed { Value::T } else { Value::NIL })
}
#[cfg(test)]
#[path = "hashtab_test.rs"]
mod tests;
