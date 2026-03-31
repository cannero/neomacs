//! Char-table and bool-vector types.
//!
//! Since we cannot add new `Value` variants, these types are represented using
//! existing `Value` infrastructure:
//!
//! - **Char-table**: A `Value::Vector` whose first element is the tag symbol
//!   `--char-table--`.  The layout is:
//!   `[--char-table-- DEFAULT PARENT SUB-TYPE EXTRA-SLOTS-COUNT ...EXTRA-SLOTS... ...DATA-PAIRS...]`
//!   where DATA-PAIRS are stored as consecutive `(char-code, value)` pairs
//!   starting after the extra slots.  For efficiency, lookups walk the data
//!   pairs linearly (fine for the typical sparse char-table).
//!
//! - **Bool-vector**: A `Value::Vector` whose first element is the tag symbol
//!   `--bool-vector--`.  The layout is:
//!   `[--bool-vector-- SIZE ...BITS...]`
//!   where SIZE is `Value::fixnum(length)` and each subsequent element is
//!   `Value::fixnum(0)` or `Value::fixnum(1)`.

use super::error::{EvalResult, Flow, signal};
use super::eval::Context;
use super::intern::resolve_sym;
use super::value::*;

// ---------------------------------------------------------------------------
// Tag constants
// ---------------------------------------------------------------------------

const CHAR_TABLE_TAG: &str = "--char-table--";
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
            && vec[0].as_symbol_id().map_or(false, |id| resolve_sym(id) == CHAR_TABLE_TAG)
    } else {
        false
    }
}

/// Return `true` if `v` is a bool-vector (tagged vector).
pub fn is_bool_vector(v: &Value) -> bool {
    if v.is_vector() {
        let vec = v.as_vector_data().unwrap();
        vec.len() >= 2
            && vec[0].as_symbol_id().map_or(false, |id| resolve_sym(id) == BOOL_VECTOR_TAG)
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
    if vec.len() < 2 || !vec[0].as_symbol_id().map_or(false, |id| resolve_sym(id) == BOOL_VECTOR_TAG)
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
        && vec[0].as_symbol_id().map_or(false, |id| resolve_sym(id) == CHAR_TABLE_TAG)
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
        ValueKind::Char(c) => Ok(c as i64),
        _other => Err(wrong_type("integerp", value)),
    }
}

/// Extract a non-negative integer (for index-like args), signaling with
/// `wholenump` on any mismatch.
fn expect_wholenump(value: &Value) -> Result<i64, Flow> {
    let n = match value.kind() {
        ValueKind::Fixnum(n) => n,
        ValueKind::Char(c) => c as i64,
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
    let extra_count = match vec[CT_EXTRA_COUNT].kind() {
        ValueKind::Fixnum(n) => n as usize,
        _ => 0,
    };
    CT_EXTRA_START + extra_count
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
        default,              // CT_DEFAULT
        Value::NIL,           // CT_PARENT
        sub_type,             // CT_SUBTYPE
        Value::fixnum(n_extras), // CT_EXTRA_COUNT
    ];
    // Allocate extra slots initialised to nil.
    for _ in 0..n_extras {
        vec.push(Value::NIL);
    }
    Value::vector(vec)
}

/// Set a single character entry in a char-table Value (for bootstrap code).
/// Panics if `table` is not a char-table Vector.
pub fn ct_set_single(table: &Value, ch: i64, value: Value) {
    if table.is_vector() {
        let vec = table.as_vector_data_mut().unwrap();
        ct_set_char(vec, ch, value);
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
        ValueKind::Fixnum(_) | ValueKind::Char(_) => {
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

    *table.as_vector_data_mut().unwrap() = vec;

    Ok(*value)
}

/// Set a single character entry in the char-table's data pairs.
fn ct_set_char(vec: &mut Vec<Value>, ch: i64, value: Value) {
    vec.push(Value::fixnum(ch));
    vec.push(value);
}

/// Set a range entry in the char-table's data pairs.
/// The range is stored as a `Cons(min . max)` key.
fn ct_set_range(vec: &mut Vec<Value>, min: i64, max: i64, value: Value) {
    vec.push(Value::cons(Value::fixnum(min), Value::fixnum(max)));
    vec.push(value);
}

/// Look up a single character in the data pairs (no parent/default fallback).
/// The last assignment that covers the character wins, matching GNU Emacs
/// `set-char-table-range` overwrite semantics for both single-char and range
/// entries.
fn ct_get_char(vec: &[Value], ch: i64) -> Option<Value> {
    let start = ct_data_start(vec);
    let mut i = start;
    let mut match_value: Option<Value> = None;
    while i + 1 < vec.len() {
        match vec[i].kind() {
            ValueKind::Fixnum(existing) => {
                if existing == ch {
                    match_value = Some(vec[i + 1]);
                }
            }
            ValueKind::Cons => {
                // Range entry: key is (MIN . MAX)
                let pair_car = vec[i].cons_car();
                let pair_cdr = vec[i].cons_cdr();
                if let (Some(min), Some(max)) = (pair_car.as_fixnum(), pair_cdr.as_fixnum()) {
                    if ch >= min && ch <= max {
                        match_value = Some(vec[i + 1]);
                    }
                }
            }
            _ => {}
        }
        i += 2;
    }
    match_value
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
        ValueKind::Fixnum(_) | ValueKind::Char(_) => {
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
    let vec = table.as_vector_data().unwrap().clone();

    if let Some(val) = ct_get_char(&vec, ch) {
        if !val.is_nil() {
            return Ok(val);
        }
    }

    let default = vec[CT_DEFAULT];
    let parent = vec[CT_PARENT];

    if !default.is_nil() {
        Ok(default)
    } else if is_char_table(&parent) {
        ct_lookup(&parent, ch)
    } else {
        Ok(Value::NIL)
    }
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

/// GNU `char-table-ref-and-range`-style helper used by subsystems that need
/// the effective value together with the maximal contiguous run covering `ch`.
pub(crate) fn char_table_ref_and_range(table: &Value, ch: i64) -> Result<(Value, i64, i64), Flow> {
    ct_lookup_and_range(table, ch)
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

    table.as_vector_data_mut().unwrap()[CT_PARENT] = *parent;
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
        f(key, run.value)?;
    }
    Ok(())
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

fn ct_collect_raw_entries(vec: &[Value]) -> Vec<RawEntry> {
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
                    raws.push(RawEntry {
                        start: min,
                        end: max,
                        value: vec[i + 1],
                    });
                }
            }
            _ => {}
        }
        i += 2;
    }
    raws
}

