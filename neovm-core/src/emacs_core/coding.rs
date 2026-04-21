//! Emacs coding system support.
//!
//! Since Rust natively uses UTF-8, this module is primarily for API
//! compatibility. The coding system infrastructure tracks registered
//! systems and their aliases but all actual encoding/decoding passes
//! through as UTF-8 identity operations.
//!
//! Contains:
//! - CodingSystemManager: registry of coding systems, aliases, priority list
//! - CodingSystemInfo: per-system metadata (name, type, mnemonic, EOL)
//! - Pure builtins: coding-system-list, coding-system-aliases, coding-system-get,
//!   coding-system-put, coding-system-base, coding-system-eol-type,
//!   coding-system-type, coding-system-change-eol-conversion,
//!   coding-system-change-text-conversion,
//!   detect-coding-string, detect-coding-region, keyboard-coding-system,
//!   terminal-coding-system, set-keyboard-coding-system,
//!   set-terminal-coding-system, coding-system-priority-list

use super::error::{EvalResult, Flow, signal};
use super::eval::Context;
use super::intern::{SymId, intern, lookup_interned, resolve_sym};
use super::value::*;
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// Argument helpers (local to this module)
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

fn expect_integer_or_marker(val: &Value) -> Result<(), Flow> {
    if val.is_marker() {
        return Ok(());
    }
    match val.kind() {
        ValueKind::Fixnum(_) => Ok(()),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *val],
        )),
    }
}

fn is_known_or_derived_coding_system(mgr: &CodingSystemManager, name: &str) -> bool {
    resolve_runtime_name(mgr, name).is_some()
}

fn normalize_keyboard_coding_system(name: &str) -> String {
    if let Some(eol) = EolType::from_suffix(name) {
        let base = strip_eol_suffix(name);
        return match eol {
            EolType::Unix => match base {
                "binary" | "no-conversion" => base.to_string(),
                _ => format!("{base}-unix"),
            },
            EolType::Dos | EolType::Mac => normalize_keyboard_coding_system(base),
            EolType::Undecided => unreachable!("suffix-based eol cannot be undecided"),
        };
    }
    match name {
        "binary" | "no-conversion" => name.to_string(),
        "emacs-internal" => "emacs-internal".to_string(),
        "ascii" | "us-ascii" => "us-ascii-unix".to_string(),
        "latin-1" | "iso-8859-1" | "iso-latin-1" => "iso-latin-1-unix".to_string(),
        "latin-5" | "iso-8859-9" | "iso-latin-5" => "iso-latin-5-unix".to_string(),
        "latin-0" | "latin-9" | "iso-8859-15" | "iso-latin-9" => "iso-latin-9-unix".to_string(),
        _ => format!("{name}-unix"),
    }
}

/// Extract a coding system name from a symbol or string argument.
#[cfg(test)]
fn coding_system_name(val: &Value) -> Result<String, Flow> {
    match val.kind() {
        ValueKind::Symbol(id) => Ok(resolve_sym(id).to_owned()),
        ValueKind::String => coding_runtime_string(val),
        ValueKind::Nil => Ok("nil".to_string()),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *val],
        )),
    }
}

/// Extract a coding system name from a symbol-like argument.
/// Accepts symbols, keywords, nil, and t.
fn coding_symbol_name(val: &Value) -> Result<String, Flow> {
    match val.as_symbol_name() {
        Some(name) => Ok(name.to_string()),
        None => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *val],
        )),
    }
}

fn coding_runtime_string(value: &Value) -> Result<String, Flow> {
    value.as_runtime_string_owned().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )
    })
}

// ---------------------------------------------------------------------------
// EOL types
// ---------------------------------------------------------------------------

/// End-of-line conversion types matching Emacs conventions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EolType {
    /// LF (Unix) -- value 0
    Unix,
    /// CRLF (DOS/Windows) -- value 1
    Dos,
    /// CR (Classic Mac) -- value 2
    Mac,
    /// Undecided / detect automatically
    Undecided,
}

impl EolType {
    pub fn to_int(&self) -> i64 {
        match self {
            EolType::Unix => 0,
            EolType::Dos => 1,
            EolType::Mac => 2,
            EolType::Undecided => 0,
        }
    }

    pub fn to_symbol(&self) -> Value {
        match self {
            EolType::Unix => Value::symbol("unix"),
            EolType::Dos => Value::symbol("dos"),
            EolType::Mac => Value::symbol("mac"),
            EolType::Undecided => Value::symbol("undecided"),
        }
    }

