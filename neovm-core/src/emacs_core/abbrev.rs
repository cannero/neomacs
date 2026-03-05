//! Abbreviation system -- text abbreviation expansion.
//!
//! Provides Emacs-compatible abbrev functionality:
//! - `define-abbrev` -- define an abbreviation in a table
//! - `expand-abbrev` -- expand the word before point (returns expansion)
//! - `abbrev-mode` -- toggle or query abbrev-mode
//! - `define-abbrev-table` -- create a named abbrev table
//! - `clear-abbrev-table` -- clear all abbrevs from a table
//! - `abbrev-table-get` -- query a table's properties
//! - `insert-abbrev-table-description` -- describe a table's contents
//! - `abbrev-expansion` -- look up an expansion without expanding

use std::collections::HashMap;

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::value::{Value, list_to_vec, with_heap};

// ---------------------------------------------------------------------------
// Abbrev types
// ---------------------------------------------------------------------------

/// A single abbreviation entry.
#[derive(Clone, Debug)]
pub struct Abbrev {
    /// The text that the abbreviation expands to.
    pub expansion: String,
    /// Optional hook function name to call after expansion.
    pub hook: Option<String>,
    /// Number of times this abbrev has been expanded.
    pub count: usize,
    /// If true, this is a system abbrev (not saved to file).
    pub system: bool,
}

/// A named table of abbreviations.
#[derive(Clone, Debug)]
pub struct AbbrevTable {
    /// The table's name (e.g. "global-abbrev-table", "lisp-mode-abbrev-table").
    pub name: String,
    /// The abbreviations: key is the abbreviated word (lowercased for
    /// case-insensitive matching), value is the Abbrev.
    pub abbrevs: HashMap<String, Abbrev>,
    /// Parent table name (for inheritance).
    pub parent: Option<String>,
    /// If true, the case of the abbreviation is kept as-is (no case folding).
    pub case_fixed: bool,
    /// If true, quoting characters (backslash) are handled.
    pub enable_quoting: bool,
}

impl AbbrevTable {
    /// Create a new empty abbrev table.
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            abbrevs: HashMap::new(),
            parent: None,
            case_fixed: false,
            enable_quoting: false,
        }
    }
}

// ---------------------------------------------------------------------------
// AbbrevManager
// ---------------------------------------------------------------------------

/// Central registry for all abbrev tables and the global abbrev-mode toggle.
#[derive(Clone, Debug)]
pub struct AbbrevManager {
    tables: HashMap<String, AbbrevTable>,
    global_table_name: String,
    abbrev_mode: bool,
}

impl Default for AbbrevManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AbbrevManager {
    /// Create a new manager with the default global abbrev table.
    pub fn new() -> Self {
        let global_name = "global-abbrev-table".to_string();
        let mut tables = HashMap::new();
        tables.insert(global_name.clone(), AbbrevTable::new(&global_name));
        Self {
            tables,
            global_table_name: global_name,
            abbrev_mode: false,
        }
    }

    /// Define an abbreviation in the named table.
    /// Creates the table if it does not already exist.
    pub fn define_abbrev(&mut self, table: &str, abbrev: &str, expansion: &str) {
        let tbl = self
            .tables
            .entry(table.to_string())
            .or_insert_with(|| AbbrevTable::new(table));
        let key = abbrev.to_lowercase();
        tbl.abbrevs.insert(
            key,
            Abbrev {
                expansion: expansion.to_string(),
                hook: None,
                count: 0,
                system: false,
            },
        );
    }

    /// Define an abbreviation with full options.
    pub fn define_abbrev_full(
        &mut self,
        table: &str,
        abbrev: &str,
        expansion: &str,
        hook: Option<String>,
        system: bool,
    ) {
        let tbl = self
            .tables
            .entry(table.to_string())
            .or_insert_with(|| AbbrevTable::new(table));
        let key = abbrev.to_lowercase();
        tbl.abbrevs.insert(
            key,
            Abbrev {
                expansion: expansion.to_string(),
                hook,
                count: 0,
                system,
            },
        );
    }

