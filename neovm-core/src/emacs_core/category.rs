//! Character category system for the Elisp VM.
//!
//! Implements Emacs-compatible character categories.  Each character can
//! belong to zero or more *categories*, where a category is a single ASCII
//! letter (`a`-`z`, `A`-`Z`).  Categories are organized into *category
//! tables*; a `CategoryManager` keeps track of named tables and the
//! current table.

use super::error::{EvalResult, Flow, signal};
use super::intern::{intern, resolve_sym};
use super::value::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

thread_local! {
    static STANDARD_CATEGORY_TABLE: RefCell<Option<Value>> = const { RefCell::new(None) };
    static PURE_CATEGORY_MANAGER: RefCell<CategoryManager> = RefCell::new(CategoryManager::new());
}

/// Clear cached thread-local category table (must be called when heap changes).
pub fn reset_category_thread_locals() {
    STANDARD_CATEGORY_TABLE.with(|slot| *slot.borrow_mut() = None);
}

/// Collect GC roots from the cached category table.
pub fn collect_category_gc_roots(roots: &mut Vec<Value>) {
    STANDARD_CATEGORY_TABLE.with(|slot| {
        if let Some(v) = *slot.borrow() {
            roots.push(v);
        }
    });
}

const CATEGORY_TABLE_PROPERTY: &str = "category-table";

// ===========================================================================
// CategoryTable
// ===========================================================================

/// A single category table mapping characters to sets of category letters
/// and storing per-category descriptions.
#[derive(Clone, Debug)]
pub struct CategoryTable {
    /// Char -> set of category letters the char belongs to.
    pub entries: HashMap<char, HashSet<char>>,
    /// Category letter -> human-readable description string.
    pub descriptions: HashMap<char, String>,
}

impl CategoryTable {
    /// Create a new, empty category table.
    pub fn new() -> Self {
        let mut descriptions = HashMap::new();
        // Emacs ships pre-defined category entries, including digit classes.
        descriptions.insert('1', "decimal digit".to_string());
        Self {
            entries: HashMap::new(),
            descriptions,
        }
    }

    /// Define a category letter with a description.
    ///
    /// Returns an error string if `cat` is not a valid category letter.
    pub fn define_category(&mut self, cat: char, docstring: &str) -> Result<(), String> {
        if !is_category_letter(cat) {
            return Err(format!(
                "Invalid category character '{}': must be ASCII graphic",
                cat
            ));
        }
        // Match official Emacs: redefining a category silently updates
        // the docstring rather than erroring.
        self.descriptions.insert(cat, docstring.to_string());
        Ok(())
    }

    /// Return the description for a category letter, or `None`.
    pub fn category_docstring(&self, cat: char) -> Option<&str> {
        self.descriptions.get(&cat).map(|s| s.as_str())
    }

    /// Find an unused category letter (one that has no description defined).
    /// Searches `a`-`z` then `A`-`Z`.  Returns `None` if all 52 are in use.
    pub fn get_unused_category(&self) -> Option<char> {
        for ch in 'a'..='z' {
            if !self.descriptions.contains_key(&ch) {
                return Some(ch);
            }
        }
        for ch in 'A'..='Z' {
            if !self.descriptions.contains_key(&ch) {
                return Some(ch);
            }
        }
        None
    }

    /// Add `cat` to the category set of `ch`.
    pub fn modify_entry(&mut self, ch: char, cat: char, reset: bool) -> Result<(), String> {
        if !is_category_letter(cat) {
            return Err(format!(
                "Invalid category character '{}': must be ASCII graphic",
                cat
            ));
        }
        let set = self.entries.entry(ch).or_default();
        if reset {
            set.remove(&cat);
        } else {
            set.insert(cat);
        }
        Ok(())
    }

    /// Return the set of category letters for `ch` (empty set if none).
    pub fn char_category_set(&self, ch: char) -> HashSet<char> {
        self.entries.get(&ch).cloned().unwrap_or_default()
    }
}

impl Default for CategoryTable {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// CategoryManager
// ===========================================================================

/// Manages named category tables and tracks the current table.
#[derive(Clone, Debug)]
pub struct CategoryManager {
    /// Named tables.  The `"standard"` table is always present.
    pub tables: HashMap<String, CategoryTable>,
    /// Name of the currently active table.
    pub current_table: String,
}

impl CategoryManager {
    /// Create a new manager with a pre-created `"standard"` table.
    pub fn new() -> Self {
        let mut tables = HashMap::new();
        tables.insert("standard".to_string(), CategoryTable::new());
        Self {
            tables,
            current_table: "standard".to_string(),
        }
    }