    pub fn suffix(&self) -> &'static str {
        match self {
            EolType::Unix => "-unix",
            EolType::Dos => "-dos",
            EolType::Mac => "-mac",
            EolType::Undecided => "",
        }
    }

    pub fn from_suffix(name: &str) -> Option<EolType> {
        if name.ends_with("-unix") {
            Some(EolType::Unix)
        } else if name.ends_with("-dos") {
            Some(EolType::Dos)
        } else if name.ends_with("-mac") {
            Some(EolType::Mac)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// CodingSystemInfo
// ---------------------------------------------------------------------------

/// Information about a single coding system.
#[derive(Clone, Debug)]
pub struct CodingSystemInfo {
    /// Canonical name of the coding system (e.g. "utf-8").
    pub name: SymId,
    /// Type category (e.g. "utf-8", "charset", "raw-text", "undecided").
    pub coding_type: SymId,
    /// Mnemonic character shown in the mode line.
    pub mnemonic: char,
    /// End-of-line conversion type.
    pub eol_type: EolType,
    /// Whether this coding system is ASCII compatible.
    pub ascii_compatible_p: bool,
    /// Charset list (names of supported charsets).
    pub charset_list: Vec<SymId>,
    /// Post-read conversion function name.
    pub post_read_conversion: Option<SymId>,
    /// Pre-write conversion function name.
    pub pre_write_conversion: Option<SymId>,
    /// Default character for encoding.
    pub default_char: Option<char>,
    /// Whether this is for unibyte buffers.
    pub for_unibyte: bool,
    /// Arbitrary property list for coding-system-get / coding-system-put.
    pub properties: HashMap<SymId, Value>,
    /// Integer property slots used by coding-system-get / coding-system-put.
    pub int_properties: HashMap<i64, Value>,
}

impl CodingSystemInfo {
    fn new(name: &str, coding_type: &str, mnemonic: char, eol_type: EolType) -> Self {
        Self {
            name: intern(name),
            coding_type: intern(coding_type),
            mnemonic,
            eol_type,
            ascii_compatible_p: false,
            charset_list: Vec::new(),
            post_read_conversion: None,
            pre_write_conversion: None,
            default_char: None,
            for_unibyte: false,
            properties: HashMap::new(),
            int_properties: HashMap::new(),
        }
    }

    /// Return the base name (strip -unix/-dos/-mac suffix).
    #[cfg(test)]
    fn base_name(&self) -> String {
        let name = resolve_sym(self.name);
        for suffix in &["-unix", "-dos", "-mac"] {
            if name.ends_with(suffix) {
                return name[..name.len() - suffix.len()].to_string();
            }
        }
        name.to_string()
    }
}

// ---------------------------------------------------------------------------
// CodingSystemManager
// ---------------------------------------------------------------------------

/// Central registry for all coding systems and their aliases.
pub struct CodingSystemManager {
    /// Registered coding systems, keyed by canonical name.
    pub systems: HashMap<SymId, CodingSystemInfo>,
    /// Alias -> canonical name mapping.
    pub aliases: HashMap<SymId, SymId>,
    /// Detection priority list (ordered list of system names).
    pub priority: Vec<SymId>,
    /// Current keyboard coding system.
    keyboard_coding: SymId,
    /// Current terminal coding system.
    terminal_coding: SymId,
}

impl CodingSystemManager {
    /// Create a new manager pre-populated with the standard coding systems.
    pub fn new() -> Self {
        let mut mgr = Self {
            systems: HashMap::new(),
            aliases: HashMap::new(),
            priority: Vec::new(),
            keyboard_coding: intern("utf-8-unix"),
            terminal_coding: intern("utf-8-unix"),
        };

        // Register standard coding systems
        mgr.register(CodingSystemInfo::new(
            "utf-8",
            "utf-8",
            'U',
            EolType::Undecided,
        ));
        mgr.register(CodingSystemInfo::new(
            "utf-8-unix",
            "utf-8",
            'U',
            EolType::Unix,
        ));
        mgr.register(CodingSystemInfo::new(
            "utf-8-dos",
            "utf-8",
            'U',
            EolType::Dos,
        ));
        mgr.register(CodingSystemInfo::new(
            "utf-8-mac",
            "utf-8",
            'U',
            EolType::Mac,
        ));
        mgr.register(CodingSystemInfo::new(
            "iso-latin-1",
            "charset",
            'l',
            EolType::Undecided,
        ));
        mgr.register(CodingSystemInfo::new(
            "iso-latin-1-unix",
            "charset",
            'l',
            EolType::Unix,
        ));
        mgr.register(CodingSystemInfo::new(
            "iso-latin-1-dos",
            "charset",
            'l',
            EolType::Dos,
        ));
        mgr.register(CodingSystemInfo::new(
            "iso-latin-1-mac",
            "charset",
            'l',
            EolType::Mac,
        ));
        mgr.register(CodingSystemInfo::new(
            "iso-latin-5",
            "charset",
            '9',
            EolType::Undecided,
        ));
        mgr.register(CodingSystemInfo::new(
            "iso-latin-5-unix",
            "charset",
            '9',
            EolType::Unix,
        ));
        mgr.register(CodingSystemInfo::new(
            "iso-latin-5-dos",
            "charset",
            '9',
            EolType::Dos,
        ));
        mgr.register(CodingSystemInfo::new(
            "iso-latin-5-mac",
            "charset",
            '9',
            EolType::Mac,
        ));
        mgr.register(CodingSystemInfo::new(
            "iso-latin-9",
            "charset",
            '0',
            EolType::Undecided,
        ));
        mgr.register(CodingSystemInfo::new(
            "iso-latin-9-unix",
            "charset",
            '0',
            EolType::Unix,
        ));
        mgr.register(CodingSystemInfo::new(
            "iso-latin-9-dos",
            "charset",
            '0',
            EolType::Dos,
        ));
        mgr.register(CodingSystemInfo::new(
            "iso-latin-9-mac",
            "charset",
            '0',
            EolType::Mac,
        ));
        mgr.register(CodingSystemInfo::new(
            "us-ascii",
            "charset",
            'A',
            EolType::Undecided,
        ));
        mgr.register(CodingSystemInfo::new(
            "us-ascii-unix",
            "charset",
            'A',
            EolType::Unix,
        ));
        mgr.register(CodingSystemInfo::new(
            "us-ascii-dos",
            "charset",
            'A',
            EolType::Dos,
        ));
        mgr.register(CodingSystemInfo::new(
            "us-ascii-mac",
            "charset",
            'A',
            EolType::Mac,
        ));
        mgr.register(CodingSystemInfo::new(
            "binary",
            "raw-text",
            '=',
            EolType::Unix,
        ));
        mgr.register(CodingSystemInfo::new(
            "raw-text",
            "raw-text",
            '=',
            EolType::Undecided,
        ));
        mgr.register(CodingSystemInfo::new(
            "raw-text-unix",
            "raw-text",
            '=',
            EolType::Unix,
        ));
        mgr.register(CodingSystemInfo::new(
            "raw-text-dos",
            "raw-text",
            '=',
            EolType::Dos,
        ));
        mgr.register(CodingSystemInfo::new(
            "raw-text-mac",
            "raw-text",
            '=',
            EolType::Mac,
        ));
        mgr.register(CodingSystemInfo::new(
            "undecided",
            "undecided",
            '-',
            EolType::Undecided,
        ));
        mgr.register(CodingSystemInfo::new(
            "undecided-unix",
            "undecided",
            '-',
            EolType::Unix,
        ));
        mgr.register(CodingSystemInfo::new(
            "undecided-dos",
            "undecided",
            '-',
            EolType::Dos,
        ));
        mgr.register(CodingSystemInfo::new(
            "undecided-mac",
            "undecided",
            '-',
            EolType::Mac,
        ));
        mgr.register(CodingSystemInfo::new(
            "emacs-internal",
            "utf-8",
            'U',
            EolType::Unix,
        ));
        mgr.register(CodingSystemInfo::new(
            "utf-8-emacs",
            "utf-8",
            'U',
            EolType::Undecided,
        ));
        mgr.register(CodingSystemInfo::new(
            "no-conversion",
            "raw-text",
            '=',
            EolType::Unix,
        ));
        mgr.register(CodingSystemInfo::new(
            "utf-8-auto",
            "utf-8",
            'U',
            EolType::Undecided,
        ));
        mgr.register(CodingSystemInfo::new(
            "utf-16",
            "utf-16",
            'U',
            EolType::Undecided,
        ));
        mgr.register(CodingSystemInfo::new(
            "utf-16-be",
            "utf-16",
            'U',
            EolType::Undecided,
        ));
        mgr.register(CodingSystemInfo::new(
            "utf-16-le",
            "utf-16",
            'U',
            EolType::Undecided,
        ));
        mgr.register(CodingSystemInfo::new(
            "utf-16be",
            "utf-16",
            'U',
            EolType::Undecided,
        ));
        mgr.register(CodingSystemInfo::new(
            "utf-16le",
            "utf-16",
            'U',
            EolType::Undecided,
        ));
        mgr.register(CodingSystemInfo::new(
            "utf-16be-with-signature",
            "utf-16",
            'U',
            EolType::Undecided,
        ));
        mgr.register(CodingSystemInfo::new(
            "utf-16le-with-signature",
            "utf-16",
            'U',
            EolType::Undecided,
        ));
        mgr.register(CodingSystemInfo::new(
            "prefer-utf-8",
            "undecided",
            '-',
            EolType::Undecided,
        ));

        // Common aliases
        mgr.add_alias("mule-utf-8", "utf-8");
        mgr.add_alias("cp65001", "utf-8");
        mgr.add_alias("iso-8859-1", "iso-latin-1");
        mgr.add_alias("latin-1", "iso-latin-1");
        mgr.add_alias("iso-8859-9", "iso-latin-5");
        mgr.add_alias("latin-5", "iso-latin-5");
        mgr.add_alias("iso-8859-15", "iso-latin-9");
        mgr.add_alias("latin-9", "iso-latin-9");
        mgr.add_alias("latin-0", "iso-latin-9");
        mgr.add_alias("ascii", "us-ascii");
        mgr.add_alias("iso-safe", "us-ascii");

        // Default priority list
        mgr.priority = vec![
            intern("utf-8"),
            intern("utf-8-unix"),
            intern("undecided"),
            intern("iso-latin-1"),
            intern("us-ascii"),
            intern("raw-text"),
            intern("binary"),
            intern("no-conversion"),
        ];

        mgr
    }

    /// Register a coding system.
    fn register(&mut self, info: CodingSystemInfo) {
        self.systems.insert(info.name, info);
    }

    /// Resolve a name through the alias table to a canonical name.
    /// Returns either the input name (if it's a direct system) or the
    /// canonical name from the alias table.
    pub fn resolve(&self, name: &str) -> Option<SymId> {
        let name = lookup_interned(name)?;
        if self.systems.contains_key(&name) {
            Some(name)
        } else {
            self.aliases
                .get(&name)
                .copied()
                .filter(|canonical| self.systems.contains_key(canonical))
        }
    }

    /// Look up a coding system by name (resolving aliases).
    pub fn get(&self, name: &str) -> Option<&CodingSystemInfo> {
        let canonical = self.resolve(name)?;
        self.systems.get(&canonical)
    }

    /// Look up a coding system mutably by name (resolving aliases).
    pub fn get_mut(&mut self, name: &str) -> Option<&mut CodingSystemInfo> {
        let canonical = self.resolve(name)?;
        self.systems.get_mut(&canonical)
    }

    /// Check if a name is a known coding system (or alias).
    pub fn is_known(&self, name: &str) -> bool {
        self.resolve(name).is_some()
    }

    /// Add an alias mapping.
    pub fn add_alias(&mut self, alias: &str, target: &str) {
        self.aliases.insert(intern(alias), intern(target));
    }

    /// Get all aliases that point to a given canonical name.
    pub fn aliases_for(&self, canonical: SymId) -> Vec<SymId> {
        self.aliases
            .iter()
            .filter(|(_, v)| **v == canonical)
            .map(|(k, _)| *k)
            .collect()
    }

    /// List all registered coding system names (canonical only).
    pub fn list_all(&self) -> Vec<SymId> {
        let mut names: Vec<SymId> = self.systems.keys().copied().collect();
        names.sort_by(|left, right| resolve_sym(*left).cmp(resolve_sym(*right)));
        names
    }

    pub(crate) fn keyboard_coding_sym(&self) -> SymId {
        self.keyboard_coding
    }
    pub(crate) fn terminal_coding_sym(&self) -> SymId {
        self.terminal_coding
    }
    // pdump accessors
    pub(crate) fn dump_keyboard_coding(&self) -> &str {
        resolve_sym(self.keyboard_coding)
    }
    pub(crate) fn dump_terminal_coding(&self) -> &str {
        resolve_sym(self.terminal_coding)
    }
    pub(crate) fn dump_keyboard_coding_sym(&self) -> SymId {
        self.keyboard_coding
    }
    pub(crate) fn dump_terminal_coding_sym(&self) -> SymId {
        self.terminal_coding
    }
    pub(crate) fn from_dump(
        systems: HashMap<SymId, CodingSystemInfo>,
        aliases: HashMap<SymId, SymId>,
        priority: Vec<SymId>,
        keyboard_coding: SymId,
        terminal_coding: SymId,
    ) -> Self {
        Self {
            systems,
            aliases,
            priority,
            keyboard_coding,
            terminal_coding,
        }
    }

    /// Collect GC roots from coding system properties.
    pub fn trace_roots(&self, roots: &mut Vec<Value>) {
        for info in self.systems.values() {
            for value in info.properties.values() {
                roots.push(*value);
            }
            for value in info.int_properties.values() {
                roots.push(*value);
            }
        }
    }
}

impl crate::gc_trace::GcTrace for CodingSystemManager {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        CodingSystemManager::trace_roots(self, roots);
    }
}

fn property_lookup(info: &CodingSystemInfo, prop: SymId) -> Option<Value> {
    if let Some(value) = info.properties.get(&prop) {
        return Some(*value);
    }
    let prop_name = resolve_sym(prop);
    if !prop_name.starts_with(':') {
        let colon_key = intern(&format!(":{prop_name}"));
        return info.properties.get(&colon_key).copied();
    }
    None
}

fn plist_push_key(plist: &mut Vec<Value>, key: SymId, value: Value) {
    plist.push(Value::from_sym_id(key));
    plist.push(value);
}

fn first_emacs_char_code(value: Value) -> Option<i64> {
    match value.kind() {
        ValueKind::Fixnum(c) => Some(c),
        ValueKind::String => {
            let string = value.as_lisp_string()?;
            if string.is_empty() {
                return Some(0);
            }
            let (ch, _) = if string.is_multibyte() {
                super::emacs_char::string_char(string.as_bytes())
            } else {
                (string.as_bytes()[0] as u32, 1)
            };
            Some(ch as i64)
        }
        _ => None,
    }
}

impl Default for CodingSystemManager {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// Pure builtins
// ===========================================================================

/// `(coding-system-list &optional BASE-ONLY)` -- return a list of all coding systems.
/// If BASE-ONLY is non-nil, only return base systems (no -unix/-dos/-mac variants).
pub(crate) fn builtin_coding_system_list(
    mgr: &CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("coding-system-list", &args, 1)?;
    let base_only = args.first().is_some_and(|v| v.is_truthy());
    let names = mgr.list_all();
    let filtered: Vec<Value> = names
        .into_iter()
        .filter(|id| {
            let n = resolve_sym(*id);
            if base_only {
                !n.ends_with("-unix") && !n.ends_with("-dos") && !n.ends_with("-mac")
            } else {
                true
            }
        })
        .map(Value::symbol)
        .collect();
    Ok(Value::list(filtered))
}

/// `(coding-system-aliases CODING-SYSTEM)` -- return a list of aliases for a
/// coding system (including the name itself as the first element).
pub(crate) fn builtin_coding_system_aliases(
    mgr: &CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("coding-system-aliases", &args, 1)?;
    if args[0].is_string() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    }
    let raw_name = coding_symbol_name(&args[0])?;
    let resolved_name = resolve_runtime_name(mgr, &raw_name)
        .ok_or_else(|| signal("coding-system-error", vec![args[0]]))?;
    let base = strip_eol_suffix(&resolved_name);

    if matches!(base, "binary" | "no-conversion") {
        return Ok(Value::list(vec![
            Value::symbol("no-conversion"),
            Value::symbol("binary"),
        ]));
    }

    let suffix = EolType::from_suffix(&resolved_name)
        .map(|eol| eol.suffix())
        .unwrap_or("");
    let canonical = runtime_bucket_name(mgr, &resolved_name)
        .ok_or_else(|| signal("coding-system-error", vec![args[0]]))?;
    let display = display_base_name(strip_eol_suffix(&resolved_name)).to_string();
    let canonical_id = mgr
        .resolve(&canonical)
        .ok_or_else(|| signal("coding-system-error", vec![args[0]]))?;
    let mut aliases = mgr.aliases_for(canonical_id);
    aliases.sort_by(|a, b| {
        alias_sort_rank(&canonical, resolve_sym(*a))
            .cmp(&alias_sort_rank(&canonical, resolve_sym(*b)))
            .then_with(|| resolve_sym(*a).cmp(resolve_sym(*b)))
    });
    let mut names = vec![format!("{display}{suffix}")];
    for alias in aliases {
        let alias = resolve_sym(alias);
        if alias != display {
            names.push(format!("{alias}{suffix}"));
        }
    }
    if canonical != display
        && !names
            .iter()
            .any(|name| name == &format!("{canonical}{suffix}"))
    {
        names.push(format!("{canonical}{suffix}"));
    }
    Ok(Value::list(names.into_iter().map(Value::symbol).collect()))
}

/// `(coding-system-get CODING-SYSTEM PROP)` -- get a property of a coding system.
/// Recognized built-in properties: :name, :type, :mnemonic, :eol-type.
/// Other properties are looked up from the per-system property list.
pub(crate) fn builtin_coding_system_get(mgr: &CodingSystemManager, args: Vec<Value>) -> EvalResult {
    expect_args("coding-system-get", &args, 2)?;
    let coding_name = coding_symbol_name(&args[0])?;
    let resolved_name = resolve_runtime_name(mgr, &coding_name)
        .ok_or_else(|| signal("coding-system-error", vec![args[0]]))?;
    let bucket = runtime_bucket_name(mgr, &resolved_name)
        .ok_or_else(|| signal("coding-system-error", vec![args[0]]))?;
    let info = mgr
        .get(&bucket)
        .ok_or_else(|| signal("coding-system-error", vec![args[0]]))?;

    if let Some(prop_id) = args[1].as_symbol_id() {
        let prop_name = resolve_sym(prop_id);
        if let Some(value) = property_lookup(info, prop_id) {
            return Ok(value);
        }
        return match prop_name {
            ":name" | "name" => Ok(Value::symbol(display_base_name(strip_eol_suffix(
                &resolved_name,
            )))),
            ":coding-type" | "coding-type" => Ok(Value::symbol(
                coding_type_for_base(strip_eol_suffix(&resolved_name))
                    .unwrap_or(resolve_sym(info.coding_type)),
            )),
            ":type" | "type" => Ok(Value::NIL),
            ":mnemonic" | "mnemonic" => Ok(Value::fixnum(
                default_mnemonic_for_base(strip_eol_suffix(&resolved_name))
                    .unwrap_or(info.mnemonic as i64),
            )),
            ":charset-list" | "charset-list" => Ok(Value::list(
                info.charset_list
                    .iter()
                    .copied()
                    .map(Value::from_sym_id)
                    .collect(),
            )),
            ":post-read-conversion" | "post-read-conversion" => Ok(info
                .post_read_conversion
                .map(Value::from_sym_id)
                .unwrap_or(Value::NIL)),
            ":pre-write-conversion" | "pre-write-conversion" => Ok(info
                .pre_write_conversion
                .map(Value::from_sym_id)
                .unwrap_or(Value::NIL)),
            ":eol-type" | "eol-type" => Ok(Value::NIL),
            _ => Ok(Value::NIL),
        };
    }

    if let Some(int_key) = args[1].as_int() {
        if let Some(value) = info.int_properties.get(&int_key) {
            return Ok(*value);
        }
    }

    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("symbolp"), args[1]],
    ))
}