    /// Try to expand `word` in the given table.  Returns the expansion
    /// string if found, and increments the usage count.  If the table has
    /// a parent and the word is not found locally, looks up the parent.
    pub fn expand_abbrev(&mut self, table: &str, word: &str) -> Option<String> {
        let key = word.to_lowercase();

        // Check if the word is in this table
        if let Some(tbl) = self.tables.get_mut(table) {
            if let Some(ab) = tbl.abbrevs.get_mut(&key) {
                ab.count += 1;
                let expansion = apply_case(&ab.expansion, word, tbl.case_fixed);
                return Some(expansion);
            }
        }

        // Fall back to parent table
        let parent = self.tables.get(table).and_then(|t| t.parent.clone());
        if let Some(parent_name) = parent {
            return self.expand_abbrev(&parent_name, word);
        }

        // Fall back to global table if this isn't already the global table
        if table != self.global_table_name {
            let global = self.global_table_name.clone();
            return self.expand_abbrev(&global, word);
        }

        None
    }

    /// Create a new abbrev table (or return a mutable reference to an
    /// existing one).
    pub fn create_table(&mut self, name: &str) -> &mut AbbrevTable {
        self.tables
            .entry(name.to_string())
            .or_insert_with(|| AbbrevTable::new(name))
    }

    /// Get a reference to a table by name.
    pub fn get_table(&self, name: &str) -> Option<&AbbrevTable> {
        self.tables.get(name)
    }

    /// List all abbreviations in a table as (abbrev, expansion) pairs,
    /// sorted by abbrev name.
    pub fn list_abbrevs(&self, table: &str) -> Vec<(&str, &str)> {
        match self.tables.get(table) {
            Some(tbl) => {
                let mut entries: Vec<(&str, &str)> = tbl
                    .abbrevs
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.expansion.as_str()))
                    .collect();
                entries.sort_by_key(|(k, _)| *k);
                entries
            }
            None => Vec::new(),
        }
    }

    /// Clear all abbreviations from a table.
    pub fn clear_table(&mut self, table: &str) {
        if let Some(tbl) = self.tables.get_mut(table) {
            tbl.abbrevs.clear();
        }
    }

    /// Whether abbrev-mode is currently enabled.
    pub fn is_enabled(&self) -> bool {
        self.abbrev_mode
    }

    /// Set the abbrev-mode flag.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.abbrev_mode = enabled;
    }

    /// Return the global table name.
    pub fn global_table_name(&self) -> &str {
        &self.global_table_name
    }

    /// Return all table names, sorted.
    pub fn all_table_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.tables.keys().map(|s| s.as_str()).collect();
        names.sort();
        names
    }

    // pdump accessors
    pub(crate) fn dump_tables(&self) -> &HashMap<String, AbbrevTable> { &self.tables }
    pub(crate) fn dump_global_table_name(&self) -> &str { &self.global_table_name }
    pub(crate) fn dump_abbrev_mode(&self) -> bool { self.abbrev_mode }
    pub(crate) fn from_dump(tables: HashMap<String, AbbrevTable>, global_table_name: String, abbrev_mode: bool) -> Self {
        Self { tables, global_table_name, abbrev_mode }
    }
}

// ---------------------------------------------------------------------------
// Case handling
// ---------------------------------------------------------------------------

/// Apply case transformation from the input word to the expansion.
///
/// If `case_fixed` is true, return the expansion as-is.
/// Otherwise:
/// - If the word is all uppercase, upcase the expansion.
/// - If the word starts with an uppercase letter, capitalize the expansion.
/// - Otherwise, return the expansion as-is (lowercase).
fn apply_case(expansion: &str, word: &str, case_fixed: bool) -> String {
    if case_fixed || word.is_empty() || expansion.is_empty() {
        return expansion.to_string();
    }

    let all_upper = word.chars().all(|c| !c.is_alphabetic() || c.is_uppercase());
    let first_upper = word
        .chars()
        .next()
        .map(|c| c.is_uppercase())
        .unwrap_or(false);

    if all_upper && word.chars().any(|c| c.is_alphabetic()) {
        expansion.to_uppercase()
    } else if first_upper {
        let mut chars = expansion.chars();
        match chars.next() {
            Some(first) => {
                let mut result = first.to_uppercase().to_string();
                result.extend(chars);
                result
            }
            None => expansion.to_string(),
        }
    } else {
        expansion.to_string()
    }
}

