//! Char-table and bool-vector types.
//!
//! Since we cannot add new `Value` variants, these types are represented using
//! existing `Value` infrastructure:
//!
//! - **Char-table**: A `Value::Vector` whose first element is the tag symbol
//!   `--char-table--`.  The layout is:
//!   `[--char-table-- DEFAULT PARENT SUB-TYPE EXTRA-SLOTS-COUNT ...EXTRA-SLOTS... ASCII-CACHE ...DATA-PAIRS...]`
//!   where DATA-PAIRS are stored as consecutive `(char-code, value)` pairs
//!   starting after the optional ASCII cache.  The cache mirrors GNU Emacs'
//!   `ascii` char-table slot for the hot 0..127 lookup path.
//!
//! - **Bool-vector**: A `Value::Vector` whose first element is the tag symbol
//!   `--bool-vector--`.  The layout is:
//!   `[--bool-vector-- SIZE ...BITS...]`
//!   where SIZE is `Value::fixnum(length)` and each subsequent element is
//!   `Value::fixnum(0)` or `Value::fixnum(1)`.

use super::error::{EvalResult, Flow, signal};
use super::eval::{Context, push_scratch_gc_root, restore_scratch_gc_roots, save_scratch_gc_roots};
use super::intern::resolve_sym;
use super::value::*;
use std::collections::{BTreeMap, BTreeSet};

// ---------------------------------------------------------------------------
// Tag constants
// ---------------------------------------------------------------------------

const CHAR_TABLE_TAG: &str = "--char-table--";
const SUB_CHAR_TABLE_TAG: &str = "--sub-char-table--";
const BOOL_VECTOR_TAG: &str = "--bool-vector--";

// Char-table fixed-layout indices (after the tag at index 0):
const CT_DEFAULT: usize = 1; // default value
const CT_PARENT: usize = 2; // parent char-table or nil
const CT_SUBTYPE: usize = 3; // sub-type symbol
const CT_EXTRA_COUNT: usize = 4; // number of extra slots
const CT_EXTRA_START: usize = 5; // first extra slot (if any)
const CT_LOGICAL_LENGTH: i64 = 0x3F_FFFF;
/// Maximum valid Unicode code point.
const MAX_CHAR: i64 = 0x3F_FFFF;
const CT_ASCII_CACHE_LEN: usize = 128;
const CT_ASCII_CACHE_MAGIC: i64 = -7_000_001;

const GNU_CHAR_TABLE_STANDARD_SLOTS: usize = 4 + GNU_CHAR_TABLE_CONTENT_BLOCKS_USIZE;
const GNU_CHAR_TABLE_CONTENT_BLOCKS_USIZE: usize = 64;
const GNU_CHAR_TABLE_CONTENT_START: usize = 4;
const GNU_CHAR_TABLE_ASCII_SLOT: usize = 3;
const GNU_CHARTAB_SIZE: [usize; 4] = [64, 16, 32, 128];
const GNU_CHARTAB_CHARS: [i64; 4] = [65_536, 4_096, 128, 1];

// Bool-vector fixed-layout indices:
const BV_SIZE: usize = 1; // logical length

// ---------------------------------------------------------------------------
// Predicates
// ---------------------------------------------------------------------------

/// Return `true` if `v` is a char-table (tagged vector).
pub fn is_char_table(v: &Value) -> bool {
    if v.is_vector() {
        let vec = v.as_vector_data().unwrap();
        vec.len() >= CT_EXTRA_START
            && vec[0]
                .as_symbol_id()
                .map_or(false, |id| resolve_sym(id) == CHAR_TABLE_TAG)
    } else {
        false
    }
}

/// Return `true` if `v` is a bool-vector (tagged vector).
pub fn is_bool_vector(v: &Value) -> bool {
    if v.is_vector() {
        let vec = v.as_vector_data().unwrap();
        vec.len() >= 2
            && vec[0]
                .as_symbol_id()
                .map_or(false, |id| resolve_sym(id) == BOOL_VECTOR_TAG)
    } else {
        false
    }
}

/// Return the logical bit length if `v` is a bool-vector.
pub(crate) fn bool_vector_length(v: &Value) -> Option<i64> {
    if !v.is_vector() {
        return None;
    };
    let vec = v.as_vector_data().unwrap();
    if vec.len() < 2
        || !vec[0]
            .as_symbol_id()
            .map_or(false, |id| resolve_sym(id) == BOOL_VECTOR_TAG)
    {
        return None;
    }
    Some(match vec[BV_SIZE].kind() {
        ValueKind::Fixnum(n) => n,
        _ => 0,
    })
}

/// Return the logical sequence length if `v` is a char-table.
pub(crate) fn char_table_length(v: &Value) -> Option<i64> {
    if !v.is_vector() {
        return None;
    };
    let vec = v.as_vector_data().unwrap();
    if vec.len() >= CT_EXTRA_START
        && vec[0]
            .as_symbol_id()
            .map_or(false, |id| resolve_sym(id) == CHAR_TABLE_TAG)
    {
        Some(CT_LOGICAL_LENGTH)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Expect exactly N arguments, or signal `wrong-number-of-arguments`.
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

/// Expect at least N arguments.
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

/// Expect at most N arguments.
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

/// Signal `wrong-type-argument` with a predicate name.
fn wrong_type(pred: &str, got: &Value) -> Flow {
    signal("wrong-type-argument", vec![Value::symbol(pred), *got])
}

/// Extract an integer (Int or Char), signal otherwise.
fn expect_int(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        _other => Err(wrong_type("integerp", value)),
    }
}

/// Extract a non-negative integer (for index-like args), signaling with
/// `wholenump` on any mismatch.
fn expect_wholenump(value: &Value) -> Result<i64, Flow> {
    let n = match value.kind() {
        ValueKind::Fixnum(n) => n,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("wholenump"), *value],
            ));
        }
    };
    if n < 0 {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("wholenump"), *value],
        ));
    }
    Ok(n)
}

/// Data-pairs region start index for a char-table vector.
fn ct_data_start(vec: &[Value]) -> usize {
    ct_ascii_cache_range(vec)
        .map(|range| range.end)
        .unwrap_or_else(|| ct_ascii_cache_start(vec))
}

pub(crate) fn char_table_data_start(vec: &[Value]) -> usize {
    ct_data_start(vec)
}

fn ct_ascii_cache_start(vec: &[Value]) -> usize {
    let extra_count = match vec[CT_EXTRA_COUNT].kind() {
        ValueKind::Fixnum(n) => n as usize,
        _ => 0,
    };
    CT_EXTRA_START + extra_count
}

fn ct_ascii_cache_range(vec: &[Value]) -> Option<std::ops::Range<usize>> {
    let start = ct_ascii_cache_start(vec);
    let values_start = start + 1;
    let values_end = values_start + CT_ASCII_CACHE_LEN;
    if vec.len() >= values_end && vec[start].as_fixnum() == Some(CT_ASCII_CACHE_MAGIC) {
        Some(values_start..values_end)
    } else {
        None
    }
}

pub(crate) fn char_table_ascii_cache_range(vec: &[Value]) -> Option<std::ops::Range<usize>> {
    ct_ascii_cache_range(vec)
}

fn append_ascii_cache(vec: &mut Vec<Value>) {
    vec.push(Value::fixnum(CT_ASCII_CACHE_MAGIC));
    vec.resize(vec.len() + CT_ASCII_CACHE_LEN, Value::NIL);
}

fn ct_update_ascii_cache(vec: &mut [Value], min: i64, max: i64, value: Value) {
    if min > max || max < 0 || min >= CT_ASCII_CACHE_LEN as i64 {
        return;
    }
    let Some(range) = ct_ascii_cache_range(vec) else {
        return;
    };
    let start = min.max(0) as usize;
    let end = max.min(CT_ASCII_CACHE_LEN as i64 - 1) as usize;
    for ch in start..=end {
        vec[range.start + ch] = value;
    }
}

fn is_sub_char_table_literal(v: &Value) -> bool {
    if !v.is_vector() {
        return false;
    }
    let vec = v.as_vector_data().unwrap();
    vec.len() >= 3
        && vec[0]
            .as_symbol_id()
            .is_some_and(|id| resolve_sym(id) == SUB_CHAR_TABLE_TAG)
}