fn plist_contains_key(plist: &[Value], key: &str) -> bool {
    let needle = key.trim_start_matches(':');
    let mut idx = 0;
    while idx + 1 < plist.len() {
        if plist[idx]
            .as_symbol_name()
            .is_some_and(|name| name.trim_start_matches(':') == needle)
        {
            return true;
        }
        idx += 2;
    }
    false
}

fn coding_category_for_base(base: &str) -> &'static str {
    match base {
        "utf-8" | "utf-8-emacs" | "utf-8-auto" | "emacs-internal" => "coding-category-utf-8",
        "latin-1" | "iso-8859-1" | "iso-latin-1" | "latin-5" | "iso-8859-9" | "iso-latin-5"
        | "latin-0" | "latin-9" | "iso-8859-15" | "iso-latin-9" | "ascii" | "us-ascii" => {
            "coding-category-charset"
        }
        "raw-text" | "binary" | "no-conversion" => "coding-category-raw-text",
        "undecided" | "prefer-utf-8" => "coding-category-undecided",
        _ => "coding-category-undecided",
    }
}

fn coding_docstring_for_base(base: &str) -> Option<&'static str> {
    match base {
        "utf-8" | "utf-8-emacs" | "utf-8-auto" | "emacs-internal" => {
            Some("UTF-8 (no signature (BOM))")
        }
        "latin-1" | "iso-8859-1" | "iso-latin-1" => {
            Some("ISO 2022 based 8-bit encoding for Latin-1 (MIME:ISO-8859-1).")
        }
        "latin-5" | "iso-8859-9" | "iso-latin-5" => {
            Some("ISO 2022 based 8-bit encoding for Latin-5 (MIME:ISO-8859-9).")
        }
        "latin-0" | "latin-9" | "iso-8859-15" | "iso-latin-9" => {
            Some("ISO 2022 based 8-bit encoding for Latin-9 (MIME:ISO-8859-15).")
        }
        "ascii" | "us-ascii" => Some("ASCII encoding."),
        "no-conversion" | "binary" | "raw-text" => Some("Do no conversion."),
        "undecided" => Some("Automatic conversion on decode."),
        _ => None,
    }
}

fn coding_charset_list_for_base(base: &str) -> Option<Vec<Value>> {
    match base {
        "utf-8" | "utf-8-emacs" | "utf-8-auto" | "emacs-internal" => {
            Some(vec![Value::symbol("unicode")])
        }
        "latin-1" | "iso-8859-1" | "iso-latin-1" => Some(vec![Value::symbol("iso-8859-1")]),
        "latin-5" | "iso-8859-9" | "iso-latin-5" => Some(vec![Value::symbol("iso-8859-9")]),
        "latin-0" | "latin-9" | "iso-8859-15" | "iso-latin-9" => {
            Some(vec![Value::symbol("iso-8859-15")])
        }
        "ascii" | "us-ascii" => Some(vec![Value::symbol("ascii")]),
        _ => None,
    }
}

fn coding_mime_charset_for_base(base: &str) -> Option<&'static str> {
    match base {
        "utf-8" | "utf-8-emacs" | "utf-8-auto" | "emacs-internal" => Some("utf-8"),
        "latin-1" | "iso-8859-1" | "iso-latin-1" => Some("iso-8859-1"),
        "latin-5" | "iso-8859-9" | "iso-latin-5" => Some("iso-8859-9"),
        "latin-0" | "latin-9" | "iso-8859-15" | "iso-latin-9" => Some("iso-8859-15"),
        "ascii" | "us-ascii" => Some("us-ascii"),
        _ => None,
    }
}