    /// Return a reference to the current table.
    pub fn current(&self) -> &CategoryTable {
        self.tables
            .get(&self.current_table)
            .expect("current_table must exist in tables")
    }

    /// Return a mutable reference to the current table.
    pub fn current_mut(&mut self) -> &mut CategoryTable {
        self.tables
            .get_mut(&self.current_table)
            .expect("current_table must exist in tables")
    }

    /// Return a reference to the standard table.
    pub fn standard(&self) -> &CategoryTable {
        self.tables
            .get("standard")
            .expect("standard table must always exist")
    }

    /// Return a mutable reference to the standard table.
    pub fn standard_mut(&mut self) -> &mut CategoryTable {
        self.tables
            .get_mut("standard")
            .expect("standard table must always exist")
    }
}

impl Default for CategoryManager {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

/// Return `true` if `ch` is a valid category code (0x20..=0x7E).
/// Matches official Emacs `CATEGORYP` which uses `RANGED_FIXNUMP(0x20, x, 0x7E)`.
fn is_category_letter(ch: char) -> bool {
    ('\x20'..='\x7E').contains(&ch)
}

/// Extract a character argument from a `Value`, accepting both `Char` and
/// `Int` (code-point) forms.  Returns `Ok(None)` for internal Emacs codes
/// above the Unicode range (0x10FFFF < code <= 0x3FFFFF).
fn extract_char_opt(value: &Value, fn_name: &str) -> Result<Option<char>, Flow> {
    match value {
        Value::Char(c) => Ok(Some(*c)),
        Value::Int(n) => {
            if let Some(c) = char::from_u32(*n as u32) {
                Ok(Some(c))
            } else if (0..=0x3FFFFF).contains(n) {
                // Internal Emacs char code above Unicode range
                Ok(None)
            } else {
                Err(signal(
                    "error",
                    vec![Value::string(format!(
                        "{}: Invalid character code: {}",
                        fn_name, n
                    ))],
                ))
            }
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *other],
        )),
    }
}

/// Extract a character argument, signaling an error for non-Unicode codes.
fn extract_char(value: &Value, fn_name: &str) -> Result<char, Flow> {
    extract_char_opt(value, fn_name)?.ok_or_else(|| {
        signal(
            "error",
            vec![Value::string(format!(
                "{}: Invalid character code",
                fn_name
            ))],
        )
    })
}

/// Expect at least `min` arguments, signalling `wrong-number-of-arguments`
/// otherwise.
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

/// Expect exactly `n` arguments.
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

/// Expect at most `max` arguments.
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

fn make_category_table_object() -> EvalResult {
    Ok(super::chartable::make_char_table_value(
        Value::symbol("category-table"),
        Value::Nil,
    ))
}

fn is_category_table_value(value: &Value) -> Result<bool, Flow> {
    let is_char_table = super::chartable::builtin_char_table_p(vec![*value])?;
    if !is_char_table.is_truthy() {
        return Ok(false);
    }
    let subtype = super::chartable::builtin_char_table_subtype(vec![*value])?;
    Ok(matches!(subtype, Value::Symbol(id) if resolve_sym(id) == "category-table"))
}

fn clone_char_table_object(value: &Value) -> EvalResult {
    match value {
        Value::Vector(v) => Ok(Value::vector(with_heap(|h| h.get_vector(*v).clone()))),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("category-table-p"), *other],
        )),
    }
}

fn ensure_standard_category_table() -> EvalResult {
    STANDARD_CATEGORY_TABLE.with(|slot| {
        if let Some(table) = slot.borrow().as_ref() {
            return Ok(*table);
        }

        let table = make_category_table_object()?;
        *slot.borrow_mut() = Some(table);
        Ok(table)
    })
}

fn category_table_pointer_eq(lhs: &Value, rhs: &Value) -> bool {
    match (lhs, rhs) {
        (Value::Vector(a), Value::Vector(b)) => a == b,
        _ => false,
    }
}

fn current_buffer_category_table(eval: &mut super::eval::Evaluator) -> Result<Value, Flow> {
    let fallback = ensure_standard_category_table()?;
    let buf = eval
        .buffers
        .current_buffer_mut()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    if let Some(table) = buf.properties.get(CATEGORY_TABLE_PROPERTY) {
        if is_category_table_value(table)? {
            return Ok(*table);
        }
    }

    buf.properties
        .insert(CATEGORY_TABLE_PROPERTY.to_string(), fallback);
    Ok(fallback)
}