fn sub_char_table_depth_min_contents(v: &Value) -> Option<(usize, i64, Vec<Value>)> {
    if !is_sub_char_table_literal(v) {
        return None;
    }
    let vec = v.as_vector_data().unwrap();
    let depth = vec.get(1)?.as_fixnum()?;
    let min_char = vec.get(2)?.as_fixnum()?;
    if !(1..=3).contains(&depth) || !(0..=MAX_CHAR).contains(&min_char) {
        return None;
    }
    Some((depth as usize, min_char, vec[3..].to_vec()))
}

/// Build the temporary reader representation for GNU `#^^[...]` literals.
///
/// GNU Emacs creates a PVEC_SUB_CHAR_TABLE directly in `lread.c`; NeoVM has no
/// dedicated `Value` variant for it, so the reader keeps a tagged vector long
/// enough for the enclosing `#^[...]` reader path to fold it into the existing
/// sparse char-table representation.
pub(crate) fn make_sub_char_table_from_external_slots(items: &[Value]) -> Result<Value, String> {
    if items.len() < 2 {
        return Err("Invalid size of sub-char-table".to_string());
    }
    let depth = items[0]
        .as_fixnum()
        .ok_or_else(|| "Invalid depth in sub-char-table".to_string())?;
    if !(1..=3).contains(&depth) {
        return Err("Invalid depth in sub-char-table".to_string());
    }
    let min_char = items[1]
        .as_fixnum()
        .ok_or_else(|| "Invalid minimum character in sub-char-table".to_string())?;
    if !(0..=MAX_CHAR).contains(&min_char) {
        return Err("Invalid minimum character in sub-char-table".to_string());
    }

    let expected = 2 + GNU_CHARTAB_SIZE[depth as usize];
    if items.len() != expected {
        return Err("Invalid size in sub-char-table".to_string());
    }

    let mut vec = Vec::with_capacity(items.len() + 1);
    vec.push(Value::symbol(SUB_CHAR_TABLE_TAG));
    vec.extend_from_slice(items);
    Ok(Value::vector(vec))
}

fn char_table_extra_count(vec: &[Value]) -> usize {
    match vec.get(CT_EXTRA_COUNT).map(|v| v.kind()) {
        Some(ValueKind::Fixnum(n)) if n >= 0 => n as usize,
        _ => 0,
    }
}

fn char_table_extra_slot_value(table: &Value, idx: usize) -> Option<Value> {
    if !is_char_table(table) {
        return None;
    }
    let vec = table.as_vector_data().unwrap();
    let extra_count = char_table_extra_count(vec);
    (idx < extra_count).then(|| vec[CT_EXTRA_START + idx])
}

fn is_char_code_property_table(table: &Value) -> bool {
    if !is_char_table(table) {
        return false;
    }
    let vec = table.as_vector_data().unwrap();
    is_char_code_property_vec(vec)
}

fn is_char_code_property_vec(vec: &[Value]) -> bool {
    vec.get(CT_SUBTYPE)
        .is_some_and(|v| v.is_symbol_named("char-code-property-table"))
        && char_table_extra_count(vec) == 5
}

fn uniprop_compressed_string(value: Value) -> Option<Vec<u32>> {
    let string = value.as_lisp_string()?;
    let codes = crate::emacs_core::builtins::lisp_string_char_codes(string);
    matches!(codes.first(), Some(1 | 2)).then_some(codes)
}

fn uniprop_compressed_value_at(value: Value, offset: i64) -> Option<Value> {
    if !(0..GNU_CHARTAB_CHARS[2]).contains(&offset) {
        return None;
    }
    let codes = uniprop_compressed_string(value)?;
    let offset = offset as u32;
    match codes.first().copied() {
        Some(1) => {
            let mut cursor = 1;
            let mut idx = codes.get(cursor).copied()?;
            cursor += 1;
            while cursor < codes.len() && idx < GNU_CHARTAB_CHARS[2] as u32 {
                if idx == offset {
                    let value = codes[cursor] as i64;
                    return Some(if value > 0 {
                        Value::fixnum(value)
                    } else {
                        Value::NIL
                    });
                }
                idx += 1;
                cursor += 1;
            }
            Some(Value::NIL)
        }
        Some(2) => {
            let mut cursor = 1;
            let mut idx = 0_u32;
            while cursor < codes.len() && idx < GNU_CHARTAB_CHARS[2] as u32 {
                let value = codes[cursor] as i64;
                cursor += 1;
                let count = if cursor < codes.len() && codes[cursor] >= 128 {
                    let count = codes[cursor] - 128;
                    cursor += 1;
                    count
                } else {
                    1
                };
                let next = idx.saturating_add(count);
                if offset >= idx && offset < next {
                    return Some(Value::fixnum(value));
                }
                idx = next;
            }
            Some(Value::NIL)
        }
        _ => None,
    }
}

fn uniprop_compressed_runs(value: Value, start: i64, end: i64) -> Option<Vec<RawEntry>> {
    if end < start || end - start + 1 != GNU_CHARTAB_CHARS[2] {
        return None;
    }
    uniprop_compressed_string(value)?;

    let mut runs = Vec::new();
    let mut run_start = start;
    let mut previous = uniprop_compressed_value_at(value, 0)?;
    for offset in 1..GNU_CHARTAB_CHARS[2] {
        let current = uniprop_compressed_value_at(value, offset)?;
        if !eq_value(&previous, &current) {
            runs.push(RawEntry {
                start: run_start,
                end: start + offset - 1,
                value: previous,
            });
            run_start = start + offset;
            previous = current;
        }
    }
    runs.push(RawEntry {
        start: run_start,
        end,
        value: previous,
    });
    Some(runs)
}

fn flatten_uniprop_compressed_string(vec: &mut Vec<Value>, start: i64, codes: &[u32]) {
    match codes.first().copied() {
        Some(1) => {
            let mut cursor = 1;
            let Some(mut idx) = codes.get(cursor).copied().map(i64::from) else {
                return;
            };
            cursor += 1;
            while cursor < codes.len() && idx < GNU_CHARTAB_CHARS[2] {
                let value = codes[cursor] as i64;
                if value > 0 {
                    ct_set_char(vec, start + idx, Value::fixnum(value));
                }
                idx += 1;
                cursor += 1;
            }
        }
        Some(2) => {
            let mut cursor = 1;
            let mut idx = 0_i64;
            while cursor < codes.len() && idx < GNU_CHARTAB_CHARS[2] {
                let value = codes[cursor] as i64;
                cursor += 1;
                let count = if cursor < codes.len() && codes[cursor] >= 128 {
                    let count = codes[cursor] as i64 - 128;
                    cursor += 1;
                    count
                } else {
                    1
                };
                for _ in 0..count {
                    if idx >= GNU_CHARTAB_CHARS[2] {
                        break;
                    }
                    ct_set_char(vec, start + idx, Value::fixnum(value));
                    idx += 1;
                }
            }
        }
        _ => {}
    }
}

fn flatten_char_table_slot(
    vec: &mut Vec<Value>,
    value: Value,
    start: i64,
    span: i64,
    is_uniprop: bool,
) {
    if value.is_nil() {
        return;
    }

    if let Some((depth, min_char, contents)) = sub_char_table_depth_min_contents(&value) {
        flatten_sub_char_table(vec, depth, min_char, &contents, is_uniprop);
        return;
    }

    if is_uniprop
        && span == GNU_CHARTAB_CHARS[2]
        && let Some(codes) = uniprop_compressed_string(value)
    {
        flatten_uniprop_compressed_string(vec, start, &codes);
        return;
    }

    let end = (start + span - 1).min(MAX_CHAR);
    if start == end {
        ct_set_char(vec, start, value);
    } else {
        ct_set_range(vec, start, end, value);
    }
}

fn flatten_sub_char_table(
    vec: &mut Vec<Value>,
    depth: usize,
    min_char: i64,
    contents: &[Value],
    is_uniprop: bool,
) {
    if depth > 3 || contents.len() != GNU_CHARTAB_SIZE[depth] {
        return;
    }
    let span = GNU_CHARTAB_CHARS[depth];
    for (idx, value) in contents.iter().copied().enumerate() {
        flatten_char_table_slot(vec, value, min_char + idx as i64 * span, span, is_uniprop);
    }
}