/// `(coding-system-plist CODING-SYSTEM)` -- return a plist describing
/// CODING-SYSTEM metadata.
pub(crate) fn builtin_coding_system_plist(
    mgr: &CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("coding-system-plist", &args, 1)?;
    if args[0].is_string() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    }

    let coding_name = coding_symbol_name(&args[0])?;
    let resolved_name = resolve_runtime_name(mgr, &coding_name)
        .ok_or_else(|| signal("coding-system-error", vec![args[0]]))?;
    let bucket = runtime_bucket_name(mgr, &resolved_name)
        .ok_or_else(|| signal("coding-system-error", vec![args[0]]))?;
    let info = mgr
        .get(&bucket)
        .ok_or_else(|| signal("coding-system-error", vec![args[0]]))?;

    let base = strip_eol_suffix(&resolved_name);
    let display_name = display_base_name(base);
    let coding_type = coding_type_for_base(base).unwrap_or(resolve_sym(info.coding_type));
    let mnemonic = default_mnemonic_for_base(base).unwrap_or(info.mnemonic as i64);

    let mut plist = Vec::new();
    plist_push_key(&mut plist, intern(":ascii-compatible-p"), Value::T);
    plist_push_key(
        &mut plist,
        intern(":category"),
        Value::symbol(coding_category_for_base(base)),
    );
    plist_push_key(&mut plist, intern(":name"), Value::symbol(display_name));
    if let Some(doc) = coding_docstring_for_base(base) {
        plist_push_key(&mut plist, intern(":docstring"), Value::string(doc));
    }
    plist_push_key(
        &mut plist,
        intern(":coding-type"),
        Value::symbol(coding_type),
    );
    plist_push_key(&mut plist, intern(":mnemonic"), Value::fixnum(mnemonic));
    if let Some(charset_list) = coding_charset_list_for_base(base).or_else(|| {
        (!info.charset_list.is_empty()).then(|| {
            info.charset_list
                .iter()
                .copied()
                .map(Value::from_sym_id)
                .collect()
        })
    }) {
        plist_push_key(
            &mut plist,
            intern(":charset-list"),
            Value::list(charset_list),
        );
    }
    if let Some(mime_charset) = coding_mime_charset_for_base(base) {
        plist_push_key(
            &mut plist,
            intern(":mime-charset"),
            Value::symbol(mime_charset),
        );
    }
    if let Some(post_read_conversion) = info.post_read_conversion {
        plist_push_key(
            &mut plist,
            intern(":post-read-conversion"),
            Value::from_sym_id(post_read_conversion),
        );
    }
    if let Some(pre_write_conversion) = info.pre_write_conversion {
        plist_push_key(
            &mut plist,
            intern(":pre-write-conversion"),
            Value::from_sym_id(pre_write_conversion),
        );
    }
    if matches!(base, "no-conversion" | "binary" | "raw-text") {
        plist_push_key(&mut plist, intern(":default-char"), Value::fixnum(0));
        plist_push_key(&mut plist, intern(":for-unibyte"), Value::T);
    }

    // Preserve caller-provided custom properties from coding-system-put.
    let mut custom_keys: Vec<SymId> = info.properties.keys().copied().collect();
    custom_keys.sort_by(|left, right| resolve_sym(*left).cmp(resolve_sym(*right)));
    for key in custom_keys {
        let key_name = resolve_sym(key);
        if !plist_contains_key(&plist, key_name) {
            if let Some(value) = info.properties.get(&key) {
                plist_push_key(&mut plist, key, *value);
            }
        }
    }

    Ok(Value::list(plist))
}

/// `(coding-system-put CODING-SYSTEM PROP VAL)` -- set a property of a coding system.
pub(crate) fn builtin_coding_system_put(
    mgr: &mut CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("coding-system-put", &args, 3)?;
    let val = args[2];

    if args[0].is_nil() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("coding-system-p"), Value::NIL],
        ));
    }

    let coding_name = coding_symbol_name(&args[0])?;
    let resolved_name = resolve_runtime_name(mgr, &coding_name)
        .ok_or_else(|| signal("coding-system-error", vec![args[0]]))?;
    let bucket = runtime_bucket_name(mgr, &resolved_name)
        .ok_or_else(|| signal("coding-system-error", vec![args[0]]))?;
    let info = mgr
        .get_mut(&bucket)
        .ok_or_else(|| signal("coding-system-error", vec![args[0]]))?;

    if let Some(prop_id) = args[1].as_symbol_id() {
        let prop_name = resolve_sym(prop_id);
        if matches!(prop_name, ":mnemonic" | "mnemonic") {
            let Some(code) = first_emacs_char_code(val) else {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("characterp"), val],
                ));
            };
            let coerced = Value::fixnum(code);
            info.properties.insert(prop_id, coerced);
            return Ok(coerced);
        }
        info.properties.insert(prop_id, val);
        return Ok(val);
    }

    if let Some(int_key) = args[1].as_int() {
        info.int_properties.insert(int_key, val);
        return Ok(val);
    }

    Ok(val)
}

/// `(coding-system-base CODING-SYSTEM)` -- return the base coding system
/// (stripping -unix, -dos, -mac suffixes).
pub(crate) fn builtin_coding_system_base(
    mgr: &CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("coding-system-base", &args, 1)?;
    let name = coding_symbol_name(&args[0])?;
    let resolved_name = resolve_runtime_name(mgr, &name)
        .ok_or_else(|| signal("coding-system-error", vec![args[0]]))?;
    Ok(Value::symbol(display_base_name(strip_eol_suffix(
        &resolved_name,
    ))))
}

/// `(coding-system-eol-type CODING-SYSTEM)` -- return the EOL type.
/// Returns 0 (unix), 1 (dos), 2 (mac), or a vector of three sub-coding-systems
/// if the EOL type is undecided.
pub(crate) fn builtin_coding_system_eol_type(
    mgr: &CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("coding-system-eol-type", &args, 1)?;
    let Some(name) = args[0].as_symbol_name() else {
        return Ok(Value::NIL);
    };
    let resolved_name = match resolve_runtime_name(mgr, name) {
        Some(resolved) => resolved,
        None => return Ok(Value::NIL),
    };
    if let Some(eol) = EolType::from_suffix(&resolved_name) {
        return Ok(Value::fixnum(eol.to_int()));
    }
    let bucket = match runtime_bucket_name(mgr, &resolved_name) {
        Some(bucket) => bucket,
        None => return Ok(Value::NIL),
    };
    let Some(info) = mgr.get(&bucket) else {
        return Ok(Value::NIL);
    };

    match info.eol_type {
        EolType::Unix => Ok(Value::fixnum(0)),
        EolType::Dos => Ok(Value::fixnum(1)),
        EolType::Mac => Ok(Value::fixnum(2)),
        EolType::Undecided => {
            // Return [base-unix base-dos base-mac] using Emacs display base names.
            let base = eol_vector_base(strip_eol_suffix(&resolved_name));
            let vec = vec![
                Value::symbol(format!("{base}-unix")),
                Value::symbol(format!("{base}-dos")),
                Value::symbol(format!("{base}-mac")),
            ];
            Ok(Value::vector(vec))
        }
    }
}

/// `(coding-system-type CODING-SYSTEM)` -- return the type symbol of the
/// coding system (e.g. utf-8, charset, raw-text, undecided).
pub(crate) fn builtin_coding_system_type(
    mgr: &CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("coding-system-type", &args, 1)?;
    let name = coding_symbol_name(&args[0])?;
    let resolved_name = resolve_runtime_name(mgr, &name)
        .ok_or_else(|| signal("coding-system-error", vec![args[0]]))?;
    let base = strip_eol_suffix(&resolved_name);
    if let Some(kind) = coding_type_for_base(base) {
        return Ok(Value::symbol(kind));
    }
    let bucket = runtime_bucket_name(mgr, &resolved_name)
        .ok_or_else(|| signal("coding-system-error", vec![args[0]]))?;
    let info = mgr
        .get(&bucket)
        .ok_or_else(|| signal("coding-system-error", vec![args[0]]))?;
    Ok(Value::from_sym_id(info.coding_type))
}

/// `(coding-system-change-eol-conversion CODING-SYSTEM EOL-TYPE)` -- return
/// a coding system derived from CODING-SYSTEM but with a different EOL type.
/// EOL-TYPE is 0 (unix), 1 (dos), or 2 (mac), or a symbol.
pub(crate) fn builtin_coding_system_change_eol_conversion(
    mgr: &CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("coding-system-change-eol-conversion", &args, 2)?;
    if args[0].is_string() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    }
    let raw_name = coding_symbol_name(&args[0])?;
    let is_nil_coding = raw_name == "nil";
    if !is_nil_coding && resolve_runtime_name(mgr, &raw_name).is_none() {
        return Err(signal("coding-system-error", vec![args[0]]));
    }
    let canonical_name = if is_nil_coding {
        "no-conversion".to_string()
    } else {
        canonical_runtime_name(mgr, &raw_name)
            .ok_or_else(|| signal("coding-system-error", vec![args[0]]))?
    };
    let canonical_base = strip_eol_suffix(&canonical_name);
    let resolved_base = canonical_base;
    let no_conversion_family = is_nil_coding || matches!(resolved_base, "no-conversion" | "binary");

    if no_conversion_family {
        let out = match args[1].kind() {
            ValueKind::Nil => Value::symbol("no-conversion"),
            ValueKind::Fixnum(n) => {
                if n == 0 {
                    if is_nil_coding {
                        Value::NIL
                    } else if resolved_base == "binary" {
                        Value::symbol("binary")
                    } else {
                        Value::symbol("no-conversion")
                    }
                } else {
                    Value::NIL
                }
            }
            ValueKind::Float => {
                if args[1].xfloat() == 0.0 {
                    if is_nil_coding {
                        Value::NIL
                    } else if resolved_base == "binary" {
                        Value::symbol("binary")
                    } else {
                        Value::symbol("no-conversion")
                    }
                } else {
                    Value::NIL
                }
            }
            ValueKind::Symbol(id) if resolve_sym(id) == "unix" => {
                if is_nil_coding {
                    Value::NIL
                } else if resolved_base == "binary" {
                    Value::symbol("binary")
                } else {
                    Value::symbol("no-conversion")
                }
            }
            ValueKind::Symbol(id)
                if {
                    let n = resolve_sym(id);
                    n == "dos" || n == "mac"
                } =>
            {
                Value::NIL
            }
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), args[1]],
                ));
            }
        };
        return Ok(out);
    }

    let target_eol = match args[1].kind() {
        ValueKind::Nil => None,
        ValueKind::Fixnum(n) => Some(n),
        ValueKind::Symbol(id) if resolve_sym(id) == "unix" => Some(0),
        ValueKind::Symbol(id) if resolve_sym(id) == "dos" => Some(1),
        ValueKind::Symbol(id) if resolve_sym(id) == "mac" => Some(2),
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("fixnump"), args[1]],
            ));
        }
    };

    let raw_base = strip_eol_suffix(&raw_name);
    if target_eol.is_none() {
        if EolType::from_suffix(&raw_name).is_some() {
            return Ok(Value::symbol(display_base_name(canonical_base)));
        }
        if canonical_base == "emacs-internal" {
            return Ok(Value::symbol("utf-8-emacs"));
        }
        return Ok(Value::symbol(raw_name));
    }

    let eol = target_eol.expect("checked above");
    if !(0..=2).contains(&eol) {
        let vec_base = eol_vector_base(resolved_base);
        let variants = vec![
            Value::symbol(format!("{vec_base}-unix")),
            Value::symbol(format!("{vec_base}-dos")),
            Value::symbol(format!("{vec_base}-mac")),
        ];
        return Err(signal(
            "args-out-of-range",
            vec![Value::vector(variants), Value::fixnum(eol)],
        ));
    }

    if let Some(derived) = derive_coding_for_eol(canonical_base, eol) {
        Ok(Value::symbol(derived))
    } else {
        Ok(Value::NIL)
    }
}