fn set_current_buffer_category_table(
    eval: &mut super::eval::Evaluator,
    table: Value,
) -> Result<(), Flow> {
    let buf = eval
        .buffers
        .current_buffer_mut()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    buf.properties
        .insert(CATEGORY_TABLE_PROPERTY.to_string(), table);
    Ok(())
}

// ===========================================================================
// Pure builtins (no evaluator needed)
// ===========================================================================

/// `(define-category CHAR DOCSTRING &optional TABLE)`
///
/// Define category CHAR (a single letter) with the given DOCSTRING.
/// TABLE is currently ignored (uses the standard table).
/// Returns nil.
pub(crate) fn builtin_define_category(args: Vec<Value>) -> EvalResult {
    expect_min_args("define-category", &args, 2)?;
    expect_max_args("define-category", &args, 3)?;

    let cat = extract_char(&args[0], "define-category")?;
    let docstring = match &args[1] {
        Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };

    if !is_category_letter(cat) {
        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Invalid category character '{}': must be ASCII graphic",
                cat
            ))],
        ));
    }

    // TABLE (arg 2) is currently ignored; category tables are not first-class.
    PURE_CATEGORY_MANAGER.with(|slot| {
        slot.borrow_mut()
            .current_mut()
            .define_category(cat, &docstring)
            .map_err(|msg| signal("error", vec![Value::string(msg)]))
    })?;

    Ok(Value::Nil)
}

/// `(category-docstring CATEGORY &optional TABLE)`
///
/// Return the docstring of CATEGORY from the pure category manager.
pub(crate) fn builtin_category_docstring(args: Vec<Value>) -> EvalResult {
    expect_min_args("category-docstring", &args, 1)?;
    expect_max_args("category-docstring", &args, 2)?;

    let cat = extract_char(&args[0], "category-docstring")?;
    // TABLE (arg 1) is currently ignored; category tables are not first-class.
    let _ = args.get(1);

    PURE_CATEGORY_MANAGER.with(|slot| {
        let doc = slot.borrow();
        match doc.current().category_docstring(cat) {
            Some(text) => Ok(Value::string(text)),
            None => Ok(Value::Nil),
        }
    })
}

/// `(get-unused-category &optional TABLE)`
///
/// Return an unused category letter, or nil if all 52 are used.
pub(crate) fn builtin_get_unused_category(args: Vec<Value>) -> EvalResult {
    expect_max_args("get-unused-category", &args, 1)?;
    // TABLE (arg 0) is currently ignored; category tables are not first-class.
    let _ = args.first();

    PURE_CATEGORY_MANAGER.with(|slot| match slot.borrow().current().get_unused_category() {
        Some(cat) => Ok(Value::Char(cat)),
        None => Ok(Value::Nil),
    })
}

/// `(category-table-p OBJ)`
///
/// Return t if OBJ is a category table.  In this implementation, category
/// tables are represented as char-tables with subtype `category-table`.
pub(crate) fn builtin_category_table_p(args: Vec<Value>) -> EvalResult {
    expect_args("category-table-p", &args, 1)?;
    Ok(Value::bool(is_category_table_value(&args[0])?))
}

/// `(category-table)`
///
/// Return the standard category table in pure mode.
pub(crate) fn builtin_category_table(args: Vec<Value>) -> EvalResult {
    expect_max_args("category-table", &args, 0)?;
    ensure_standard_category_table()
}

/// `(standard-category-table)`
///
/// Return the standard category table.
pub(crate) fn builtin_standard_category_table(args: Vec<Value>) -> EvalResult {
    expect_max_args("standard-category-table", &args, 0)?;
    ensure_standard_category_table()
}

/// `(make-category-table)`
///
/// Create a new (empty) category table.
pub(crate) fn builtin_make_category_table(args: Vec<Value>) -> EvalResult {
    expect_max_args("make-category-table", &args, 0)?;
    make_category_table_object()
}

/// `(copy-category-table &optional TABLE)`
///
/// Return a fresh copy of TABLE.  When TABLE is omitted or nil, copy the
/// process-wide standard category table.
pub(crate) fn builtin_copy_category_table(args: Vec<Value>) -> EvalResult {
    expect_max_args("copy-category-table", &args, 1)?;

    let source = match args.first() {
        None | Some(Value::Nil) => ensure_standard_category_table()?,
        Some(table) => {
            if !is_category_table_value(table)? {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("category-table-p"), *table],
                ));
            }
            *table
        }
    };

    clone_char_table_object(&source)
}