fn ct_local_value_at(raws: &[RawEntry], ch: i64) -> Option<Value> {
    let mut value = None;
    for raw in raws {
        if ch >= raw.start && ch <= raw.end {
            value = Some(raw.value);
        }
    }
    value
}

fn ct_effective_value_at(
    raws: &[RawEntry],
    default: Value,
    parent_runs: &[EffectiveRun],
    ch: i64,
) -> Value {
    if let Some(local) = ct_local_value_at(raws, ch) {
        if !local.is_nil() {
            return local;
        }
    }
    if !default.is_nil() {
        return default;
    }
    effective_runs_value_at(parent_runs, ch).unwrap_or(Value::NIL)
}

fn effective_runs_value_at(entries: &[EffectiveRun], ch: i64) -> Option<Value> {
    for run in entries {
        if ch >= run.start && ch <= run.end {
            return Some(run.value);
        }
    }
    None
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
    let raws = ct_collect_raw_entries(&vec);
    let default = vec[CT_DEFAULT];
    let parent = vec[CT_PARENT];
    let parent_runs = if is_char_table(&parent) {
        ct_effective_runs(&parent)
    } else {
        vec![EffectiveRun {
            start: 0,
            end: MAX_CHAR,
            value: Value::NIL,
        }]
    };

    let mut boundaries = std::collections::BTreeSet::new();
    boundaries.insert(0);
    boundaries.insert(MAX_CHAR.saturating_add(1));
    for raw in &raws {
        boundaries.insert(raw.start);
        boundaries.insert(raw.end.saturating_add(1).min(MAX_CHAR.saturating_add(1)));
    }
    for run in &parent_runs {
        boundaries.insert(run.start);
        boundaries.insert(run.end.saturating_add(1).min(MAX_CHAR.saturating_add(1)));
    }

    let boundary_vec = boundaries.into_iter().collect::<Vec<_>>();
    let mut runs: Vec<EffectiveRun> = Vec::new();

    for window in boundary_vec.windows(2) {
        let start = window[0];
        let end_exclusive = window[1];
        if start > MAX_CHAR || end_exclusive <= start {
            continue;
        }
        let end = end_exclusive.saturating_sub(1).min(MAX_CHAR);
        let value = ct_effective_value_at(&raws, default, &parent_runs, start);
        match runs.last_mut() {
            Some(last) if last.value == value && start == last.end.saturating_add(1) => {
                last.end = end;
            }
            _ => runs.push(EffectiveRun { start, end, value }),
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

fn uniform_run_value(runs: &[EffectiveRun], start: i64, end: i64) -> Option<Value> {
    runs.iter()
        .find(|run| start >= run.start && end <= run.end)
        .map(|run| run.value)
}

pub(crate) fn char_table_external_slots(table: &Value) -> Option<Vec<Value>> {
    if !is_char_table(table) {
        return None;
    }

    if !table.is_vector() {
        return None;
    };
    let vec = table.as_vector_data().unwrap().clone();
    let runs = ct_effective_runs(table);
    let extra_count = match vec[CT_EXTRA_COUNT].kind() {
        ValueKind::Fixnum(n) if n >= 0 => n as usize,
        _ => 0,
    };

    let mut slots = Vec::with_capacity(4 + GNU_CHAR_TABLE_CONTENT_BLOCKS as usize + extra_count);
    slots.push(vec[CT_DEFAULT]);
    slots.push(vec[CT_PARENT]);
    slots.push(vec[CT_SUBTYPE]);
    slots.push(uniform_run_value(&runs, 0, 127).unwrap_or(Value::NIL));

    for idx in 0..GNU_CHAR_TABLE_CONTENT_BLOCKS {
        let start = idx * GNU_CHAR_TABLE_BLOCK_CHARS;
        let end = (start + GNU_CHAR_TABLE_BLOCK_CHARS - 1).min(MAX_CHAR);
        slots.push(uniform_run_value(&runs, start, end).unwrap_or(Value::NIL));
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

    table.as_vector_data_mut().unwrap()[CT_EXTRA_START + n as usize] = *value;
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
            vec![Value::fixnum(len_a), Value::fixnum(len_b), Value::fixnum(len_b)],
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
        let mut payload: Vec<Value> = expected_lengths.iter().copied().map(Value::fixnum).collect();
        payload.push(Value::fixnum(len as i64));
        return Err(signal("wrong-length-argument", payload));
    }
    let v_mut = dest.as_vector_data_mut().unwrap();
    for (i, &b) in bits.iter().enumerate() {
        v_mut[2 + i] = Value::fixnum(if b { 1 } else { 0 });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "chartable_test.rs"]
mod tests;