/// `(coding-system-change-text-conversion CODING-SYSTEM TEXT-CODING)` -- return
/// a coding system derived from TEXT-CODING but preserving the EOL type of
/// CODING-SYSTEM.
pub(crate) fn builtin_coding_system_change_text_conversion(
    mgr: &CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("coding-system-change-text-conversion", &args, 2)?;
    let first_eol = match args[0].kind() {
        ValueKind::Nil => Some(0),
        ValueKind::String => None,
        _ => match args[0].as_symbol_name() {
            Some(name) if name == "nil" => Some(0),
            Some(name) => {
                if let Some(resolved) = resolve_runtime_name(mgr, name) {
                    if let Some(eol) = EolType::from_suffix(&resolved) {
                        Some(eol.to_int())
                    } else if let Some(info) = mgr.get(&resolved) {
                        match info.eol_type {
                            EolType::Unix => Some(0),
                            EolType::Dos => Some(1),
                            EolType::Mac => Some(2),
                            EolType::Undecided => None,
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            None => None,
        },
    };

    let text_raw = coding_symbol_name(&args[1])?;
    let text_name = if text_raw == "nil" {
        "undecided".to_string()
    } else {
        text_raw.clone()
    };
    let resolved_text = resolve_runtime_name(mgr, &text_name)
        .ok_or_else(|| signal("coding-system-error", vec![args[1]]))?;
    let resolved_text_base = strip_eol_suffix(&resolved_text);

    if let Some(eol) = first_eol {
        if let Some(derived) = derive_coding_for_eol(resolved_text_base, eol) {
            return Ok(Value::symbol(derived));
        }
        return Ok(Value::NIL);
    }

    if EolType::from_suffix(&text_name).is_some() {
        return Ok(Value::symbol(display_base_name(strip_eol_suffix(
            &text_name,
        ))));
    }

    match text_name.as_str() {
        "binary" => Ok(Value::symbol("no-conversion")),
        "emacs-internal" => Ok(Value::symbol("utf-8-emacs")),
        _ => Ok(Value::symbol(text_name)),
    }
}

/// `(coding-system-p OBJECT)` -- return t when OBJECT names a known coding
/// system or alias, nil otherwise.
pub(crate) fn builtin_coding_system_p(mgr: &CodingSystemManager, args: Vec<Value>) -> EvalResult {
    expect_args("coding-system-p", &args, 1)?;
    let known = match args[0].as_symbol_name() {
        Some(name) if name == "nil" => true,
        Some(name) => is_known_or_derived_coding_system(mgr, name),
        None => false,
    };
    Ok(Value::bool_val(known))
}

/// `(check-coding-system CODING-SYSTEM)` -- validate CODING-SYSTEM.
/// Returns CODING-SYSTEM when valid, nil for nil, and signals on invalid input.
pub(crate) fn builtin_check_coding_system(
    mgr: &CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("check-coding-system", &args, 1)?;
    match args[0].kind() {
        ValueKind::Nil => Ok(Value::NIL),
        ValueKind::Symbol(id) => {
            if is_known_or_derived_coding_system(mgr, resolve_sym(id)) {
                Ok(args[0])
            } else {
                Err(signal("coding-system-error", vec![args[0]]))
            }
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        )),
    }
}

/// `(check-coding-systems-region START END CODING-SYSTEMS)` -- compatibility
/// helper that currently performs argument shape checks and returns nil.
pub(crate) fn builtin_check_coding_systems_region(
    _mgr: &CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("check-coding-systems-region", &args, 3)?;
    expect_integer_or_marker(&args[1])?;
    Ok(Value::NIL)
}

/// `(find-coding-system CODING-SYSTEM)` -- resolve CODING-SYSTEM to a known
/// canonical symbol, or return nil when unknown.
#[cfg(test)]
pub(crate) fn builtin_find_coding_system(
    mgr: &CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("find-coding-system", &args, 1)?;
    let name = coding_system_name(&args[0])?;
    if name == "nil" {
        return Ok(Value::NIL);
    }
    match mgr.resolve(&name) {
        Some(canonical) => Ok(Value::symbol(canonical)),
        None => Ok(Value::NIL),
    }
}

/// `(define-coding-system-internal NAME MNEMONIC CODING-TYPE CHARSET-LIST
///    ASCII-COMPAT DECODE-TL ENCODE-TL POST-READ PRE-WRITE DEFAULT-CHAR
///    FOR-UNIBYTE PLIST EOL-TYPE &rest TYPE-SPECIFIC-ATTRS)`
///
/// Internal entry point for registering a coding system.
/// Called by the `define-coding-system` macro in mule.el with ≥13 positional args.
pub(crate) fn builtin_define_coding_system_internal(
    mgr: &mut CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("define-coding-system-internal", &args, 13)?;

    // arg[0]: name (symbol)
    let name = coding_symbol_name_required(&args[0])?;

    // arg[1]: mnemonic (char)
    let mnemonic = match args[1].kind() {
        ValueKind::Fixnum(c) => super::builtins::character_code_to_rust_char(c).unwrap_or('?'),
        _ => '?',
    };

    // arg[2]: coding-type (symbol)
    let coding_type = match args[2].kind() {
        ValueKind::Symbol(id) => id,
        _ => intern("undecided"),
    };

    // arg[3]: charset-list (list of symbols, or special symbol like 'iso-2022)
    let charset_list = match args[3].kind() {
        ValueKind::Symbol(id) => vec![id],
        _ => {
            if let Some(items) = super::value::list_to_vec(&args[3]) {
                items.iter().filter_map(|v| v.as_symbol_id()).collect()
            } else {
                Vec::new()
            }
        }
    };

    // arg[4]: ascii-compatible-p
    let ascii_compatible_p = args[4].is_truthy();

    // arg[5]: decode-translation-table (ignored for now)
    // arg[6]: encode-translation-table (ignored for now)

    // arg[7]: post-read-conversion
    let post_read_conversion = match args[7].kind() {
        ValueKind::Symbol(id) if resolve_sym(id) != "nil" => Some(id),
        _ => None,
    };

    // arg[8]: pre-write-conversion
    let pre_write_conversion = match args[8].kind() {
        ValueKind::Symbol(id) if resolve_sym(id) != "nil" => Some(id),
        _ => None,
    };

    // arg[9]: default-char
    let default_char = match args[9].kind() {
        ValueKind::Fixnum(c) => char::from_u32(c as u32),
        _ => None,
    };

    // arg[10]: for-unibyte
    let for_unibyte = args[10].is_truthy();

    // arg[11]: plist
    let mut properties = HashMap::new();
    if let Some(items) = super::value::list_to_vec(&args[11]) {
        let mut i = 0;
        while i + 1 < items.len() {
            if let Some(key) = items[i].as_symbol_id() {
                properties.insert(key, items[i + 1]);
            }
            i += 2;
        }
    }

    // arg[12]: eol-type (symbol: unix/dos/mac, or vector for auto-detect)
    let eol_type = match args[12].kind() {
        ValueKind::Symbol(id) => {
            let s = resolve_sym(id);
            match s {
                "unix" => EolType::Unix,
                "dos" => EolType::Dos,
                "mac" => EolType::Mac,
                _ => EolType::Undecided,
            }
        }
        _ => EolType::Undecided,
    };

    // Build the base coding system info.
    let mut info =
        CodingSystemInfo::new(&name, resolve_sym(coding_type), mnemonic, eol_type.clone());
    info.ascii_compatible_p = ascii_compatible_p;
    info.charset_list = charset_list;
    info.post_read_conversion = post_read_conversion;
    info.pre_write_conversion = pre_write_conversion;
    info.default_char = default_char;
    info.for_unibyte = for_unibyte;
    info.properties = properties;

    // Register the base coding system.
    mgr.register(info);

    // Auto-create EOL variants (-unix, -dos, -mac) unless the eol_type
    // is already specific or the name already has an EOL suffix.
    if matches!(eol_type, EolType::Undecided) && EolType::from_suffix(&name).is_none() {
        for (suffix, et) in [
            ("-unix", EolType::Unix),
            ("-dos", EolType::Dos),
            ("-mac", EolType::Mac),
        ] {
            let variant_name = format!("{name}{suffix}");
            if !mgr.is_known(&variant_name) {
                let variant =
                    CodingSystemInfo::new(&variant_name, resolve_sym(coding_type), mnemonic, et);
                mgr.register(variant);
            }
        }
    }

    Ok(Value::NIL)
}

/// Extract a coding system name from a symbol argument, signaling on non-symbol.
fn coding_symbol_name_required(val: &Value) -> Result<String, Flow> {
    match val.kind() {
        ValueKind::Symbol(id) => Ok(resolve_sym(id).to_owned()),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *val],
        )),
    }
}

