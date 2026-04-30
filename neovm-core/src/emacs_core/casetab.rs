//! Case-table support for Emacs case conversion.
//!
//! Provides a `CaseTable` struct holding upcase/downcase/canonicalize/equivalences
//! mappings, a `CaseTableManager` with standard ASCII case tables pre-initialized,
//! and pure builtins for case-table predicates and character case conversion.

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::value::*;
use std::cell::RefCell;
use std::collections::HashMap;

thread_local! {
    static STANDARD_CASE_TABLE_OBJECT: RefCell<Option<Value>> = const { RefCell::new(None) };
}

/// Clear cached thread-local case table (must be called when heap changes).
pub fn reset_casetab_thread_locals() {
    STANDARD_CASE_TABLE_OBJECT.with(|slot| *slot.borrow_mut() = None);
}

/// Collect GC roots from the cached case table.
pub fn collect_casetab_gc_roots(roots: &mut Vec<Value>) {
    STANDARD_CASE_TABLE_OBJECT.with(|slot| {
        if let Some(v) = *slot.borrow() {
            roots.push(v);
        }
    });
}

// ---------------------------------------------------------------------------
// CaseTable
// ---------------------------------------------------------------------------

/// A case table holding four character mappings.
#[derive(Clone, Debug)]
pub struct CaseTable {
    /// Maps lowercase characters to their uppercase equivalents.
    pub upcase: HashMap<char, char>,
    /// Maps uppercase characters to their lowercase equivalents.
    pub downcase: HashMap<char, char>,
    /// Maps characters to a canonical form (used for case-insensitive comparison).
    pub canonicalize: HashMap<char, char>,
    /// Maps characters to the next character in the equivalence class cycle.
    pub equivalences: HashMap<char, char>,
}

impl CaseTable {
    /// Create an empty case table with no mappings.
    pub fn empty() -> Self {
        Self {
            upcase: HashMap::new(),
            downcase: HashMap::new(),
            canonicalize: HashMap::new(),
            equivalences: HashMap::new(),
        }
    }

    /// Create the standard ASCII case table (a-z <-> A-Z).
    pub fn standard_ascii() -> Self {
        let mut upcase = HashMap::new();
        let mut downcase = HashMap::new();
        let mut canonicalize = HashMap::new();
        let mut equivalences = HashMap::new();

        for lower in b'a'..=b'z' {
            let upper = lower - b'a' + b'A';
            let lc = lower as char;
            let uc = upper as char;

            // Upcase: lowercase -> uppercase
            upcase.insert(lc, uc);
            // Downcase: uppercase -> lowercase
            downcase.insert(uc, lc);

            // Canonicalize: both map to lowercase
            canonicalize.insert(uc, lc);
            canonicalize.insert(lc, lc);

            // Equivalences: cycle upper -> lower -> upper
            equivalences.insert(uc, lc);
            equivalences.insert(lc, uc);
        }

        Self {
            upcase,
            downcase,
            canonicalize,
            equivalences,
        }
    }
}

// ---------------------------------------------------------------------------
// CaseTableManager
// ---------------------------------------------------------------------------

/// Manages case tables, providing standard ASCII case conversion by default.
#[derive(Clone, Debug)]
pub struct CaseTableManager {
    /// The standard (immutable) case table.
    standard: CaseTable,
    /// The current buffer-local case table.
    current: CaseTable,
}

impl CaseTableManager {
    /// Create a new manager with the standard ASCII case table.
    pub fn new() -> Self {
        let table = CaseTable::standard_ascii();
        Self {
            standard: table.clone(),
            current: table,
        }
    }

    /// Convert a character to uppercase using the current case table.
    /// Returns the character unchanged if no upcase mapping exists.
    pub fn upcase_char(&self, c: char) -> char {
        *self.current.upcase.get(&c).unwrap_or(&c)
    }

    /// Convert a character to lowercase using the current case table.
    /// Returns the character unchanged if no downcase mapping exists.
    pub fn downcase_char(&self, c: char) -> char {
        *self.current.downcase.get(&c).unwrap_or(&c)
    }

    /// Convert an entire string to uppercase.
    pub fn upcase_string(&self, s: &str) -> String {
        s.chars().map(|c| self.upcase_char(c)).collect()
    }