/// `(set-category-table TABLE)`
///
/// Pure-mode fallback: validate TABLE and return it.
pub(crate) fn builtin_set_category_table(args: Vec<Value>) -> EvalResult {
    expect_args("set-category-table", &args, 1)?;
    if args[0].is_nil() {
        return ensure_standard_category_table();
    }
    if !is_category_table_value(&args[0])? {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("category-table-p"), args[0]],
        ));
    }
    Ok(args[0])
}

/// `(make-category-set CATEGORIES)`
///
/// Return a bool-vector representing a set of categories.
/// CATEGORIES is a string of category letters.
/// The resulting bool-vector has 128 slots (one per ASCII code);
/// positions corresponding to the given category letters are set to t.
pub(crate) fn builtin_make_category_set(args: Vec<Value>) -> EvalResult {
    expect_args("make-category-set", &args, 1)?;

    let cats = match &args[0] {
        Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };

    // Build a bool-vector of 128 slots (matching Emacs behavior).
    let mut bits = vec![Value::Int(0); 128];
    for ch in cats.chars() {
        if is_category_letter(ch) {
            let idx = ch as usize;
            if idx < 128 {
                bits[idx] = Value::Int(1);
            }
        }
    }

    // Return as a plain list of 0/1 values wrapped in a cons.
    // In Emacs this would be a bool-vector, but for compatibility with our
    // bool-vector implementation we construct one using the tagged-vector
    // convention from chartable.rs.
    let mut vec = Vec::with_capacity(2 + 128);
    vec.push(Value::Symbol(intern("--bool-vector--")));
    vec.push(Value::Int(128));
    vec.extend(bits);
    Ok(Value::vector(vec))
}

/// `(category-set-mnemonics CATEGORY-SET)`
///
/// Return CATEGORY-SET as a sorted mnemonic string.
pub(crate) fn builtin_category_set_mnemonics(args: Vec<Value>) -> EvalResult {
    expect_args("category-set-mnemonics", &args, 1)?;

    let Value::Vector(bits_arc) = &args[0] else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("categorysetp"), args[0]],
        ));
    };

    let bits = with_heap(|h| h.get_vector(*bits_arc).clone());
    let valid_shape = bits.len() >= 130
        && matches!(&bits[0], Value::Symbol(id) if resolve_sym(*id) == "--bool-vector--")
        && matches!(&bits[1], Value::Int(128));
    if !valid_shape {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("categorysetp"), args[0]],
        ));
    }

    let mut out = String::new();
    for idx in 0..128usize {
        let is_set = match &bits[2 + idx] {
            Value::Nil => false,
            Value::Int(0) => false,
            _ => true,
        };
        if is_set {
            let ch = idx as u8 as char;
            if is_category_letter(ch) {
                out.push(ch);
            }
        }
    }

    Ok(Value::string(&out))
}

// ===========================================================================
// Eval-dependent builtins (require evaluator / CategoryManager access)
// ===========================================================================

/// `(modify-category-entry CHAR CATEGORY &optional TABLE RESET)`
///
/// Add (or remove when RESET is non-nil) CATEGORY from the category set
/// of CHAR in the current category table.
pub(crate) fn builtin_modify_category_entry(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    modify_category_entry_in_manager(&mut eval.category_manager, &args)
}

pub(crate) fn modify_category_entry_in_manager(
    category_manager: &mut CategoryManager,
    args: &[Value],
) -> EvalResult {
    expect_min_args("modify-category-entry", &args, 2)?;
    expect_max_args("modify-category-entry", &args, 4)?;

    let cat = extract_char(&args[1], "modify-category-entry")?;

    // TABLE (arg 2) is ignored — we always use the current table.
    let reset = if args.len() >= 4 {
        args[3].is_truthy()
    } else {
        false
    };

    if !is_category_letter(cat) {
        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Invalid category character '{}': must be 0x20..0x7E",
                cat
            ))],
        ));
    }

    // First argument: single character OR range (FROM . TO).
    match &args[0] {
        Value::Cons(cell) => {
            // Range: (FROM . TO)
            let pair = read_cons(*cell);
            let from = extract_char_opt(&pair.car, "modify-category-entry")?;
            let to = extract_char_opt(&pair.cdr, "modify-category-entry")?;
            match (from, to) {
                (Some(f), Some(t)) => {
                    let table = category_manager.current_mut();
                    for cp in (f as u32)..=(t as u32) {
                        if let Some(ch) = char::from_u32(cp) {
                            table
                                .modify_entry(ch, cat, reset)
                                .map_err(|msg| signal("error", vec![Value::string(&msg)]))?;
                        }
                    }
                }
                _ => {
                    // Range endpoints are non-Unicode Emacs internal codes;
                    // silently skip.
                }
            }
        }
        _ => {
            if let Some(ch) = extract_char_opt(&args[0], "modify-category-entry")? {
                category_manager
                    .current_mut()
                    .modify_entry(ch, cat, reset)
                    .map_err(|msg| signal("error", vec![Value::string(&msg)]))?;
            }
            // Non-Unicode internal code: silently skip.
        }
    }

    Ok(Value::Nil)
}