/// `(define-coding-system-alias ALIAS CODING-SYSTEM)` -- register ALIAS for
/// CODING-SYSTEM and return nil.
pub(crate) fn builtin_define_coding_system_alias(
    mgr: &mut CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("define-coding-system-alias", &args, 2)?;

    let alias = match args[0].kind() {
        ValueKind::Symbol(id) => resolve_sym(id).to_owned(),
        ValueKind::Nil => "nil".to_string(),
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[0]],
            ));
        }
    };

    let target = match args[1].kind() {
        ValueKind::Symbol(id) => resolve_sym(id).to_owned(),
        ValueKind::Nil => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("coding-system-p"), Value::NIL],
            ));
        }
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[1]],
            ));
        }
    };

    let canonical = mgr
        .resolve(&target)
        .ok_or_else(|| signal("coding-system-error", vec![Value::symbol(&target)]))?;
    mgr.add_alias(&alias, resolve_sym(canonical));
    Ok(Value::NIL)
}

/// `(set-coding-system-priority &rest CODING-SYSTEMS)` -- move CODING-SYSTEMS
/// to the front of the detection priority list in order, keeping relative order
/// of the remaining systems.
pub(crate) fn builtin_set_coding_system_priority(
    mgr: &mut CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    if args.is_empty() {
        return Ok(Value::NIL);
    }

    let mut requested: Vec<(String, String)> = Vec::with_capacity(args.len());
    for arg in &args {
        if arg.is_nil() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("coding-system-p"), Value::NIL],
            ));
        }
        let Some(name) = arg.as_symbol_name().map(|s| s.to_string()) else {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), *arg],
            ));
        };
        let resolved = resolve_runtime_name(mgr, &name)
            .ok_or_else(|| signal("coding-system-error", vec![*arg]))?;
        let canonical = mgr
            .resolve(&resolved)
            .map(|id| resolve_sym(id).to_string())
            .unwrap_or(resolved.clone());
        requested.push((name, canonical));
    }

    let mut seen_canonicals: HashSet<SymId> =
        HashSet::with_capacity(mgr.priority.len() + requested.len());
    let mut reordered: Vec<SymId> = Vec::with_capacity(mgr.priority.len() + requested.len());

    for (display, canonical) in requested {
        if let Some(display_id) = lookup_interned(&display)
            && let Some(canonical_id) = lookup_interned(&canonical)
            && seen_canonicals.insert(canonical_id)
        {
            reordered.push(display_id);
        }
    }

    for &name in &mgr.priority {
        let canonical = mgr.resolve(resolve_sym(name)).unwrap_or(name);
        if seen_canonicals.insert(canonical) {
            reordered.push(name);
        }
    }

    mgr.priority = reordered;
    Ok(Value::NIL)
}

/// `(detect-coding-string STRING &optional HIGHEST)` -- detect the encoding of
/// a string. Since all strings in this runtime are UTF-8, always returns utf-8.
/// If HIGHEST is non-nil, return a single coding system; otherwise return a list.
pub(crate) fn builtin_detect_coding_string(
    _mgr: &CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("detect-coding-string", &args, 1)?;
    expect_max_args("detect-coding-string", &args, 2)?;
    match args[0].kind() {
        ValueKind::String => {}
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), args[0]],
            ));
        }
    }
    let highest = args.get(1).is_some_and(|v| v.is_truthy());
    if highest {
        Ok(Value::symbol("undecided"))
    } else {
        Ok(Value::list(vec![Value::symbol("undecided")]))
    }
}

/// `(detect-coding-region START END &optional HIGHEST)` -- detect the encoding
/// of a buffer region. Stub: always returns utf-8.
pub(crate) fn builtin_detect_coding_region(
    _mgr: &CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("detect-coding-region", &args, 2)?;
    expect_max_args("detect-coding-region", &args, 3)?;
    expect_integer_or_marker(&args[0])?;
    expect_integer_or_marker(&args[1])?;
    let highest = args.get(2).is_some_and(|v| v.is_truthy());
    if highest {
        Ok(Value::symbol("undecided"))
    } else {
        Ok(Value::list(vec![Value::symbol("undecided")]))
    }
}

/// `(keyboard-coding-system &optional TERMINAL)` -- return the current
/// keyboard coding system. The TERMINAL argument is ignored.
pub(crate) fn builtin_keyboard_coding_system(
    mgr: &CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("keyboard-coding-system", &args, 1)?;
    Ok(Value::symbol(mgr.keyboard_coding))
}

/// `(terminal-coding-system &optional TERMINAL)` -- return the current
/// terminal coding system. The TERMINAL argument is ignored.
pub(crate) fn builtin_terminal_coding_system(
    mgr: &CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("terminal-coding-system", &args, 1)?;
    Ok(Value::symbol(mgr.terminal_coding))
}

/// `(set-keyboard-coding-system CODING-SYSTEM &optional TERMINAL)` -- set the
/// keyboard coding system. Stub: records the value but has no functional effect.
pub(crate) fn builtin_set_keyboard_coding_system(
    mgr: &mut CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-keyboard-coding-system", &args, 1)?;
    expect_max_args("set-keyboard-coding-system", &args, 2)?;
    if args[0].is_nil() {
        mgr.keyboard_coding = intern("no-conversion");
        return Ok(Value::NIL);
    }
    let Some(name) = args[0].as_symbol_name().map(|s| s.to_string()) else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    };
    if !is_known_or_derived_coding_system(mgr, &name) {
        return Err(signal("coding-system-error", vec![args[0]]));
    }
    let normalization_input = if matches!(EolType::from_suffix(&name), Some(EolType::Unix)) {
        name.clone()
    } else {
        canonical_runtime_name(mgr, &name)
            .ok_or_else(|| signal("coding-system-error", vec![args[0]]))?
    };
    let base = strip_eol_suffix(&normalization_input);
    if matches!(base, "utf-8-auto" | "prefer-utf-8") {
        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Unsuitable coding system for keyboard: {name}"
            ))],
        ));
    }
    if base == "undecided" {
        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Unsupported coding system for keyboard: {normalization_input}"
            ))],
        ));
    }
    let normalized = normalize_keyboard_coding_system(&normalization_input);
    mgr.keyboard_coding = intern(&normalized);
    Ok(Value::symbol(mgr.keyboard_coding))
}

/// `(set-terminal-coding-system CODING-SYSTEM &optional TERMINAL)` -- set the
/// terminal coding system. Stub: records the value but has no functional effect.
pub(crate) fn builtin_set_terminal_coding_system(
    mgr: &mut CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-terminal-coding-system", &args, 1)?;
    expect_max_args("set-terminal-coding-system", &args, 3)?;
    if args[0].is_nil() {
        mgr.terminal_coding = intern("nil");
        return Ok(Value::NIL);
    }
    let Some(name) = args[0].as_symbol_name().map(|s| s.to_string()) else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    };
    if !is_known_or_derived_coding_system(mgr, &name) {
        return Err(signal("coding-system-error", vec![args[0]]));
    }
    mgr.terminal_coding = intern(&name);
    Ok(Value::NIL)
}

/// `(set-keyboard-coding-system-internal CODING-SYSTEM &optional TERMINAL)` --
/// internal keyboard coding setter. Mirrors the surface validation but always
/// returns nil.
pub(crate) fn builtin_set_keyboard_coding_system_internal(
    mgr: &mut CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-keyboard-coding-system-internal", &args, 1)?;
    expect_max_args("set-keyboard-coding-system-internal", &args, 2)?;
    let _ = builtin_set_keyboard_coding_system(mgr, args)?;
    Ok(Value::NIL)
}

/// `(set-terminal-coding-system-internal CODING-SYSTEM &optional TERMINAL)` --
/// internal terminal coding setter.
pub(crate) fn builtin_set_terminal_coding_system_internal(
    mgr: &mut CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-terminal-coding-system-internal", &args, 1)?;
    expect_max_args("set-terminal-coding-system-internal", &args, 2)?;
    let _ = builtin_set_terminal_coding_system(mgr, args)?;
    Ok(Value::NIL)
}

/// `(set-safe-terminal-coding-system-internal CODING-SYSTEM)` -- internal safe
/// terminal coding setter.
pub(crate) fn builtin_set_safe_terminal_coding_system_internal(
    mgr: &mut CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-safe-terminal-coding-system-internal", &args, 1)?;
    let _ = builtin_set_terminal_coding_system(mgr, args)?;
    Ok(Value::NIL)
}