    /// Convert an entire string to lowercase.
    pub fn downcase_string(&self, s: &str) -> String {
        s.chars().map(|c| self.downcase_char(c)).collect()
    }

    /// Return a reference to the standard case table.
    pub fn standard_table(&self) -> &CaseTable {
        &self.standard
    }

    /// Return a reference to the current case table.
    pub fn current_table(&self) -> &CaseTable {
        &self.current
    }

    /// Set the current case table.
    pub fn set_current(&mut self, table: CaseTable) {
        self.current = table;
    }

    /// Set the standard case table.
    pub fn set_standard(&mut self, table: CaseTable) {
        self.standard = table;
    }
}

impl Default for CaseTableManager {
    fn default() -> Self {
        Self::new()
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

/// Signal `wrong-type-argument` with a predicate name.
fn wrong_type(pred: &str, got: &Value) -> Flow {
    signal("wrong-type-argument", vec![Value::symbol(pred), *got])
}

/// Extract a character from a Value (Int or Char), signal otherwise.
fn expect_char(value: &Value) -> Result<char, Flow> {
    match value.kind() {
        ValueKind::Fixnum(c) => super::builtins::character_code_to_rust_char(c).ok_or_else(|| {
            signal(
                "error",
                vec![Value::string("Invalid character code"), *value],
            )
        }),
        other => Err(wrong_type("characterp", value)),
    }
}

// ---------------------------------------------------------------------------
// Builtins
// ---------------------------------------------------------------------------

/// `(case-table-p OBJ)` -- return t if OBJ is a case table.
///
/// A case table is a char-table with `case-table` sub-type and 3 extra slots
/// (upcase, canonicalize, equivalences).
pub(crate) fn builtin_case_table_p(args: Vec<Value>) -> EvalResult {
    expect_args("case-table-p", &args, 1)?;
    Ok(Value::bool_val(is_case_table(&args[0])))
}

/// `(downcase CHAR)` -- convert a character to lowercase.
///
/// If the argument is an integer or character, returns the lowercase version
/// using the standard ASCII case table. Characters outside A-Z are returned
/// unchanged.
#[cfg(test)]
pub(crate) fn builtin_downcase_char(args: Vec<Value>) -> EvalResult {
    expect_args("downcase", &args, 1)?;
    let c = expect_char(&args[0])?;
    let manager = CaseTableManager::new();
    let result = manager.downcase_char(c);
    Ok(Value::fixnum(result as i64))
}

// ---------------------------------------------------------------------------
// Case-table as char-table
// ---------------------------------------------------------------------------

// Char-table vector layout constants (mirrored from chartable.rs).
const CT_CHAR_TABLE_TAG: &str = "--char-table--";
const CT_SUBTYPE: usize = 3;
const CT_EXTRA_COUNT: usize = 4;
const CT_EXTRA_START: usize = 5;
// Phase 10D holdout 5: per-buffer case-table char-table now lives in
// `Buffer::slots[BUFFER_SLOT_CASE_TABLE]`. NeoMacs collapses GNU's four
// separate `downcase_table_` / `upcase_table_` / `case_canon_table_` /
// `case_eqv_table_` BVAR slots (`buffer.h:408-417`) into a single
// downcase char-table whose extras[0..2] hold the upcase / canonicalize /
// equivalence subsidiary tables — the same value shape `Fcurrent_case_table`
// returns. The slot is non-Lisp-visible (`install_as_forwarder: false`),
// always-local (`local_flags_idx == -1`, matching GNU `buffer.c:4731-4734`).
// Reads/writes happen through `(current-case-table)` / `(set-case-table)`.
const STANDARD_CASE_TABLE_SYMBOL: &str = "neovm--standard-case-table-object";

/// Build a char-table vector with the given subtype, extra slots, default, and data pairs.
fn build_char_table(
    subtype: &str,
    extra_slots: &[Value],
    default: Value,
    data_pairs: &[(i64, Value)],
) -> Value {
    let extra_count = extra_slots.len();
    let mut vec = Vec::with_capacity(CT_EXTRA_START + extra_count + data_pairs.len() * 2);
    vec.push(Value::symbol(CT_CHAR_TABLE_TAG)); // tag
    vec.push(default); // CT_DEFAULT
    vec.push(Value::NIL); // CT_PARENT
    vec.push(Value::symbol(subtype)); // CT_SUBTYPE
    vec.push(Value::fixnum(extra_count as i64)); // CT_EXTRA_COUNT
    for slot in extra_slots {
        vec.push(*slot);
    }
    for &(ch, val) in data_pairs {
        vec.push(Value::fixnum(ch));
        vec.push(val);
    }
    Value::vector(vec)
}

/// Create the standard case table: a char-table with `case-table` subtype,
/// 3 extra slots (upcase, canonicalize, equivalences), and ASCII case mappings.
fn make_standard_case_table_value() -> Value {
    let mut downcase_pairs = Vec::with_capacity(128);
    let mut upcase_pairs = Vec::with_capacity(128);
    let mut canon_pairs = Vec::with_capacity(128);
    let mut eqv_pairs = Vec::with_capacity(128);

    for i in 0i64..128 {
        // Downcase: A-Z -> a-z, others -> themselves
        let down = if (b'A' as i64..=b'Z' as i64).contains(&i) {
            i + (b'a' as i64 - b'A' as i64)
        } else {
            i
        };
        downcase_pairs.push((i, Value::fixnum(down)));

        // Upcase: a-z -> A-Z, others -> themselves
        let up = if (b'a' as i64..=b'z' as i64).contains(&i) {
            i + (b'A' as i64 - b'a' as i64)
        } else {
            i
        };
        upcase_pairs.push((i, Value::fixnum(up)));

        // Canonicalize: same as downcase
        canon_pairs.push((i, Value::fixnum(down)));

        // Equivalences: A -> a, a -> A, others -> themselves
        let eqv = if (b'A' as i64..=b'Z' as i64).contains(&i) {
            i + (b'a' as i64 - b'A' as i64)
        } else if (b'a' as i64..=b'z' as i64).contains(&i) {
            i + (b'A' as i64 - b'a' as i64)
        } else {
            i
        };
        eqv_pairs.push((i, Value::fixnum(eqv)));
    }

    // Build subsidiary char-tables (no extra slots)
    let upcase_ct = build_char_table("case-table", &[], Value::NIL, &upcase_pairs);
    let canon_ct = build_char_table("case-table", &[], Value::NIL, &canon_pairs);
    let eqv_ct = build_char_table("case-table", &[], Value::NIL, &eqv_pairs);

    // Build the main downcase char-table with 3 extra slots
    build_char_table(
        "case-table",
        &[upcase_ct, canon_ct, eqv_ct],
        Value::NIL,
        &downcase_pairs,
    )
}

/// Create an empty case-table char-table (valid for `case-table-p`).
fn make_case_table_value() -> Value {
    build_char_table(
        "case-table",
        &[Value::NIL, Value::NIL, Value::NIL],
        Value::NIL,
        &[],
    )
}

fn ensure_standard_case_table_object() -> EvalResult {
    STANDARD_CASE_TABLE_OBJECT.with(|slot| {
        if let Some(value) = slot.borrow().as_ref() {
            return Ok(*value);
        }
        let table = make_standard_case_table_value();
        *slot.borrow_mut() = Some(table);
        Ok(table)
    })
}

/// `(current-case-table)` -- evaluator-backed current buffer case table object.
pub(crate) fn builtin_current_case_table(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("current-case-table", &args, 0)?;
    current_case_table_for_buffer_in_state(&mut ctx.obarray, &mut ctx.buffers)
}

/// `(standard-case-table)` -- evaluator-backed standard case table object.
pub(crate) fn builtin_standard_case_table(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("standard-case-table", &args, 0)?;
    ensure_standard_case_table_object_in_state(&mut ctx.obarray)
}

/// `(set-case-table TABLE)` -- evaluator-backed current buffer case table set.
pub(crate) fn builtin_set_case_table(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-case-table", &args, 1)?;
    if !is_case_table(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("case-table-p"), args[0]],
        ));
    }
    let table = args[0];
    let _ = ensure_standard_case_table_object_in_state(&mut ctx.obarray)?;
    set_current_case_table_for_buffer_in_state(&mut ctx.buffers, table)?;
    Ok(table)
}