// ===========================================================================
// Builtin helpers
// ===========================================================================

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

fn expect_string(value: &Value) -> Result<String, Flow> {
    match value {
        Value::Str(id) => Ok(with_heap(|h| h.get_string(*id).clone())),
        Value::Symbol(id) => Ok(resolve_sym(*id).to_owned()),
        Value::Nil => Ok("nil".to_string()),
        Value::True => Ok("t".to_string()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *other],
        )),
    }
}

// ===========================================================================
// Builtins (evaluator-dependent)
// ===========================================================================

/// (define-abbrev TABLE ABBREV EXPANSION &optional HOOK SYSTEM) -> nil
///
/// TABLE is a string naming the abbrev table.
pub(crate) fn builtin_define_abbrev(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("define-abbrev", &args, 3)?;
    let table = expect_string(&args[0])?;
    let abbrev = expect_string(&args[1])?;
    let expansion = expect_string(&args[2])?;
    let hook = if args.len() > 3 && !args[3].is_nil() {
        Some(expect_string(&args[3])?)
    } else {
        None
    };
    let system = args.len() > 4 && args[4].is_truthy();

    eval.abbrevs
        .define_abbrev_full(&table, &abbrev, &expansion, hook, system);
    Ok(Value::Nil)
}

/// (expand-abbrev) -> string or nil
///
/// NeoVM currently provides a batch-compatible surface: the callable arity
/// matches GNU Emacs and the primitive returns nil in non-interactive use.
/// Full buffer-context expansion is tracked separately.
pub(crate) fn builtin_expand_abbrev(
    _eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("expand-abbrev", &args, 0)?;
    Ok(Value::Nil)
}

/// (abbrev-mode &optional ARG) -> t or nil
///
/// With no arg, toggle abbrev-mode.  With a positive arg, enable.
/// With zero or negative, disable.  Returns the new state.
pub(crate) fn builtin_abbrev_mode(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    if args.is_empty() {
        // Toggle
        let new_state = !eval.abbrevs.is_enabled();
        eval.abbrevs.set_enabled(new_state);
        Ok(Value::bool(new_state))
    } else {
        match &args[0] {
            Value::Int(n) => {
                let enabled = *n > 0;
                eval.abbrevs.set_enabled(enabled);
                Ok(Value::bool(enabled))
            }
            Value::Nil => {
                eval.abbrevs.set_enabled(false);
                Ok(Value::Nil)
            }
            _ => {
                eval.abbrevs.set_enabled(true);
                Ok(Value::True)
            }
        }
    }
}