/// `(text-quoting-style)` -- return the current effective text quoting style.
///
/// Mirrors GNU `Ftext_quoting_style` (`src/doc.c:652-678`):
///   - If `text-quoting-style' is `grave', `straight', or `curve', return it.
///   - If nil (the default), return `grave' when curved quotes cannot be
///     displayed, otherwise `curve'.
///   - Any other value is treated as `curve'.
///   - Never returns nil.
///
/// The display-capability fallback (GNU's `default_to_grave_quoting_style')
/// is currently a stub that always picks `curve' — neomacs does not yet
/// query the active display table for curved-quote support. This matches
/// GNU's behavior on a graphical/UTF-8 terminal.
pub(crate) fn builtin_text_quoting_style(
    eval: &super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("text-quoting-style", &args, 0)?;
    let var = eval
        .obarray
        .symbol_value("text-quoting-style")
        .copied()
        .unwrap_or(Value::NIL);
    if var.is_nil() {
        // GNU `default_to_grave_quoting_style' inspects the standard
        // display table to decide whether curved quotes are renderable.
        // Stub: always pick `curve'. Bringing in real display-capability
        // detection is a separate task.
        return Ok(Value::symbol("curve"));
    }
    if let Some(name) = var.as_symbol_name() {
        match name {
            "grave" | "straight" | "curve" => return Ok(Value::symbol(name)),
            _ => {}
        }
    }
    Ok(Value::symbol("curve"))
}

/// `(set-text-conversion-style STYLE &optional WHERE)` -- set conversion style.
/// NeoVM currently accepts all values and returns nil.
pub(crate) fn builtin_set_text_conversion_style(args: Vec<Value>) -> EvalResult {
    expect_min_args("set-text-conversion-style", &args, 1)?;
    expect_max_args("set-text-conversion-style", &args, 2)?;
    Ok(Value::NIL)
}

/// `(coding-system-priority-list &optional HIGHESTP)` -- return the current
/// priority list of coding systems for detection. If HIGHESTP is non-nil,
/// return only the highest-priority system.
pub(crate) fn builtin_coding_system_priority_list(
    mgr: &CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("coding-system-priority-list", &args, 1)?;
    let highest_only = args.first().is_some_and(|v| v.is_truthy());
    if highest_only {
        if let Some(first) = mgr.priority.first() {
            Ok(Value::list(vec![Value::symbol(*first)]))
        } else {
            Ok(Value::NIL)
        }
    } else {
        let items: Vec<Value> = mgr.priority.iter().map(|id| Value::symbol(*id)).collect();
        Ok(Value::list(items))
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Strip -unix, -dos, or -mac suffix from a coding system name.
fn strip_eol_suffix(name: &str) -> &str {
    for suffix in &["-unix", "-dos", "-mac"] {
        if name.ends_with(suffix) {
            return &name[..name.len() - suffix.len()];
        }
    }
    name
}

fn allows_derived_eol_variant(base: &str) -> bool {
    matches!(
        base,
        "utf-8"
            | "mule-utf-8"
            | "latin-1"
            | "iso-8859-1"
            | "iso-latin-1"
            | "latin-5"
            | "iso-8859-9"
            | "iso-latin-5"
            | "latin-0"
            | "latin-9"
            | "iso-8859-15"
            | "iso-latin-9"
            | "ascii"
            | "us-ascii"
            | "raw-text"
            | "undecided"
            | "utf-8-auto"
            | "prefer-utf-8"
            | "utf-8-emacs"
    )
}

fn normalize_coding_name_for_lookup(name: &str) -> &str {
    if name == "nil" { "no-conversion" } else { name }
}

fn display_base_name(base: &str) -> &str {
    match base {
        "latin-1" | "iso-8859-1" | "iso-latin-1" => "iso-latin-1",
        "latin-5" | "iso-8859-9" | "iso-latin-5" => "iso-latin-5",
        "latin-0" | "latin-9" | "iso-8859-15" | "iso-latin-9" => "iso-latin-9",
        "ascii" | "us-ascii" => "us-ascii",
        "binary" | "no-conversion" | "nil" => "no-conversion",
        "emacs-internal" | "utf-8-emacs" => "utf-8-emacs",
        "mule-utf-8" => "utf-8",
        other => other,
    }
}

fn coding_type_for_base(base: &str) -> Option<&'static str> {
    match base {
        "utf-8" | "mule-utf-8" | "utf-8-auto" | "emacs-internal" | "utf-8-emacs" => Some("utf-8"),
        "latin-1" | "iso-8859-1" | "iso-latin-1" | "latin-5" | "iso-8859-9" | "iso-latin-5"
        | "latin-0" | "latin-9" | "iso-8859-15" | "iso-latin-9" | "ascii" | "us-ascii" => {
            Some("charset")
        }
        "raw-text" | "binary" | "no-conversion" => Some("raw-text"),
        "undecided" | "prefer-utf-8" => Some("undecided"),
        _ => None,
    }
}

fn default_mnemonic_for_base(base: &str) -> Option<i64> {
    match base {
        "utf-8" | "mule-utf-8" | "utf-8-auto" | "emacs-internal" | "utf-8-emacs" => {
            Some('U' as i64)
        }
        "latin-1" | "iso-8859-1" | "iso-latin-1" => Some('1' as i64),
        "latin-5" | "iso-8859-9" | "iso-latin-5" => Some('9' as i64),
        "latin-0" | "latin-9" | "iso-8859-15" | "iso-latin-9" => Some('0' as i64),
        "ascii" | "us-ascii" | "undecided" | "prefer-utf-8" => Some('-' as i64),
        "raw-text" => Some('t' as i64),
        "binary" | "no-conversion" => Some('=' as i64),
        _ => None,
    }
}

fn properties_bucket_base(base: &str) -> &str {
    match base {
        "latin-1" | "iso-8859-1" | "iso-latin-1" => "iso-latin-1",
        "latin-5" | "iso-8859-9" | "iso-latin-5" => "iso-latin-5",
        "latin-0" | "latin-9" | "iso-8859-15" | "iso-latin-9" => "iso-latin-9",
        "ascii" | "us-ascii" => "us-ascii",
        "binary" | "no-conversion" | "nil" => "no-conversion",
        "emacs-internal" | "utf-8-emacs" => "utf-8-emacs",
        "mule-utf-8" => "utf-8",
        other => other,
    }
}

fn eol_vector_base(base: &str) -> &str {
    match base {
        "latin-1" | "iso-8859-1" | "iso-latin-1" => "iso-latin-1",
        "latin-5" | "iso-8859-9" | "iso-latin-5" => "iso-latin-5",
        "latin-0" | "latin-9" | "iso-8859-15" | "iso-latin-9" => "iso-latin-9",
        "ascii" | "us-ascii" => "us-ascii",
        "mule-utf-8" => "utf-8",
        "emacs-internal" | "utf-8-emacs" => "utf-8-emacs",
        other => other,
    }
}

fn derive_coding_for_eol(base: &str, eol: i64) -> Option<String> {
    let suffix = match eol {
        0 => "-unix",
        1 => "-dos",
        2 => "-mac",
        _ => return None,
    };
    let derived = match base {
        "latin-1" | "iso-8859-1" | "iso-latin-1" => format!("iso-latin-1{suffix}"),
        "latin-5" | "iso-8859-9" | "iso-latin-5" => format!("iso-latin-5{suffix}"),
        "latin-0" | "latin-9" | "iso-8859-15" | "iso-latin-9" => {
            format!("iso-latin-9{suffix}")
        }
        "ascii" | "us-ascii" => format!("us-ascii{suffix}"),
        "mule-utf-8" | "utf-8" => format!("utf-8{suffix}"),
        "utf-8-auto" => format!("utf-8-auto{suffix}"),
        "prefer-utf-8" => format!("prefer-utf-8{suffix}"),
        "undecided" => format!("undecided{suffix}"),
        "raw-text" => format!("raw-text{suffix}"),
        "utf-8-emacs" => format!("utf-8-emacs{suffix}"),
        "emacs-internal" => match eol {
            0 => "emacs-internal".to_string(),
            1 => "utf-8-emacs-dos".to_string(),
            2 => "utf-8-emacs-mac".to_string(),
            _ => unreachable!("validated above"),
        },
        "no-conversion" => {
            if eol == 0 {
                "no-conversion".to_string()
            } else {
                return None;
            }
        }
        "binary" => {
            if eol == 0 {
                "binary".to_string()
            } else {
                return None;
            }
        }
        other => format!("{other}{suffix}"),
    };
    Some(derived)
}

fn resolve_runtime_name(mgr: &CodingSystemManager, name: &str) -> Option<String> {
    let normalized = normalize_coding_name_for_lookup(name);
    if mgr.resolve(normalized).is_some() {
        return Some(normalized.to_string());
    }

    let eol = EolType::from_suffix(normalized)?;
    let base = strip_eol_suffix(normalized);
    if !allows_derived_eol_variant(base) {
        return None;
    }
    let canonical_base = mgr.resolve(base)?;
    derive_coding_for_eol(resolve_sym(canonical_base), eol.to_int()).map(|_| normalized.to_string())
}

fn canonical_runtime_name(mgr: &CodingSystemManager, name: &str) -> Option<String> {
    let normalized = normalize_coding_name_for_lookup(name);
    if let Some(eol) = EolType::from_suffix(normalized) {
        let base = strip_eol_suffix(normalized);
        let canonical_base = mgr.resolve(base)?;
        return derive_coding_for_eol(resolve_sym(canonical_base), eol.to_int());
    }

    mgr.resolve(normalized)
        .map(|id| resolve_sym(id).to_string())
}

fn runtime_bucket_name(mgr: &CodingSystemManager, resolved_name: &str) -> Option<String> {
    let base = strip_eol_suffix(resolved_name);
    let bucket_base = properties_bucket_base(base);
    let bucket_name = mgr
        .resolve(bucket_base)
        .map(|id| resolve_sym(id).to_string())
        .unwrap_or_else(|| bucket_base.to_string());
    if mgr.is_known(bucket_name.as_str()) {
        Some(bucket_name)
    } else {
        None
    }
}

fn alias_sort_rank(canonical: &str, alias: &str) -> usize {
    match canonical {
        "utf-8" => match alias {
            "mule-utf-8" => 0,
            "cp65001" => 1,
            _ => 2,
        },
        "iso-latin-1" => match alias {
            "iso-8859-1" => 0,
            "latin-1" => 1,
            _ => 2,
        },
        "iso-latin-5" => match alias {
            "iso-8859-9" => 0,
            "latin-5" => 1,
            _ => 2,
        },
        "iso-latin-9" => match alias {
            "iso-8859-15" => 0,
            "latin-9" => 1,
            "latin-0" => 2,
            _ => 3,
        },
        "us-ascii" => match alias {
            "iso-safe" => 0,
            "ascii" => 1,
            _ => 2,
        },
        _ => 0,
    }
}

fn raw_coding_candidates(mgr: &CodingSystemManager, exclude: Option<&[Value]>) -> Vec<String> {
    let excluded: HashSet<String> = exclude
        .unwrap_or(&[])
        .iter()
        .filter_map(|value| value.as_symbol_name().map(|name| name.to_string()))
        .collect();

    let mut names: Vec<String> = mgr
        .systems
        .values()
        .filter(|info| info.eol_type == EolType::Undecided)
        .map(|info| display_base_name(strip_eol_suffix(resolve_sym(info.name))).to_string())
        .filter(|name| {
            !matches!(
                name.as_str(),
                "raw-text" | "no-conversion" | "binary" | "undecided"
            )
        })
        .filter(|name| !excluded.contains(name))
        .collect();
    names.sort();
    names.dedup();
    names
}

fn coding_can_encode_char(coding: &str, ch: char) -> bool {
    match properties_bucket_base(coding) {
        "utf-8" | "utf-8-emacs" | "utf-8-auto" | "prefer-utf-8" => true,
        "iso-latin-1" | "iso-latin-5" | "iso-latin-9" => (ch as u32) <= 0xFF,
        "us-ascii" => ch.is_ascii(),
        _ => false,
    }
}

fn safe_coding_systems_for_text(
    mgr: &CodingSystemManager,
    text: &str,
    multibyte: bool,
    exclude: Option<&[Value]>,
) -> Value {
    if !multibyte || text.is_ascii() {
        return Value::T;
    }

    if !text.is_ascii() {
        let mut safe_codings = Vec::new();
        for coding in raw_coding_candidates(mgr, exclude) {
            if text
                .chars()
                .filter(|ch| !ch.is_ascii())
                .all(|ch| coding_can_encode_char(&coding, ch))
            {
                safe_codings.push(Value::symbol(coding));
            }
        }
        safe_codings.push(Value::symbol("raw-text"));
        safe_codings.push(Value::symbol("no-conversion"));
        return Value::list(safe_codings);
    }

    Value::T
}

fn marker_or_integer_position(value: &Value) -> Result<i64, Flow> {
    if value.is_marker() {
        return super::marker::marker_position_as_int(value);
    }
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *value],
        )),
    }
}