/// `(set-standard-case-table TABLE)` -- evaluator-backed standard table set.
pub(crate) fn builtin_set_standard_case_table(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-standard-case-table", &args, 1)?;
    if !is_case_table(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("case-table-p"), args[0]],
        ));
    }
    STANDARD_CASE_TABLE_OBJECT.with(|slot| {
        *slot.borrow_mut() = Some(args[0]);
    });
    let table = args[0];
    ctx.obarray
        .set_symbol_value(STANDARD_CASE_TABLE_SYMBOL, table);
    Ok(table)
}

fn ensure_standard_case_table_object_in_state(obarray: &mut super::symbol::Obarray) -> EvalResult {
    if let Some(value) = obarray.symbol_value(STANDARD_CASE_TABLE_SYMBOL).cloned() {
        if is_case_table(&value) {
            return Ok(value);
        }
    }
    let table = make_standard_case_table_value();
    obarray.set_symbol_value(STANDARD_CASE_TABLE_SYMBOL, table);
    Ok(table)
}

fn current_case_table_for_buffer_in_state(
    obarray: &mut super::symbol::Obarray,
    buffers: &mut crate::buffer::BufferManager,
) -> Result<Value, Flow> {
    use crate::buffer::buffer::BUFFER_SLOT_CASE_TABLE;
    let fallback = ensure_standard_case_table_object_in_state(obarray)?;
    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = buffers
        .get_mut(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    // Mirrors GNU `Fcurrent_case_table` (`casetab.c:65-72`):
    //     return BVAR (current_buffer, downcase_table);
    let value = buf.slots[BUFFER_SLOT_CASE_TABLE];
    if is_case_table(&value) {
        return Ok(value);
    }

    // Slot unset or invalid: seed from the standard table —
    // matches GNU `reset_buffer` cloning the standard tables
    // into a fresh buffer (`buffer.c:1149-1157`).
    buf.slots[BUFFER_SLOT_CASE_TABLE] = fallback;
    Ok(fallback)
}

pub(crate) fn sync_current_buffer_case_table_state(
    ctx: &mut crate::emacs_core::eval::Context,
) -> Result<(), Flow> {
    let _ = current_case_table_for_buffer_in_state(&mut ctx.obarray, &mut ctx.buffers)?;
    Ok(())
}

fn set_current_case_table_for_buffer_in_state(
    buffers: &mut crate::buffer::BufferManager,
    table: Value,
) -> Result<(), Flow> {
    use crate::buffer::buffer::BUFFER_SLOT_CASE_TABLE;
    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = buffers
        .get_mut(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    // Mirrors GNU `Fset_case_table` (`casetab.c:82-86`) → `set_case_table`
    // (`casetab.c:135-202`) which decomposes the table into 4 BVAR slots
    // and bset_*'s each one. NeoMacs collapses those into a single slot,
    // so the write here is the equivalent of GNU's bset_downcase_table
    // plus the implicit consistency between extras[0..2] and the other
    // 3 case tables. The case-table slot is always-local
    // (`local_flags_idx == -1`), so no flag bit needs setting.
    buf.slots[BUFFER_SLOT_CASE_TABLE] = table;
    Ok(())
}

/// Return `true` if `v` is a case table (char-table with `case-table` subtype).
pub fn is_case_table(v: &Value) -> bool {
    use super::chartable::is_char_table;
    if !is_char_table(v) {
        return false;
    }

    let Some(vec) = v.as_vector_data() else {
        return false;
    };
    if vec.len() <= CT_EXTRA_START + 2 || !vec[CT_SUBTYPE].is_symbol_named("case-table") {
        return false;
    }
    let ValueKind::Fixnum(extra_count) = vec[CT_EXTRA_COUNT].kind() else {
        return false;
    };
    if extra_count < 3 || vec.len() < CT_EXTRA_START + extra_count as usize {
        return false;
    }

    let up = vec[CT_EXTRA_START];
    let canon = vec[CT_EXTRA_START + 1];
    let eqv = vec[CT_EXTRA_START + 2];

    if !up.is_nil() && !is_char_table(&up) {
        return false;
    }

    if canon.is_nil() && eqv.is_nil() {
        true
    } else if is_char_table(&canon) {
        eqv.is_nil() || is_char_table(&eqv)
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "casetab_test.rs"]
mod tests;