/// Build a NeoVM char-table from GNU's readable `#^[...]` char-table literal.
///
/// GNU's external order is:
/// `DEFAULT PARENT PURPOSE ASCII CONTENTS[64] EXTRAS...`.
pub(crate) fn make_char_table_from_external_slots(items: &[Value]) -> Result<Value, String> {
    if items.len() < GNU_CHAR_TABLE_STANDARD_SLOTS {
        return Err("Invalid size char-table".to_string());
    }

    let default = items[0];
    let parent = items[1];
    let purpose = items[2];
    let extra_count = items.len() - GNU_CHAR_TABLE_STANDARD_SLOTS;
    let mut vec = vec![
        Value::symbol(CHAR_TABLE_TAG),
        default,
        parent,
        purpose,
        Value::fixnum(extra_count as i64),
    ];
    vec.extend_from_slice(&items[GNU_CHAR_TABLE_STANDARD_SLOTS..]);
    append_ascii_cache(&mut vec);

    let is_uniprop = purpose.is_symbol_named("char-code-property-table") && extra_count == 5;
    for block in 0..GNU_CHAR_TABLE_CONTENT_BLOCKS_USIZE {
        let start = block as i64 * GNU_CHAR_TABLE_BLOCK_CHARS;
        flatten_char_table_slot(
            &mut vec,
            items[GNU_CHAR_TABLE_CONTENT_START + block],
            start,
            GNU_CHAR_TABLE_BLOCK_CHARS,
            is_uniprop,
        );
    }

    // GNU keeps an ASCII cache slot that takes precedence for 0..127.  Append it
    // after the content blocks so the existing "last assignment wins" lookup
    // model observes the same precedence.
    flatten_char_table_slot(
        &mut vec,
        items[GNU_CHAR_TABLE_ASCII_SLOT],
        0,
        128,
        is_uniprop,
    );

    Ok(Value::vector(vec))
}

// ---------------------------------------------------------------------------
// Char-table builtins
// ---------------------------------------------------------------------------

/// Create a char-table `Value` directly (for use in bootstrap code).
pub fn make_char_table_value(sub_type: Value, default: Value) -> Value {
    make_char_table_with_extra_slots(sub_type, default, 0)
}

/// Create a char-table with a specified number of extra slots.
pub fn make_char_table_with_extra_slots(sub_type: Value, default: Value, n_extras: i64) -> Value {
    let mut vec = vec![
        Value::symbol(CHAR_TABLE_TAG),
        default,                 // CT_DEFAULT
        Value::NIL,              // CT_PARENT
        sub_type,                // CT_SUBTYPE
        Value::fixnum(n_extras), // CT_EXTRA_COUNT
    ];
    // Allocate extra slots initialised to nil.
    for _ in 0..n_extras {
        vec.push(Value::NIL);
    }
    append_ascii_cache(&mut vec);
    Value::vector(vec)
}

/// Set a single character entry in a char-table Value (for bootstrap code).
/// Panics if `table` is not a char-table Vector.
pub fn ct_set_single(table: &Value, ch: i64, value: Value) {
    if table.is_vector() {
        let mut vec = table
            .as_vector_data()
            .map(|items| items.to_vec())
            .unwrap_or_default();
        ct_set_char(&mut vec, ch, value);
        let _ = table.replace_vector_data(vec);
    } else {
        panic!("ct_set_single: expected char-table Vector");
    }
}

/// `(make-char-table SUB-TYPE &optional DEFAULT)` -- create a char-table.
///
/// If SUB-TYPE has a `char-table-extra-slots` property, its value
/// specifies how many extra slots the char-table has (0..10).
pub(crate) fn builtin_make_char_table(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    expect_min_args("make-char-table", &args, 1)?;
    expect_max_args("make-char-table", &args, 2)?;
    let sub_type = args[0];
    let default = if args.len() > 1 { args[1] } else { Value::NIL };
    // Read char-table-extra-slots property from the sub-type symbol,
    // matching GNU Emacs chartab.c:Fmake_char_table.
    let n_extras = if let Some(name) = sub_type.as_symbol_name() {
        eval.obarray
            .get_property(name, "char-table-extra-slots")
            .and_then(|v| v.as_int())
            .unwrap_or(0)
    } else {
        0
    };
    Ok(make_char_table_with_extra_slots(
        sub_type, default, n_extras,
    ))
}

/// `(char-table-p OBJ)` -- return t if OBJ is a char-table.
pub(crate) fn builtin_char_table_p(args: Vec<Value>) -> EvalResult {
    expect_args("char-table-p", &args, 1)?;
    Ok(Value::bool_val(is_char_table(&args[0])))
}

/// `(set-char-table-range CHAR-TABLE RANGE VALUE)` -- set entries.
///
/// RANGE may be:
/// - a character (integer/char) -- set that single entry
/// - a cons `(MIN . MAX)` -- set all characters MIN..=MAX
/// - `nil` -- set the default value
/// - `t` -- set all character entries while leaving the default slot alone
pub(crate) fn builtin_set_char_table_range(args: Vec<Value>) -> EvalResult {
    expect_args("set-char-table-range", &args, 3)?;
    let table = &args[0];
    let range = &args[1];
    let value = &args[2];

    if !is_char_table(table) {
        return Err(wrong_type("char-table-p", table));
    }

    let mut vec = table.as_vector_data().unwrap().clone();

    match range.kind() {
        // nil -> set default
        ValueKind::Nil => {
            vec[CT_DEFAULT] = *value;
        }
        // t -> set all characters, but not the default slot.
        ValueKind::T => {
            ct_set_range(&mut vec, 0, MAX_CHAR, *value);
        }
        // Single character
        ValueKind::Fixnum(_) => {
            let ch = expect_int(range)?;
            ct_set_char(&mut vec, ch, *value);
        }
        // Range cons (MIN . MAX)
        ValueKind::Cons => {
            let pair_car = range.cons_car();
            let pair_cdr = range.cons_cdr();
            let min = expect_int(&pair_car)?;
            let max = expect_int(&pair_cdr)?;
            if min > max {
                return Err(signal(
                    "args-out-of-range",
                    vec![Value::fixnum(min), Value::fixnum(max)],
                ));
            }
            ct_set_range(&mut vec, min, max, *value);
        }
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("char-table-range"), *range],
            ));
        }
    }

    let _ = table.replace_vector_data(vec);

    Ok(*value)
}

/// Set a single character entry in the char-table's data pairs.
fn ct_set_char(vec: &mut Vec<Value>, ch: i64, value: Value) {
    ct_update_ascii_cache(vec, ch, ch, value);
    vec.push(Value::fixnum(ch));
    vec.push(value);
}

/// Set a range entry in the char-table's data pairs.
/// The range is stored as a `Cons(min . max)` key.
fn ct_set_range(vec: &mut Vec<Value>, min: i64, max: i64, value: Value) {
    ct_update_ascii_cache(vec, min, max, value);
    vec.push(Value::cons(Value::fixnum(min), Value::fixnum(max)));
    vec.push(value);
}

