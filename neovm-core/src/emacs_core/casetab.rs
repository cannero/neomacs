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
            vec![Value::symbol(name), Value::Int(args.len() as i64)],
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
    match value {
        Value::Char(c) => Ok(*c),
        Value::Int(n) => {
            if let Some(c) = char::from_u32(*n as u32) {
                Ok(c)
            } else {
                Err(wrong_type("characterp", value))
            }
        }
        other => Err(wrong_type("characterp", other)),
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
    Ok(Value::bool(is_case_table(&args[0])))
}

/// `(current-case-table)` -- return the current case table.
///
/// Pure fallback returns the standard case table object.
pub(crate) fn builtin_current_case_table(args: Vec<Value>) -> EvalResult {
    expect_args("current-case-table", &args, 0)?;
    ensure_standard_case_table_object()
}

/// `(standard-case-table)` -- return the standard case table.
///
/// Returns the process-wide standard case table object.
pub(crate) fn builtin_standard_case_table(args: Vec<Value>) -> EvalResult {
    expect_args("standard-case-table", &args, 0)?;
    ensure_standard_case_table_object()
}

/// `(set-case-table TABLE)` -- set the current case table.
///
/// Pure fallback: validate TABLE and return it.
pub(crate) fn builtin_set_case_table(args: Vec<Value>) -> EvalResult {
    expect_args("set-case-table", &args, 1)?;
    if !is_case_table(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("case-table-p"), args[0]],
        ));
    }
    Ok(args[0])
}

/// `(set-standard-case-table TABLE)` -- set the standard case table.
///
/// Pure fallback: validate TABLE and return it.
pub(crate) fn builtin_set_standard_case_table(args: Vec<Value>) -> EvalResult {
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
    Ok(args[0])
}

/// `(current-case-table)` -- evaluator-backed current buffer case table object.
pub(crate) fn builtin_current_case_table_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("current-case-table", &args, 0)?;
    current_case_table_for_buffer(eval)
}

/// `(standard-case-table)` -- evaluator-backed standard case table object.
pub(crate) fn builtin_standard_case_table_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("standard-case-table", &args, 0)?;
    ensure_standard_case_table_object_eval(eval)
}

/// `(set-case-table TABLE)` -- evaluator-backed current buffer case table set.
pub(crate) fn builtin_set_case_table_eval(
    eval: &mut super::eval::Evaluator,
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
    set_current_case_table_for_buffer(eval, table)?;
    Ok(table)
}

/// `(set-standard-case-table TABLE)` -- evaluator-backed standard table set.
pub(crate) fn builtin_set_standard_case_table_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    let table = builtin_set_standard_case_table(args)?;
    eval.obarray
        .set_symbol_value(STANDARD_CASE_TABLE_SYMBOL, table);
    Ok(table)
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
    Ok(Value::Int(result as i64))
}

// ---------------------------------------------------------------------------
// Case-table as char-table
// ---------------------------------------------------------------------------

// Char-table vector layout constants (mirrored from chartable.rs).
const CT_CHAR_TABLE_TAG: &str = "--char-table--";
const CT_SUBTYPE: usize = 3;
const CT_EXTRA_COUNT: usize = 4;
const CT_EXTRA_START: usize = 5;
const CURRENT_CASE_TABLE_PROPERTY: &str = "case-table";
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
    vec.push(Value::Nil); // CT_PARENT
    vec.push(Value::symbol(subtype)); // CT_SUBTYPE
    vec.push(Value::Int(extra_count as i64)); // CT_EXTRA_COUNT
    for slot in extra_slots {
        vec.push(*slot);
    }
    for &(ch, val) in data_pairs {
        vec.push(Value::Int(ch));
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
        downcase_pairs.push((i, Value::Int(down)));

        // Upcase: a-z -> A-Z, others -> themselves
        let up = if (b'a' as i64..=b'z' as i64).contains(&i) {
            i + (b'A' as i64 - b'a' as i64)
        } else {
            i
        };
        upcase_pairs.push((i, Value::Int(up)));

        // Canonicalize: same as downcase
        canon_pairs.push((i, Value::Int(down)));

        // Equivalences: A -> a, a -> A, others -> themselves
        let eqv = if (b'A' as i64..=b'Z' as i64).contains(&i) {
            i + (b'a' as i64 - b'A' as i64)
        } else if (b'a' as i64..=b'z' as i64).contains(&i) {
            i + (b'A' as i64 - b'a' as i64)
        } else {
            i
        };
        eqv_pairs.push((i, Value::Int(eqv)));
    }

    // Build subsidiary char-tables (no extra slots)
    let upcase_ct = build_char_table("case-table", &[], Value::Nil, &upcase_pairs);
    let canon_ct = build_char_table("case-table", &[], Value::Nil, &canon_pairs);
    let eqv_ct = build_char_table("case-table", &[], Value::Nil, &eqv_pairs);

    // Build the main downcase char-table with 3 extra slots
    build_char_table(
        "case-table",
        &[upcase_ct, canon_ct, eqv_ct],
        Value::Nil,
        &downcase_pairs,
    )
}

/// Create an empty case-table char-table (valid for `case-table-p`).
fn make_case_table_value() -> Value {
    build_char_table(
        "case-table",
        &[Value::Nil, Value::Nil, Value::Nil],
        Value::Nil,
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

fn ensure_standard_case_table_object_eval(eval: &mut super::eval::Evaluator) -> EvalResult {
    if let Some(value) = eval
        .obarray
        .symbol_value(STANDARD_CASE_TABLE_SYMBOL)
        .cloned()
    {
        if is_case_table(&value) {
            return Ok(value);
        }
    }
    let table = make_standard_case_table_value();
    eval.obarray
        .set_symbol_value(STANDARD_CASE_TABLE_SYMBOL, table);
    Ok(table)
}

fn current_case_table_for_buffer(eval: &mut super::eval::Evaluator) -> Result<Value, Flow> {
    let fallback = ensure_standard_case_table_object_eval(eval)?;
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let buf = eval
        .buffers
        .get(current_id)
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    if let Some(value) = buf.properties.get(CURRENT_CASE_TABLE_PROPERTY) {
        if is_case_table(value) {
            return Ok(*value);
        }
    }

    let _ =
        eval.buffers
            .set_buffer_local_property(current_id, CURRENT_CASE_TABLE_PROPERTY, fallback);
    Ok(fallback)
}

fn set_current_case_table_for_buffer(
    eval: &mut super::eval::Evaluator,
    table: Value,
) -> Result<(), Flow> {
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = eval
        .buffers
        .set_buffer_local_property(current_id, CURRENT_CASE_TABLE_PROPERTY, table);
    Ok(())
}

/// Return `true` if `v` is a case table (char-table with `case-table` subtype).
pub fn is_case_table(v: &Value) -> bool {
    use super::chartable::is_char_table;
    if !is_char_table(v) {
        return false;
    }
    if let Value::Vector(arc) = v {
        let vec = with_heap(|h| h.get_vector(*arc).clone());
        vec.len() > CT_SUBTYPE
            && matches!(&vec[CT_SUBTYPE], Value::Symbol(id) if resolve_sym(*id) == "case-table")
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