pub(crate) fn builtin_find_coding_systems_region_internal(
    eval: &mut Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("find-coding-systems-region-internal", &args, 2)?;
    expect_max_args("find-coding-systems-region-internal", &args, 3)?;

    if args[0].is_string() {
        let exclude = args.get(2).and_then(super::value::list_to_vec);
        let text = coding_runtime_string(&args[0])?;
        let multibyte = args[0].string_is_multibyte();
        return Ok(safe_coding_systems_for_text(
            &eval.coding_systems,
            &text,
            multibyte,
            exclude.as_deref(),
        ));
    }

    let start = marker_or_integer_position(&args[0])?;
    let end = marker_or_integer_position(&args[1])?;
    let exclude = args.get(2).and_then(super::value::list_to_vec);

    let buffer = eval
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    if !buffer.get_multibyte() {
        return Ok(Value::T);
    }

    let char_count = buffer.text.char_count() as i64;
    if !(1..=char_count + 1).contains(&start) || !(1..=char_count + 1).contains(&end) || start > end
    {
        return Err(signal("args-out-of-range", vec![args[0], args[1]]));
    }

    let start_byte = buffer.lisp_pos_to_full_buffer_byte(start);
    let end_byte = buffer.lisp_pos_to_full_buffer_byte(end);
    let text = {
        let string = buffer.buffer_substring_lisp_string(start_byte, end_byte);
        super::builtins::runtime_string_from_lisp_string(&string)
    };
    Ok(safe_coding_systems_for_text(
        &eval.coding_systems,
        &text,
        buffer.get_multibyte(),
        exclude.as_deref(),
    ))
}

// ===========================================================================
// Bootstrap variables
// ===========================================================================

/// Initialize coding-system-related variables that official Emacs sets
/// in C code (coding.c syms_of_coding).
pub fn register_bootstrap_vars(obarray: &mut crate::emacs_core::symbol::Obarray) {
    // latin-extra-code-table: 256-element nil vector (coding.c:12065).
    obarray.set_symbol_value(
        "latin-extra-code-table",
        Value::vector(vec![Value::NIL; 256]),
    );

    // coding.c:11927 — DEFVAR_LISP (Vcoding_system_list)
    obarray.set_symbol_value("coding-system-list", Value::NIL);
    // coding.c:11930 — DEFVAR_LISP (Vcoding_system_alist)
    obarray.set_symbol_value("coding-system-alist", Value::NIL);
    // coding.c:11935 — DEFVAR_LISP (Vcoding_category_list)
    obarray.set_symbol_value("coding-category-list", Value::NIL);
    // coding.c:11941 — DEFVAR_LISP (Vcoding_system_for_read)
    obarray.set_symbol_value("coding-system-for-read", Value::NIL);
    // coding.c:11949 — DEFVAR_LISP (Vcoding_system_for_write)
    obarray.set_symbol_value("coding-system-for-write", Value::NIL);
    // coding.c:11959 — DEFVAR_LISP (Vlast_code_conversion_error)
    obarray.set_symbol_value("last-code-conversion-error", Value::NIL);
    // coding.c:11999 — DEFVAR_LISP (Vlocale_coding_system)
    obarray.set_symbol_value("locale-coding-system", Value::NIL);
    // coding.c:12014 — DEFVAR_LISP (Veol_mnemonic_unix)
    obarray.set_symbol_value("eol-mnemonic-unix", Value::string(":"));
    // coding.c:12019 — DEFVAR_LISP (Veol_mnemonic_dos)
    obarray.set_symbol_value("eol-mnemonic-dos", Value::string("\\"));
    // coding.c:12024 — DEFVAR_LISP (Veol_mnemonic_mac)
    obarray.set_symbol_value("eol-mnemonic-mac", Value::string("/"));
    // coding.c:12029 — DEFVAR_LISP (Veol_mnemonic_undecided)
    obarray.set_symbol_value("eol-mnemonic-undecided", Value::string(":"));
    // coding.c:12036 — DEFVAR_LISP (Venable_character_translation)
    obarray.set_symbol_value("enable-character-translation", Value::T);
    // coding.c:12046 — DEFVAR_LISP (Vstandard_translation_table_for_decode)
    obarray.set_symbol_value("standard-translation-table-for-decode", Value::NIL);
    // coding.c:12050 — DEFVAR_LISP (Vstandard_translation_table_for_encode)
    obarray.set_symbol_value("standard-translation-table-for-encode", Value::NIL);
    // coding.c:12054 — DEFVAR_LISP (Vcharset_revision_table)
    obarray.set_symbol_value("charset-revision-table", Value::NIL);
    // coding.c:12072 — DEFVAR_LISP (Vselect_safe_coding_system_function)
    obarray.set_symbol_value("select-safe-coding-system-function", Value::NIL);
    // coding.c:12085 — DEFVAR_LISP (Vtranslation_table_for_input)
    obarray.set_symbol_value("translation-table-for-input", Value::NIL);
    // coding.c:11993 — DEFVAR_LISP (Vnetwork_coding_system_alist)
    obarray.set_symbol_value("network-coding-system-alist", Value::NIL);
    // coding.c:11996 — DEFVAR_LISP (Vprocess_coding_system_alist)
    obarray.set_symbol_value("process-coding-system-alist", Value::NIL);
}

/// `(set-buffer-file-coding-system CODING-SYSTEM &optional FORCE NOMODIFY)` --
/// set the buffer-local `buffer-file-coding-system` variable.
///
/// CODING-SYSTEM is validated via the coding-system registry.  FORCE and
/// NOMODIFY are accepted for arity compatibility but currently ignored
/// (GNU uses them to control modification flag and EOL override behaviour).
pub(crate) fn builtin_set_buffer_file_coding_system(
    eval: &mut Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("set-buffer-file-coding-system", &args, 1)?;
    expect_max_args("set-buffer-file-coding-system", &args, 3)?;

    // Validate coding-system argument.
    let cs_val = args[0];
    if !cs_val.is_nil() {
        match cs_val.as_symbol_name() {
            Some(name) if is_known_or_derived_coding_system(&eval.coding_systems, name) => {}
            Some(_) => {
                return Err(signal("coding-system-error", vec![cs_val]));
            }
            None => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("symbolp"), cs_val],
                ));
            }
        }
    }

    // Set buffer-local buffer-file-coding-system on the current buffer.
    let current_id = eval
        .buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    eval.buffers
        .set_buffer_local_property(current_id, "buffer-file-coding-system", cs_val);

    Ok(Value::NIL)
}

// ===========================================================================
// Tests
// ===========================================================================
#[cfg(test)]
#[path = "coding_test.rs"]
mod tests;