/// Look up a single character in the data pairs (no parent/default fallback).
/// The last assignment that covers the character wins, matching GNU Emacs
/// `set-char-table-range` overwrite semantics for both single-char and range
/// entries.
fn ct_get_char(vec: &[Value], ch: i64, is_uniprop: bool) -> Option<Value> {
    let start = ct_data_start(vec);
    let len = vec.len();
    if len < start + 2 {
        return None;
    }
    // Scan right-to-left so the first match seen is the most recently
    // pushed entry — matching the "last assignment wins" semantic of
    // `set-char-table-range` without needing to scan every pair on
    // every call. The hot font-lock/syntax-ppss path pounds this
    // function millions of times per fontification; the old
    // unconditional O(N) scan was the dominant cost on a 147-char
    // *scratch* buffer (see commit note).
    let mut i = len; // walk backwards two slots at a time
    while i >= start + 2 {
        i -= 2;
        let key = vec[i];
        match key.kind() {
            ValueKind::Fixnum(existing) => {
                if existing == ch {
                    return Some(vec[i + 1]);
                }
            }
            ValueKind::Cons => {
                let pair_car = key.cons_car();
                let pair_cdr = key.cons_cdr();
                if let (Some(min), Some(max)) = (pair_car.as_fixnum(), pair_cdr.as_fixnum()) {
                    if ch >= min && ch <= max {
                        let value = vec[i + 1];
                        if is_uniprop
                            && max - min + 1 == GNU_CHARTAB_CHARS[2]
                            && let Some(decoded) = uniprop_compressed_value_at(value, ch - min)
                        {
                            return Some(decoded);
                        }
                        return Some(value);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn ct_lookup_ascii_cached(table: &Value, ch: i64) -> Option<Value> {
    if !(0..CT_ASCII_CACHE_LEN as i64).contains(&ch) {
        return None;
    }

    let ch = ch as usize;
    let mut current = *table;
    loop {
        let vec_ref = current.as_vector_data()?;
        let Some(cache_range) = ct_ascii_cache_range(vec_ref) else {
            return None;
        };

        let value = vec_ref[cache_range.start + ch];
        if !value.is_nil() {
            return Some(value);
        }

        let default = vec_ref[CT_DEFAULT];
        if !default.is_nil() {
            return Some(default);
        }

        let parent = vec_ref[CT_PARENT];
        if !is_char_table(&parent) {
            return Some(Value::NIL);
        }
        current = parent;
    }
}

/// `(char-table-range CHAR-TABLE RANGE)` -- look up a value.
///
/// RANGE may be:
/// - a character -- look up that character (with parent fallback)
/// - `nil` -- return the default value
pub(crate) fn builtin_char_table_range(args: Vec<Value>) -> EvalResult {
    expect_args("char-table-range", &args, 2)?;
    let table = &args[0];
    let range = &args[1];

    if !is_char_table(table) {
        return Err(wrong_type("char-table-p", table));
    }

    match range.kind() {
        ValueKind::Nil => {
            // Return the default value.
            let vec = table.as_vector_data().unwrap();
            Ok(vec[CT_DEFAULT])
        }
        ValueKind::Fixnum(_) => {
            let ch = expect_int(range)?;
            ct_lookup(table, ch)
        }
        ValueKind::Cons => {
            let pair_car = range.cons_car();
            let pair_cdr = range.cons_cdr();
            let from = expect_int(&pair_car)?;
            let _to = expect_int(&pair_cdr)?;
            let (value, _run_from, _run_to) = ct_lookup_and_range(table, from)?;
            Ok(value)
        }
        _ => Err(signal(
            "error",
            vec![Value::string(
                "Invalid RANGE argument to `char-table-range'",
            )],
        )),
    }
}

/// Recursive char-table lookup: check own entries, then default, then parent.
///
/// This matches GNU Emacs semantics:
/// 1. Look up the character in the char-table's data pairs
/// 2. If the local entry is nil or absent, use the char-table's default value
/// 3. If default is nil, recursively check the parent char-table
pub(crate) fn ct_lookup(table: &Value, ch: i64) -> EvalResult {
    if !table.is_vector() {
        return Err(wrong_type("char-table-p", table));
    }
    if let Some(value) = ct_lookup_ascii_cached(table, ch) {
        return Ok(value);
    }
    // Borrow the Vec instead of cloning — the 115K clones/sec we used to
    // do in font-lock's syntax-ppss path each allocated a ~50+-entry Vec
    // and nuked syntax-table reading throughput. GNU's `CHAR_TABLE_REF`
    // is direct array indexing; the closest we can do without reshaping
    // the table is to index without copying.
    let vec_ref = table.as_vector_data().unwrap();

    if let Some(val) = ct_get_char(vec_ref, ch, is_char_code_property_vec(vec_ref)) {
        if !val.is_nil() {
            return Ok(val);
        }
    }

    let default = vec_ref[CT_DEFAULT];
    let parent = vec_ref[CT_PARENT];

    if !default.is_nil() {
        Ok(default)
    } else if is_char_table(&parent) {
        ct_lookup(&parent, ch)
    } else {
        Ok(Value::NIL)
    }
}

/// Translate character `c` through translation `table`.
///
/// Mirrors GNU `translate_char` (character.c:151). If `table` is a
/// char-table, look up `c`; if the entry is a character, that's the
/// translation. If `table` is a list, fold left through all tables.
/// Returns `c` unchanged if no translation applies.
pub(crate) fn translate_char(table: &Value, c: i64) -> i64 {
    if is_char_table(table) {
        match ct_lookup(table, c) {
            Ok(val) => match val.kind() {
                ValueKind::Fixnum(n)
                    if (0..=crate::emacs_core::emacs_char::MAX_CHAR as i64).contains(&n) =>
                {
                    n
                }
                _ => c,
            },
            Err(_) => c,
        }
    } else if table.is_cons() {
        let mut result = c;
        let mut cur = *table;
        while cur.is_cons() {
            let car = cur.cons_car();
            result = translate_char(&car, result);
            cur = cur.cons_cdr();
        }
        result
    } else {
        c
    }
}

/// Return the unified character code for `c`, given the value `val`
/// retrieved from `Vchar_unify_table`.
///
/// Mirrors GNU `maybe_unify_char` (charset.c:1606). `val` may be:
///   * nil — return `c` unchanged
///   * a fixnum — that fixnum is the unified code
///   * a charset symbol — would normally trigger `load_charset` and a
///     re-lookup. Neomacs lacks the full charset/decoder infrastructure
///     today, so we treat this case as identity. Once charsets are
///     implemented this branch should re-lookup through
///     `Vchar_unify_table` after `load_charset`.
pub(crate) fn maybe_unify_char(c: i64, val: &Value) -> i64 {
    if let Some(n) = val.as_fixnum() {
        if (0..=MAX_CHAR).contains(&n) {
            return n;
        }
    }
    // nil, or charset-symbol fallback — TODO: full charset support.
    c
}

fn ct_lookup_and_range(table: &Value, ch: i64) -> Result<(Value, i64, i64), Flow> {
    if !is_char_table(table) {
        return Err(wrong_type("char-table-p", table));
    }
    for run in ct_effective_runs(table) {
        if ch >= run.start && ch <= run.end {
            return Ok((run.value, run.start, run.end));
        }
    }
    Ok((Value::NIL, 0, MAX_CHAR))
}

fn key_span(key: Value) -> Option<(i64, i64)> {
    match key.kind() {
        ValueKind::Fixnum(ch) => Some((ch, ch)),
        ValueKind::Cons => {
            let start = key.cons_car().as_fixnum()?;
            let end = key.cons_cdr().as_fixnum()?;
            Some((start, end))
        }
        _ => None,
    }
}

fn refine_atomic_boundary(start: i64, end: i64, ch: i64, lo: &mut i64, hi: &mut i64) {
    let domain_end = MAX_CHAR.saturating_add(1);
    let start = start.clamp(0, domain_end);
    let end_exclusive = end.saturating_add(1).clamp(0, domain_end);
    for boundary in [start, end_exclusive] {
        if boundary <= ch {
            *lo = (*lo).max(boundary);
        } else {
            *hi = (*hi).min(boundary);
        }
    }
}

fn ct_lookup_atomic_range(table: &Value, ch: i64) -> Result<(Value, i64, i64), Flow> {
    if !is_char_table(table) {
        return Err(wrong_type("char-table-p", table));
    }
    if !(0..=MAX_CHAR).contains(&ch) {
        return Ok((Value::NIL, 0, MAX_CHAR));
    }

    let vec = table.as_vector_data().unwrap();
    let start = ct_data_start(vec);
    let mut lo = 0;
    let mut hi = MAX_CHAR.saturating_add(1);
    let mut found_local = false;
    let mut local_value = Value::NIL;

    let mut i = vec.len();
    while i >= start + 2 {
        i -= 2;
        if let Some((entry_start, entry_end)) = key_span(vec[i]) {
            refine_atomic_boundary(entry_start, entry_end, ch, &mut lo, &mut hi);
            if !found_local && ch >= entry_start && ch <= entry_end {
                found_local = true;
                local_value = vec[i + 1];
            }
        }
    }

    let atomic_end = hi.saturating_sub(1).min(MAX_CHAR);
    if found_local && !local_value.is_nil() {
        return Ok((local_value, lo, atomic_end));
    }

    let default = vec[CT_DEFAULT];
    if !default.is_nil() {
        return Ok((default, lo, atomic_end));
    }

    let parent = vec[CT_PARENT];
    if is_char_table(&parent) {
        let (parent_value, parent_start, parent_end) = ct_lookup_atomic_range(&parent, ch)?;
        return Ok((
            parent_value,
            lo.max(parent_start),
            atomic_end.min(parent_end),
        ));
    }

    Ok((Value::NIL, lo, atomic_end))
}

/// GNU `char-table-ref-and-range`-style helper used by subsystems that need
/// the effective value together with the maximal contiguous run covering `ch`.
pub(crate) fn char_table_ref_and_range(table: &Value, ch: i64) -> Result<(Value, i64, i64), Flow> {
    ct_lookup_and_range(table, ch)
}

/// Return the effective value and a contiguous range around `ch` where no local
/// char-table assignment boundary occurs.
///
/// This range may be smaller than GNU's maximal `char-table-ref-and-range`
/// result, but every character in it has the same effective value.  It is for
/// bulk mutators that only need a correct split point and would otherwise pay to
/// rebuild the full effective run list for each cursor step.
pub(crate) fn char_table_ref_and_atomic_range(
    table: &Value,
    ch: i64,
) -> Result<(Value, i64, i64), Flow> {
    ct_lookup_atomic_range(table, ch)
}

/// `(char-table-parent CHAR-TABLE)` -- return the parent table (or nil).
pub(crate) fn builtin_char_table_parent(args: Vec<Value>) -> EvalResult {
    expect_args("char-table-parent", &args, 1)?;
    let table = &args[0];
    if !is_char_table(table) {
        return Err(wrong_type("char-table-p", table));
    }
    let vec = table.as_vector_data().unwrap();
    Ok(vec[CT_PARENT])
}

/// Return the sparse local `(key . value)` entries stored directly in a char-table.
///
/// Keys are either character codes (fixnums) or range conses `(FROM . TO)`.
/// Parent/default fallback is intentionally not applied here; callers that need
/// effective values should use `ct_lookup`.
pub(crate) fn char_table_local_entries(table: &Value) -> Result<Vec<(Value, Value)>, Flow> {
    if !is_char_table(table) {
        return Err(wrong_type("char-table-p", table));
    }
    let vec = table.as_vector_data().unwrap().clone();
    let start = ct_data_start(&vec);
    let mut out = Vec::new();
    let mut i = start;
    while i + 1 < vec.len() {
        match vec[i].kind() {
            ValueKind::Fixnum(_) | ValueKind::Cons => out.push((vec[i], vec[i + 1])),
            _ => {}
        }
        i += 2;
    }
    Ok(out)
}

/// `(set-char-table-parent CHAR-TABLE PARENT)` -- set the parent table.
pub(crate) fn builtin_set_char_table_parent(args: Vec<Value>) -> EvalResult {
    expect_args("set-char-table-parent", &args, 2)?;
    let table = &args[0];
    let parent = &args[1];
    if !is_char_table(table) {
        return Err(wrong_type("char-table-p", table));
    }

    // parent must be nil or a char-table.
    if !parent.is_nil() && !is_char_table(parent) {
        return Err(wrong_type("char-table-p", parent));
    }

    if !parent.is_nil() {
        let mut cursor = *parent;
        while is_char_table(&cursor) {
            if cursor.is_vector() && table.is_vector() {
                // Check pointer equality to detect cycles
                if std::ptr::eq(
                    cursor.as_vector_data().unwrap() as *const _,
                    table.as_vector_data().unwrap() as *const _,
                ) {
                    return Err(signal(
                        "error",
                        vec![Value::string(
                            "Attempt to make a chartable be its own parent",
                        )],
                    ));
                }
            }
            let vec = cursor.as_vector_data().unwrap().clone();
            cursor = vec[CT_PARENT];
        }
    }

    let _ = table.set_vector_slot(CT_PARENT, *parent);
    Ok(*parent)
}

/// `(map-char-table FUNCTION CHAR-TABLE)` -- call FUNCTION for each
/// entry with a non-nil value.  FUNCTION receives `(KEY VALUE)` where
/// KEY is either a character (integer) or a cons `(FROM . TO)` for ranges.
///
/// GNU Emacs passes a shared mutable cons cell for range keys; if Lisp code
/// retains those keys, later internal mutations are observable.  Mirror that
/// behavior instead of materializing fresh range objects.
/// Returns nil.
pub(crate) fn for_each_char_table_mapping(
    table: &Value,
    mut f: impl FnMut(Value, Value) -> Result<(), Flow>,
) -> Result<(), Flow> {
    if !is_char_table(table) {
        return Err(wrong_type("char-table-p", table));
    }

    let shared_range = Value::cons(Value::fixnum(0), Value::fixnum(MAX_CHAR));
    let saved = save_scratch_gc_roots();
    push_scratch_gc_root(shared_range);
    let result = (|| {
        for run in ct_effective_runs(table) {
            shared_range.set_car(Value::fixnum(run.start));
            shared_range.set_cdr(Value::fixnum(run.end));
            if run.value.is_nil() {
                continue;
            }
            let key = if run.start == run.end {
                Value::fixnum(run.start)
            } else {
                shared_range
            };
            let value = decode_unicode_property_map_value(*table, run.value);
            f(key, value)?;
        }
        Ok(())
    })();
    restore_scratch_gc_roots(saved);
    result
}

pub(crate) fn builtin_map_char_table(eval: &mut Context, args: Vec<Value>) -> EvalResult {
    expect_args("map-char-table", &args, 2)?;
    let func = args[0];
    let table = args[1];
    for_each_char_table_mapping(&table, |key, value| {
        let _ = eval.apply(func, vec![key, value])?;
        Ok(())
    })?;
    Ok(Value::NIL)
}

/// Resolve a char-table into non-overlapping effective runs, including nil.
fn ct_resolved_entries(table: &Value) -> Vec<(Value, Value)> {
    ct_effective_runs(table)
        .into_iter()
        .filter(|run| !run.value.is_nil())
        .map(|run| (run_key(run.start, run.end), run.value))
        .collect()
}

#[derive(Clone, Copy)]
struct RawEntry {
    start: i64,
    end: i64,
    value: Value,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct EffectiveRun {
    start: i64,
    end: i64,
    value: Value,
}

fn ct_collect_raw_entries(vec: &[Value], is_uniprop: bool) -> Vec<RawEntry> {
    let start = ct_data_start(vec);
    let mut raws = Vec::new();
    let mut i = start;
    while i + 1 < vec.len() {
        match vec[i].kind() {
            ValueKind::Fixnum(ch) => raws.push(RawEntry {
                start: ch,
                end: ch,
                value: vec[i + 1],
            }),
            ValueKind::Cons => {
                let pair_car = vec[i].cons_car();
                let pair_cdr = vec[i].cons_cdr();
                if let (Some(min), Some(max)) = (pair_car.as_fixnum(), pair_cdr.as_fixnum()) {
                    if is_uniprop
                        && let Some(mut decoded) = uniprop_compressed_runs(vec[i + 1], min, max)
                    {
                        raws.append(&mut decoded);
                    } else {
                        raws.push(RawEntry {
                            start: min,
                            end: max,
                            value: vec[i + 1],
                        });
                    }
                }
            }
            _ => {}
        }
        i += 2;
    }
    raws
}

fn ct_collect_local_raw_entries(vec: &[Value]) -> Vec<RawEntry> {
    ct_collect_raw_entries(vec, false)
}

fn ct_effective_runs(table: &Value) -> Vec<EffectiveRun> {
    if !table.is_vector() {
        return vec![EffectiveRun {
            start: 0,
            end: MAX_CHAR,
            value: Value::NIL,
        }];
    };
    let vec = table.as_vector_data().unwrap().clone();
    let raws = ct_collect_raw_entries(&vec, is_char_code_property_vec(&vec));
    let default = vec[CT_DEFAULT];
    let parent = vec[CT_PARENT];
    let domain_end = MAX_CHAR.saturating_add(1);
    let parent_runs = if is_char_table(&parent) {
        ct_effective_runs(&parent)
    } else {
        vec![EffectiveRun {
            start: 0,
            end: MAX_CHAR,
            value: Value::NIL,
        }]
    };

    let mut boundaries = BTreeSet::new();
    let mut starts: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    let mut ends: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    boundaries.insert(0);
    boundaries.insert(domain_end);
    for (idx, raw) in raws.iter().enumerate() {
        let end_exclusive = raw.end.saturating_add(1).min(domain_end);
        boundaries.insert(raw.start);
        boundaries.insert(end_exclusive);
        starts.entry(raw.start).or_default().push(idx);
        ends.entry(end_exclusive).or_default().push(idx);
    }
    for run in &parent_runs {
        boundaries.insert(run.start);
        boundaries.insert(run.end.saturating_add(1).min(domain_end));
    }

    let boundary_vec = boundaries.into_iter().collect::<Vec<_>>();
    let mut runs: Vec<EffectiveRun> = Vec::new();
    let mut active_raws = BTreeSet::new();
    let mut parent_idx = 0usize;

    for window in boundary_vec.windows(2) {
        let start = window[0];
        let end_exclusive = window[1];
        if let Some(indices) = ends.get(&start) {
            for idx in indices {
                active_raws.remove(idx);
            }
        }
        if let Some(indices) = starts.get(&start) {
            for idx in indices {
                active_raws.insert(*idx);
            }
        }
        if start > MAX_CHAR || end_exclusive <= start {
            continue;
        }
        let end = end_exclusive.saturating_sub(1).min(MAX_CHAR);
        while parent_idx + 1 < parent_runs.len() && start > parent_runs[parent_idx].end {
            parent_idx += 1;
        }
        let local = active_raws.iter().next_back().map(|idx| raws[*idx].value);
        let value = match local {
            Some(local) if !local.is_nil() => local,
            _ if !default.is_nil() => default,
            _ => parent_runs
                .get(parent_idx)
                .filter(|run| start >= run.start && start <= run.end)
                .map(|run| run.value)
                .unwrap_or(Value::NIL),
        };
        if let Some(previous) = runs.last_mut()
            && previous.end.saturating_add(1) == start
            && eq_value(&previous.value, &value)
        {
            previous.end = end;
        } else {
            runs.push(EffectiveRun { start, end, value });
        }
    }

    if runs.is_empty() {
        vec![EffectiveRun {
            start: 0,
            end: MAX_CHAR,
            value: Value::NIL,
        }]
    } else {
        runs
    }
}

fn run_key(start: i64, end: i64) -> Value {
    if start == end {
        Value::fixnum(start)
    } else {
        Value::cons(Value::fixnum(start), Value::fixnum(end))
    }
}

pub(crate) fn for_each_non_nil_char_table_run<F>(table: &Value, mut f: F)
where
    F: FnMut(Value, Value),
{
    if !is_char_table(table) {
        return;
    }

    for run in ct_effective_runs(table) {
        if run.value.is_nil() {
            continue;
        }
        f(run_key(run.start, run.end), run.value);
    }
}

const GNU_CHAR_TABLE_CONTENT_BLOCKS: i64 = 64;
const GNU_CHAR_TABLE_BLOCK_CHARS: i64 = 1 << 16;

fn raw_entry_overlaps(raw: &RawEntry, start: i64, end: i64) -> bool {
    raw.start <= end && raw.end >= start
}

fn local_raw_value_at(raws: &[RawEntry], ch: i64) -> Value {
    raws.iter()
        .rev()
        .find(|raw| ch >= raw.start && ch <= raw.end)
        .map(|raw| raw.value)
        .unwrap_or(Value::NIL)
}

fn local_uniform_value(raws: &[RawEntry], start: i64, end: i64) -> Option<Value> {
    if start > end {
        return Some(Value::NIL);
    }
    if !raws.iter().any(|raw| raw_entry_overlaps(raw, start, end)) {
        return Some(Value::NIL);
    }

    let mut boundaries = BTreeSet::new();
    let domain_end = MAX_CHAR.saturating_add(1);
    boundaries.insert(start.clamp(0, domain_end));
    boundaries.insert(end.saturating_add(1).clamp(0, domain_end));
    for raw in raws
        .iter()
        .filter(|raw| raw_entry_overlaps(raw, start, end))
    {
        boundaries.insert(raw.start.max(start).clamp(0, domain_end));
        boundaries.insert(raw.end.saturating_add(1).min(end.saturating_add(1)));
    }

    let mut value = None;
    for window in boundaries.into_iter().collect::<Vec<_>>().windows(2) {
        let segment_start = window[0];
        if segment_start > end || window[1] <= segment_start {
            continue;
        }
        let segment_value = local_raw_value_at(raws, segment_start);
        match value {
            Some(previous) if !eq_value(&previous, &segment_value) => return None,
            Some(_) => {}
            None => value = Some(segment_value),
        }
    }
    value.or(Some(Value::NIL))
}

fn make_sub_char_table_literal(depth: usize, min_char: i64, contents: Vec<Value>) -> Value {
    let mut values = Vec::with_capacity(contents.len() + 3);
    values.push(Value::symbol(SUB_CHAR_TABLE_TAG));
    values.push(Value::fixnum(depth as i64));
    values.push(Value::fixnum(min_char));
    values.extend(contents);
    Value::vector(values)
}

fn external_subtree_for_span(
    raws: &[RawEntry],
    depth: usize,
    min_char: i64,
    start: i64,
    end: i64,
) -> Value {
    if let Some(value) = local_uniform_value(raws, start, end) {
        return value;
    }

    let child_span = GNU_CHARTAB_CHARS[depth];
    let mut contents = Vec::with_capacity(GNU_CHARTAB_SIZE[depth]);
    for idx in 0..GNU_CHARTAB_SIZE[depth] {
        let child_start = min_char + idx as i64 * child_span;
        let child_end = (child_start + child_span - 1).min(MAX_CHAR);
        let child = if depth == 3 {
            local_uniform_value(raws, child_start, child_end).unwrap_or(Value::NIL)
        } else {
            external_subtree_for_span(raws, depth + 1, child_start, child_start, child_end)
        };
        contents.push(child);
    }
    make_sub_char_table_literal(depth, min_char, contents)
}

fn external_ascii_slot(raws: &[RawEntry]) -> Value {
    external_subtree_for_span(raws, 3, 0, 0, 127)
}

pub(crate) fn sub_char_table_external_slots(table: &Value) -> Option<(i64, i64, Vec<Value>)> {
    let (depth, min_char, contents) = sub_char_table_depth_min_contents(table)?;
    Some((depth as i64, min_char, contents))
}

pub(crate) fn char_table_external_slots(table: &Value) -> Option<Vec<Value>> {
    if !is_char_table(table) {
        return None;
    }

    if !table.is_vector() {
        return None;
    };
    let vec = table.as_vector_data().unwrap().clone();
    let raws = ct_collect_local_raw_entries(&vec);
    let extra_count = match vec[CT_EXTRA_COUNT].kind() {
        ValueKind::Fixnum(n) if n >= 0 => n as usize,
        _ => 0,
    };

    let mut slots = Vec::with_capacity(4 + GNU_CHAR_TABLE_CONTENT_BLOCKS as usize + extra_count);
    slots.push(vec[CT_DEFAULT]);
    slots.push(vec[CT_PARENT]);
    slots.push(vec[CT_SUBTYPE]);
    slots.push(external_ascii_slot(&raws));

    for idx in 0..GNU_CHAR_TABLE_CONTENT_BLOCKS {
        let start = idx * GNU_CHAR_TABLE_BLOCK_CHARS;
        let end = (start + GNU_CHAR_TABLE_BLOCK_CHARS - 1).min(MAX_CHAR);
        slots.push(external_subtree_for_span(&raws, 1, start, start, end));
    }

    for extra_idx in 0..extra_count {
        slots.push(vec[CT_EXTRA_START + extra_idx]);
    }

    Some(slots)
}

/// `(char-table-extra-slot TABLE N)` -- get extra slot N (0-based).
pub(crate) fn builtin_char_table_extra_slot(args: Vec<Value>) -> EvalResult {
    expect_args("char-table-extra-slot", &args, 2)?;
    let table = &args[0];
    let n = expect_int(&args[1])?;

    if !is_char_table(table) {
        return Err(wrong_type("char-table-p", table));
    }
    let v = table.as_vector_data().unwrap().clone();
    let extra_count = match v[CT_EXTRA_COUNT].kind() {
        ValueKind::Fixnum(c) => c,
        _ => 0,
    };

    if n < 0 || n >= extra_count {
        return Err(signal("args-out-of-range", vec![args[0], args[1]]));
    }

    Ok(v[CT_EXTRA_START + n as usize])
}

/// `(set-char-table-extra-slot TABLE N VALUE)` -- set extra slot N.
pub(crate) fn builtin_set_char_table_extra_slot(args: Vec<Value>) -> EvalResult {
    expect_args("set-char-table-extra-slot", &args, 3)?;
    let table = &args[0];
    let n = expect_int(&args[1])?;
    let value = &args[2];

    if !is_char_table(table) {
        return Err(wrong_type("char-table-p", table));
    }
    let v = table.as_vector_data().unwrap();
    let extra_count = match v[CT_EXTRA_COUNT].kind() {
        ValueKind::Fixnum(c) => c,
        _ => 0,
    };

    if n < 0 || n >= extra_count {
        return Err(signal("args-out-of-range", vec![args[0], args[1]]));
    }

    let slot_idx = CT_EXTRA_START + n as usize;
    let _ = table.set_vector_slot(slot_idx, *value);
    Ok(*value)
}

/// `(char-table-subtype TABLE)` -- return the sub-type symbol.
pub(crate) fn builtin_char_table_subtype(args: Vec<Value>) -> EvalResult {
    expect_args("char-table-subtype", &args, 1)?;
    let table = &args[0];
    if !is_char_table(table) {
        return Err(wrong_type("char-table-p", table));
    }
    let vec = table.as_vector_data().unwrap();
    Ok(vec[CT_SUBTYPE])
}

fn assq_cell_eq(key: Value, list: Value) -> Result<Value, Flow> {
    let mut cursor = list;
    loop {
        match cursor.kind() {
            ValueKind::Nil => return Ok(Value::NIL),
            ValueKind::Cons => {
                let entry = cursor.cons_car();
                if entry.is_cons() && eq_value(&entry.cons_car(), &key) {
                    return Ok(entry);
                }
                cursor = cursor.cons_cdr();
            }
            _ => return Err(wrong_type("listp", &list)),
        }
    }
}

fn char_code_property_cell(eval: &Context, prop: Value) -> Result<Value, Flow> {
    let alist = eval
        .obarray
        .symbol_value("char-code-property-alist")
        .copied()
        .unwrap_or(Value::NIL);
    assq_cell_eq(prop, alist)
}

/// `(unicode-property-table-internal PROP)`.
///
/// GNU's `chartab.c:uniprop_table` lazily loads `international/<file>` when
/// `char-code-property-alist` stores a string, then the public primitive returns
/// the alist cdr even for property tables whose decoder is Lisp rather than the
/// C fast-path decoder.  That distinction matters for `name`/`old-name`, whose
/// generated tables use byte-code decoder functions in extra slots.
pub(crate) fn builtin_unicode_property_table_internal(
    eval: &mut Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("unicode-property-table-internal", &args, 1)?;
    let prop = args[0];
    let mut cell = char_code_property_cell(eval, prop)?;
    if cell.is_nil() {
        return Ok(Value::NIL);
    }

    let table = cell.cons_cdr();
    if table.is_string() {
        let Some(file_name) = table.as_runtime_string_owned() else {
            return Ok(table);
        };
        let load_name = Value::string(format!("international/{file_name}"));
        let _ = crate::emacs_core::load::builtin_load_in_vm_runtime(
            eval,
            &[load_name, Value::T, Value::T, Value::T, Value::T],
        )?;
        cell = char_code_property_cell(eval, prop)?;
        if cell.is_nil() {
            return Ok(Value::NIL);
        }
    }

    Ok(cell.cons_cdr())
}

fn expect_character(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) if (0..=MAX_CHAR).contains(&n) => Ok(n),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *value],
        )),
    }
}

fn invalid_unicode_property_table() -> Flow {
    signal(
        "error",
        vec![Value::string("Invalid Unicode property table")],
    )
}

fn decode_uniprop_run_length(table: Value, value: Value) -> Value {
    let Some(ValueKind::Fixnum(index)) = Some(value.kind()) else {
        return value;
    };
    if index < 0 {
        return value;
    }
    let Some(value_table) = char_table_extra_slot_value(&table, 4) else {
        return value;
    };
    if !value_table.is_vector() {
        return value;
    }
    value_table
        .as_vector_data()
        .and_then(|values| values.get(index as usize).copied())
        .unwrap_or(value)
}

fn decode_unicode_property_map_value(table: Value, value: Value) -> Value {
    if !is_char_code_property_table(&table) {
        return value;
    }

    match char_table_extra_slot_value(&table, 1).map(|v| v.kind()) {
        Some(ValueKind::Fixnum(0)) => decode_uniprop_run_length(table, value),
        _ => value,
    }
}

/// `(get-unicode-property-internal CHAR-TABLE CH)`.
///
/// This mirrors GNU's C fast path for Unicode property tables: `CHAR-TABLE`
/// must have purpose `char-code-property-table` and five extra slots; a fixnum
/// decoder in extra slot 1 selects the built-in run-length decoder.
pub(crate) fn builtin_get_unicode_property_internal(args: Vec<Value>) -> EvalResult {
    expect_args("get-unicode-property-internal", &args, 2)?;
    let table = args[0];
    let ch = expect_character(&args[1])?;

    if !is_char_table(&table) {
        return Err(wrong_type("char-table-p", &table));
    }
    if !is_char_code_property_table(&table) {
        return Err(invalid_unicode_property_table());
    }

    let decoder = char_table_extra_slot_value(&table, 1).unwrap_or(Value::NIL);
    let value = ct_lookup(&table, ch)?;
    match decoder.kind() {
        ValueKind::Fixnum(0) => Ok(decode_uniprop_run_length(table, value)),
        ValueKind::Nil => Ok(value),
        ValueKind::Fixnum(_) => Err(invalid_unicode_property_table()),
        _ => Ok(value),
    }
}

// ---------------------------------------------------------------------------
// Bool-vector builtins
// ---------------------------------------------------------------------------

/// `(make-bool-vector LENGTH INIT)` -- create a bool vector of LENGTH bits,
/// each initialized to INIT (nil or non-nil).
pub(crate) fn builtin_make_bool_vector(args: Vec<Value>) -> EvalResult {
    expect_args("make-bool-vector", &args, 2)?;
    let length = expect_int(&args[0])?;
    if length < 0 {
        return Err(signal("args-out-of-range", vec![args[0]]));
    }
    let init_val = if args[1].is_truthy() {
        Value::fixnum(1)
    } else {
        Value::fixnum(0)
    };
    let len = length as usize;
    let mut vec = Vec::with_capacity(2 + len);
    vec.push(Value::symbol(BOOL_VECTOR_TAG));
    vec.push(Value::fixnum(length));
    for _ in 0..len {
        vec.push(init_val);
    }
    Ok(Value::vector(vec))
}

/// `(bool-vector &rest OBJECTS)` -- create a bool-vector from OBJECTS
/// truthiness.
pub(crate) fn builtin_bool_vector(args: Vec<Value>) -> EvalResult {
    let bits: Vec<bool> = args.into_iter().map(|v| v.is_truthy()).collect();
    Ok(bv_from_bits(&bits))
}

/// `(bool-vector-p OBJ)` -- return t if OBJ is a bool-vector.
pub(crate) fn builtin_bool_vector_p(args: Vec<Value>) -> EvalResult {
    expect_args("bool-vector-p", &args, 1)?;
    Ok(Value::bool_val(is_bool_vector(&args[0])))
}

/// Helper: extract a bool-vector's length.
fn bv_length(vec: &[Value]) -> i64 {
    match vec[BV_SIZE].kind() {
        ValueKind::Fixnum(n) => n,
        _ => 0,
    }
}

/// Helper: extract the bits of a bool-vector as a `Vec<bool>`.
fn bv_bits(vec: &[Value]) -> Vec<bool> {
    let len = bv_length(vec) as usize;
    let mut bits = Vec::with_capacity(len);
    for i in 0..len {
        let v = &vec[2 + i];
        bits.push(v.as_fixnum().map_or(false, |n| n != 0));
    }
    bits
}

/// `(bool-vector-count-population BV)` -- count the number of true values.
pub(crate) fn builtin_bool_vector_count_population(args: Vec<Value>) -> EvalResult {
    expect_args("bool-vector-count-population", &args, 1)?;
    let (bits, _len) = extract_bv_bits(&args[0])?;
    let count = bits.iter().filter(|&&b| b).count();
    Ok(Value::fixnum(count as i64))
}

fn extract_bv_bits(value: &Value) -> Result<(Vec<bool>, i64), Flow> {
    if !is_bool_vector(value) {
        return Err(wrong_type("bool-vector-p", value));
    }
    let vec = value.as_vector_data().unwrap().clone();
    let len = bv_length(&vec);
    let bits = bv_bits(&vec);
    Ok((bits, len))
}

/// Build a bool-vector `Value` from a slice of bools.
fn bv_from_bits(bits: &[bool]) -> Value {
    let len = bits.len();
    let mut vec = Vec::with_capacity(2 + len);
    vec.push(Value::symbol(BOOL_VECTOR_TAG));
    vec.push(Value::fixnum(len as i64));
    for &b in bits {
        vec.push(Value::fixnum(if b { 1 } else { 0 }));
    }
    Value::vector(vec)
}

/// `(bool-vector-intersection A B &optional C)` -- bitwise AND.
/// If C is provided, store result in C and return C; otherwise return a new
/// bool-vector.
pub(crate) fn builtin_bool_vector_intersection(args: Vec<Value>) -> EvalResult {
    expect_min_args("bool-vector-intersection", &args, 2)?;
    expect_max_args("bool-vector-intersection", &args, 3)?;
    let (bits_a, len_a) = extract_bv_bits(&args[0])?;
    let (bits_b, len_b) = extract_bv_bits(&args[1])?;
    if len_a != len_b {
        return Err(signal(
            "wrong-length-argument",
            vec![Value::fixnum(len_a), Value::fixnum(len_b)],
        ));
    }
    let result_bits: Vec<bool> = bits_a
        .iter()
        .zip(bits_b.iter())
        .map(|(&a, &b)| a && b)
        .collect();

    if args.len() == 3 {
        store_bv_result_with_expected_lengths(&args[2], &result_bits, &[len_a, len_b])?;
        Ok(args[2])
    } else {
        Ok(bv_from_bits(&result_bits))
    }
}

/// `(bool-vector-union A B &optional C)` -- bitwise OR.
pub(crate) fn builtin_bool_vector_union(args: Vec<Value>) -> EvalResult {
    expect_min_args("bool-vector-union", &args, 2)?;
    expect_max_args("bool-vector-union", &args, 3)?;
    let (bits_a, len_a) = extract_bv_bits(&args[0])?;
    let (bits_b, len_b) = extract_bv_bits(&args[1])?;
    if len_a != len_b {
        return Err(signal(
            "wrong-length-argument",
            vec![Value::fixnum(len_a), Value::fixnum(len_b)],
        ));
    }
    let result_bits: Vec<bool> = bits_a
        .iter()
        .zip(bits_b.iter())
        .map(|(&a, &b)| a || b)
        .collect();

    if args.len() == 3 {
        store_bv_result_with_expected_lengths(&args[2], &result_bits, &[len_a, len_b])?;
        Ok(args[2])
    } else {
        Ok(bv_from_bits(&result_bits))
    }
}

/// `(bool-vector-exclusive-or A B &optional C)` -- bitwise XOR.
pub(crate) fn builtin_bool_vector_exclusive_or(args: Vec<Value>) -> EvalResult {
    expect_min_args("bool-vector-exclusive-or", &args, 2)?;
    expect_max_args("bool-vector-exclusive-or", &args, 3)?;
    let (bits_a, len_a) = extract_bv_bits(&args[0])?;
    let (bits_b, len_b) = extract_bv_bits(&args[1])?;
    if len_a != len_b {
        return Err(signal(
            "wrong-length-argument",
            vec![Value::fixnum(len_a), Value::fixnum(len_b)],
        ));
    }
    let result_bits: Vec<bool> = bits_a
        .iter()
        .zip(bits_b.iter())
        .map(|(&a, &b)| a ^ b)
        .collect();

    if args.len() == 3 {
        store_bv_result_with_expected_lengths(&args[2], &result_bits, &[len_a, len_b])?;
        Ok(args[2])
    } else {
        Ok(bv_from_bits(&result_bits))
    }
}

/// `(bool-vector-not A &optional B)` -- bitwise NOT.
///
/// If B is provided, store result in B and return B; otherwise return a new
/// bool-vector.
pub(crate) fn builtin_bool_vector_not(args: Vec<Value>) -> EvalResult {
    expect_min_args("bool-vector-not", &args, 1)?;
    expect_max_args("bool-vector-not", &args, 2)?;
    let (bits, len_a) = extract_bv_bits(&args[0])?;
    let result_bits: Vec<bool> = bits.into_iter().map(|b| !b).collect();
    if args.len() == 2 {
        store_bv_result_with_expected_lengths(&args[1], &result_bits, &[len_a])?;
        Ok(args[1])
    } else {
        Ok(bv_from_bits(&result_bits))
    }
}

/// `(bool-vector-set-difference A B &optional C)` -- `A & (not B)`.
pub(crate) fn builtin_bool_vector_set_difference(args: Vec<Value>) -> EvalResult {
    expect_min_args("bool-vector-set-difference", &args, 2)?;
    expect_max_args("bool-vector-set-difference", &args, 3)?;
    let (bits_a, len_a) = extract_bv_bits(&args[0])?;
    let (bits_b, len_b) = extract_bv_bits(&args[1])?;
    if len_a != len_b {
        return Err(signal(
            "wrong-length-argument",
            vec![Value::fixnum(len_a), Value::fixnum(len_b)],
        ));
    }
    let result_bits: Vec<bool> = bits_a
        .iter()
        .zip(bits_b.iter())
        .map(|(&a, &b)| a && !b)
        .collect();
    if args.len() == 3 {
        store_bv_result_with_expected_lengths(&args[2], &result_bits, &[len_a, len_b])?;
        Ok(args[2])
    } else {
        Ok(bv_from_bits(&result_bits))
    }
}

/// `(bool-vector-count-consecutive BV BOOL START)` -- count matching bits from
/// START until the first non-matching bit or the end.
pub(crate) fn builtin_bool_vector_count_consecutive(args: Vec<Value>) -> EvalResult {
    expect_args("bool-vector-count-consecutive", &args, 3)?;
    let (bits, len) = extract_bv_bits(&args[0])?;
    let target = args[1].is_truthy();
    let start = expect_wholenump(&args[2])?;
    if start > len {
        return Err(signal(
            "args-out-of-range",
            vec![args[0], Value::fixnum(start)],
        ));
    }
    let mut count = 0usize;
    for bit in bits.iter().skip(start as usize) {
        if *bit != target {
            break;
        }
        count += 1;
    }
    Ok(Value::fixnum(count as i64))
}

/// `(bool-vector-subsetp A B)` -- return t if every true bit in A is also true
/// in B.
pub(crate) fn builtin_bool_vector_subsetp(args: Vec<Value>) -> EvalResult {
    expect_args("bool-vector-subsetp", &args, 2)?;
    let (bits_a, len_a) = extract_bv_bits(&args[0])?;
    let (bits_b, len_b) = extract_bv_bits(&args[1])?;
    if len_a != len_b {
        return Err(signal(
            "wrong-length-argument",
            vec![
                Value::fixnum(len_a),
                Value::fixnum(len_b),
                Value::fixnum(len_b),
            ],
        ));
    }
    let is_subset = bits_a.iter().zip(bits_b.iter()).all(|(&a, &b)| !a || b);
    Ok(Value::bool_val(is_subset))
}

/// Store bits into an existing bool-vector (for the optional dest argument).
fn store_bv_result_with_expected_lengths(
    dest: &Value,
    bits: &[bool],
    expected_lengths: &[i64],
) -> Result<(), Flow> {
    if !is_bool_vector(dest) {
        return Err(wrong_type("bool-vector-p", dest));
    }
    let v = dest.as_vector_data().unwrap().clone();
    let len = bv_length(&v) as usize;
    if len != bits.len() {
        let mut payload: Vec<Value> = expected_lengths
            .iter()
            .copied()
            .map(Value::fixnum)
            .collect();
        payload.push(Value::fixnum(len as i64));
        return Err(signal("wrong-length-argument", payload));
    }
    let mut slots = dest
        .as_vector_data()
        .map(|items| items.to_vec())
        .unwrap_or_default();
    for (i, &b) in bits.iter().enumerate() {
        slots[2 + i] = Value::fixnum(if b { 1 } else { 0 });
    }
    let _ = dest.replace_vector_data(slots);
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "chartable_test.rs"]
mod tests;