/// (define-abbrev-table NAME DEFS &optional DOCSTRING &rest PROPS) -> nil
///
/// Create a new abbrev table with the given NAME.
///
/// For compatibility we require at least NAME+DEFS arity, but still accept
/// NeoVM's legacy string/symbol parent shorthand in DEFS.
pub(crate) fn builtin_define_abbrev_table(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("define-abbrev-table", &args, 2)?;
    let name = expect_string(&args[0])?;
    let tbl = eval.abbrevs.create_table(&name);
    if let Some(parent) = args.get(1).and_then(|value| match value {
        Value::Str(id) => Some(with_heap(|h| h.get_string(*id).clone())),
        Value::Symbol(id) => Some(resolve_sym(*id).to_owned()),
        _ => None,
    }) {
        tbl.parent = Some(parent);
    }

    // GNU Emacs compatibility:
    // - Third arg is optional docstring.
    // - If non-nil symbol and PROPS exists, treat it as first property key.
    let mut props = if args.len() > 3 {
        args[3..].to_vec()
    } else {
        Vec::new()
    };
    if let Some(docstring) = args.get(2) {
        let symbolp_non_nil = !docstring.is_nil()
            && matches!(
                docstring,
                Value::Symbol(_) | Value::Keyword(_) | Value::True
            );
        if symbolp_non_nil && !props.is_empty() {
            props.insert(0, *docstring);
        }
    }

    let mut idx = 0usize;
    while idx < props.len() {
        if idx + 1 >= props.len() {
            return Err(signal(
                "error",
                vec![Value::string(format!(
                    "Missing value for property {}",
                    props[idx]
                ))],
            ));
        }

        if let Some(prop_name) = match &props[idx] {
            Value::Symbol(id) => Some(resolve_sym(*id)),
            Value::Keyword(id) => Some(resolve_sym(*id)),
            Value::True => Some("t"),
            Value::Nil => Some("nil"),
            _ => None,
        } {
            match prop_name {
                ":case-fixed" => {
                    tbl.case_fixed = props[idx + 1].is_truthy();
                }
                ":parents" => {
                    let parent = match &props[idx + 1] {
                        Value::Str(id) => Some(with_heap(|h| h.get_string(*id).clone())),
                        Value::Symbol(id) => Some(resolve_sym(*id).to_owned()),
                        Value::Keyword(id) => Some(resolve_sym(*id).to_owned()),
                        Value::True => Some("t".to_string()),
                        Value::Nil => Some("nil".to_string()),
                        Value::Cons(_) => list_to_vec(&props[idx + 1]).and_then(|parents| {
                            parents.first().and_then(|first| match first {
                                Value::Str(id) => Some(with_heap(|h| h.get_string(*id).clone())),
                                Value::Symbol(id) => Some(resolve_sym(*id).to_owned()),
                                Value::Keyword(id) => Some(resolve_sym(*id).to_owned()),
                                Value::True => Some("t".to_string()),
                                Value::Nil => Some("nil".to_string()),
                                _ => None,
                            })
                        }),
                        _ => None,
                    };
                    if let Some(parent) = parent {
                        tbl.parent = Some(parent);
                    }
                }
                _ => {}
            }
        }

        idx += 2;
    }

    Ok(Value::Nil)
}

/// (clear-abbrev-table TABLE) -> nil
pub(crate) fn builtin_clear_abbrev_table(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("clear-abbrev-table", &args, 1)?;
    let table = expect_string(&args[0])?;
    eval.abbrevs.clear_table(&table);
    Ok(Value::Nil)
}

/// (abbrev-expansion ABBREV &optional TABLE) -> string or nil
///
/// Look up the expansion of ABBREV without actually expanding it
/// (does not increment the count).
pub(crate) fn builtin_abbrev_expansion(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("abbrev-expansion", &args, 1)?;
    let abbrev = expect_string(&args[0])?;
    let table_name = if args.len() > 1 && !args[1].is_nil() {
        expect_string(&args[1])?
    } else {
        eval.abbrevs.global_table_name().to_string()
    };
    let key = abbrev.to_lowercase();
    match eval.abbrevs.get_table(&table_name) {
        Some(tbl) => match tbl.abbrevs.get(&key) {
            Some(ab) => Ok(Value::string(ab.expansion.clone())),
            None => Ok(Value::Nil),
        },
        None => Ok(Value::Nil),
    }
}

/// (insert-abbrev-table-description TABLE) -> string
///
/// Return a human-readable description of the abbreviations in TABLE.
pub(crate) fn builtin_insert_abbrev_table_description(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("insert-abbrev-table-description", &args, 1)?;
    let table_name = expect_string(&args[0])?;
    let entries = eval.abbrevs.list_abbrevs(&table_name);
    if entries.is_empty() {
        Ok(Value::string(format!(
            "(define-abbrev-table '{})\n",
            table_name
        )))
    } else {
        let mut out = format!("(define-abbrev-table '{}\n  '(\n", table_name);
        for (abbrev, expansion) in &entries {
            out.push_str(&format!("    (\"{}\" \"{}\")\n", abbrev, expansion));
        }
        out.push_str("    ))\n");
        Ok(Value::string(out))
    }
}

/// (abbrev-table-p NAME) -> t or nil
///
/// Return t if NAME is a known abbrev table, nil otherwise.
pub(crate) fn builtin_abbrev_table_p(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("abbrev-table-p", &args, 1)?;
    let name = expect_string(&args[0])?;
    Ok(Value::bool(eval.abbrevs.get_table(&name).is_some()))
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "abbrev_test.rs"]
mod tests;