/// `(define-category CHAR DOCSTRING &optional TABLE)` (evaluator-backed).
///
/// Stores the category docstring in the active category manager table and
/// returns nil.
pub(crate) fn builtin_define_category_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("define-category", &args, 2)?;
    expect_max_args("define-category", &args, 3)?;

    let cat = extract_char(&args[0], "define-category")?;
    let docstring = match &args[1] {
        Value::Str(id) => with_heap(|h| h.get_string(*id).to_owned()),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
            ));
        }
    };

    if !is_category_letter(cat) {
        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Invalid category character '{}': must be ASCII graphic",
                cat
            ))],
        ));
    }

    // TABLE (arg 2) is currently ignored; category tables are not first-class.
    eval.category_manager
        .current_mut()
        .define_category(cat, &docstring)
        .map_err(|msg| signal("error", vec![Value::string(&msg)]))?;

    let _ = docstring;
    Ok(Value::Nil)
}

/// `(category-docstring CATEGORY &optional TABLE)` (evaluator-backed).
///
/// Returns the category docstring from the active table, or nil when absent.
pub(crate) fn builtin_category_docstring_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("category-docstring", &args, 1)?;
    expect_max_args("category-docstring", &args, 2)?;

    let cat = extract_char(&args[0], "category-docstring")?;
    // TABLE (arg 1) is currently ignored; category tables are not first-class.
    let _ = args.get(1);

    match eval.category_manager.current().category_docstring(cat) {
        Some(doc) => Ok(Value::string(doc)),
        None => Ok(Value::Nil),
    }
}

/// `(get-unused-category &optional TABLE)` (evaluator-backed).
///
/// Returns the first unused category letter in the active table, or nil.
pub(crate) fn builtin_get_unused_category_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("get-unused-category", &args, 1)?;
    // TABLE (arg 0) is currently ignored; category tables are not first-class.
    let _ = args.first();

    match eval.category_manager.current().get_unused_category() {
        Some(cat) => Ok(Value::Char(cat)),
        None => Ok(Value::Nil),
    }
}

/// `(char-category-set CHAR)`
///
/// Return a bool-vector of 128 elements indicating which categories CHAR
/// belongs to in the current category table.
pub(crate) fn builtin_char_category_set(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("char-category-set", &args, 1)?;

    let ch = extract_char(&args[0], "char-category-set")?;

    let cats = eval.category_manager.current().char_category_set(ch);

    // Build a 128-element bool-vector.
    let mut bits = vec![Value::Int(0); 128];
    for &cat in &cats {
        let idx = cat as usize;
        if idx < 128 {
            bits[idx] = Value::Int(1);
        }
    }

    let mut vec = Vec::with_capacity(2 + 128);
    vec.push(Value::Symbol(intern("--bool-vector--")));
    vec.push(Value::Int(128));
    vec.extend(bits);
    Ok(Value::vector(vec))
}

/// `(category-table)` (evaluator-backed).
///
/// Return the current buffer's category table object.
pub(crate) fn builtin_category_table_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("category-table", &args, 0)?;
    current_buffer_category_table(eval)
}

/// `(standard-category-table)` (evaluator-backed).
///
/// Return the process-wide standard category table object.
pub(crate) fn builtin_standard_category_table_eval(
    _eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("standard-category-table", &args, 0)?;
    ensure_standard_category_table()
}

/// `(set-category-table TABLE)` (evaluator-backed).
///
/// Install TABLE in the current buffer and return the installed table.
pub(crate) fn builtin_set_category_table_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-category-table", &args, 1)?;

    let installed = if args[0].is_nil() {
        let current = current_buffer_category_table(eval)?;
        let standard = ensure_standard_category_table()?;
        if category_table_pointer_eq(&current, &standard) {
            standard
        } else {
            clone_char_table_object(&standard)?
        }
    } else {
        if !is_category_table_value(&args[0])? {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("category-table-p"), args[0]],
            ));
        }
        args[0]
    };

    set_current_buffer_category_table(eval, installed)?;
    Ok(installed)
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "category_test.rs"]
mod tests;
