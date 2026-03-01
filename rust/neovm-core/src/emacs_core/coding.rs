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

use super::error::{signal, EvalResult, Flow};
use super::intern::resolve_sym;
use super::value::*;
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// Argument helpers (local to this module)
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

fn expect_integer_or_marker(val: &Value) -> Result<(), Flow> {
    match val {
        Value::Int(_) => Ok(()),
        v if super::marker::is_marker(v) => Ok(()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *other],
        )),
    }
}

fn is_known_or_derived_coding_system(mgr: &CodingSystemManager, name: &str) -> bool {
    if mgr.is_known(name) {
        return true;
    }
    let base = strip_eol_suffix(name);
    if base == name || !allows_derived_eol_variant(base) {
        return false;
    }
    mgr.is_known(base)
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
        _ => format!("{name}-unix"),
    }
}

/// Extract a coding system name from a symbol or string argument.
#[cfg(test)]
fn coding_system_name(val: &Value) -> Result<String, Flow> {
    match val {
        Value::Symbol(id) => Ok(resolve_sym(*id).to_owned()),
        Value::Str(id) => Ok(with_heap(|h| h.get_string(*id).clone())),
        Value::Nil => Ok("nil".to_string()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *other],
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
    pub name: String,
    /// Type category (e.g. "utf-8", "charset", "raw-text", "undecided").
    pub coding_type: String,
    /// Mnemonic character shown in the mode line.
    pub mnemonic: char,
    /// End-of-line conversion type.
    pub eol_type: EolType,
    /// Whether this coding system is ASCII compatible.
    pub ascii_compatible_p: bool,
    /// Charset list (names of supported charsets).
    pub charset_list: Vec<String>,
    /// Post-read conversion function name.
    pub post_read_conversion: Option<String>,
    /// Pre-write conversion function name.
    pub pre_write_conversion: Option<String>,
    /// Default character for encoding.
    pub default_char: Option<char>,
    /// Whether this is for unibyte buffers.
    pub for_unibyte: bool,
    /// Arbitrary property list for coding-system-get / coding-system-put.
    pub properties: HashMap<String, Value>,
    /// Integer property slots used by coding-system-get / coding-system-put.
    pub int_properties: HashMap<i64, Value>,
}

impl CodingSystemInfo {
    fn new(name: &str, coding_type: &str, mnemonic: char, eol_type: EolType) -> Self {
        Self {
            name: name.to_string(),
            coding_type: coding_type.to_string(),
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
    fn base_name(&self) -> &str {
        for suffix in &["-unix", "-dos", "-mac"] {
            if self.name.ends_with(suffix) {
                return &self.name[..self.name.len() - suffix.len()];
            }
        }
        &self.name
    }
}

// ---------------------------------------------------------------------------
// CodingSystemManager
// ---------------------------------------------------------------------------

/// Central registry for all coding systems and their aliases.
pub struct CodingSystemManager {
    /// Registered coding systems, keyed by canonical name.
    pub systems: HashMap<String, CodingSystemInfo>,
    /// Alias -> canonical name mapping.
    pub aliases: HashMap<String, String>,
    /// Detection priority list (ordered list of system names).
    pub priority: Vec<String>,
    /// Current keyboard coding system.
    keyboard_coding: String,
    /// Current terminal coding system.
    terminal_coding: String,
}

impl CodingSystemManager {
    /// Create a new manager pre-populated with the standard coding systems.
    pub fn new() -> Self {
        let mut mgr = Self {
            systems: HashMap::new(),
            aliases: HashMap::new(),
            priority: Vec::new(),
            keyboard_coding: "utf-8-unix".to_string(),
            terminal_coding: "utf-8-unix".to_string(),
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
            "latin-1",
            "charset",
            'l',
            EolType::Undecided,
        ));
        mgr.register(CodingSystemInfo::new(
            "latin-1-unix",
            "charset",
            'l',
            EolType::Unix,
        ));
        mgr.register(CodingSystemInfo::new(
            "latin-1-dos",
            "charset",
            'l',
            EolType::Dos,
        ));
        mgr.register(CodingSystemInfo::new(
            "latin-1-mac",
            "charset",
            'l',
            EolType::Mac,
        ));
        mgr.register(CodingSystemInfo::new(
            "ascii",
            "charset",
            'A',
            EolType::Undecided,
        ));
        mgr.register(CodingSystemInfo::new(
            "ascii-unix",
            "charset",
            'A',
            EolType::Unix,
        ));
        mgr.register(CodingSystemInfo::new(
            "ascii-dos",
            "charset",
            'A',
            EolType::Dos,
        ));
        mgr.register(CodingSystemInfo::new(
            "ascii-mac",
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
            "prefer-utf-8",
            "undecided",
            '-',
            EolType::Undecided,
        ));

        // Common aliases
        mgr.aliases
            .insert("mule-utf-8".to_string(), "utf-8".to_string());
        mgr.aliases
            .insert("cp65001".to_string(), "utf-8".to_string());
        mgr.aliases
            .insert("iso-8859-1".to_string(), "latin-1".to_string());
        mgr.aliases
            .insert("iso-latin-1".to_string(), "latin-1".to_string());
        mgr.aliases
            .insert("us-ascii".to_string(), "ascii".to_string());
        mgr.aliases
            .insert("iso-safe".to_string(), "ascii".to_string());

        // Default priority list
        mgr.priority = vec![
            "utf-8".to_string(),
            "utf-8-unix".to_string(),
            "undecided".to_string(),
            "latin-1".to_string(),
            "ascii".to_string(),
            "raw-text".to_string(),
            "binary".to_string(),
            "no-conversion".to_string(),
        ];

        mgr
    }

    /// Register a coding system.
    fn register(&mut self, info: CodingSystemInfo) {
        self.systems.insert(info.name.clone(), info);
    }

    /// Resolve a name through the alias table to a canonical name.
    /// Returns either the input name (if it's a direct system) or the
    /// canonical name from the alias table.
    pub fn resolve<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        if self.systems.contains_key(name) {
            Some(name)
        } else if let Some(canonical) = self.aliases.get(name) {
            if self.systems.contains_key(canonical.as_str()) {
                Some(canonical.as_str())
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Look up a coding system by name (resolving aliases).
    pub fn get(&self, name: &str) -> Option<&CodingSystemInfo> {
        let canonical = self.resolve(name)?;
        self.systems.get(canonical)
    }

    /// Look up a coding system mutably by name (resolving aliases).
    pub fn get_mut(&mut self, name: &str) -> Option<&mut CodingSystemInfo> {
        // Need to resolve first, then borrow mutably.
        let canonical = if self.systems.contains_key(name) {
            name.to_string()
        } else if let Some(c) = self.aliases.get(name) {
            c.clone()
        } else {
            return None;
        };
        self.systems.get_mut(&canonical)
    }

    /// Check if a name is a known coding system (or alias).
    pub fn is_known(&self, name: &str) -> bool {
        self.resolve(name).is_some()
    }

    /// Add an alias mapping.
    pub fn add_alias(&mut self, alias: &str, target: &str) {
        self.aliases.insert(alias.to_string(), target.to_string());
    }

    /// Get all aliases that point to a given canonical name.
    pub fn aliases_for(&self, canonical: &str) -> Vec<String> {
        // Resolve in case the caller passed an alias
        let target = if self.systems.contains_key(canonical) {
            canonical
        } else if let Some(c) = self.aliases.get(canonical) {
            c.as_str()
        } else {
            return Vec::new();
        };

        self.aliases
            .iter()
            .filter(|(_, v)| v.as_str() == target)
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// List all registered coding system names (canonical only).
    pub fn list_all(&self) -> Vec<String> {
        let mut names: Vec<String> = self.systems.keys().cloned().collect();
        names.sort();
        names
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
        .filter(|n| {
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
    if matches!(args[0], Value::Str(_)) {
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

    let canonical = runtime_bucket_name(mgr, &resolved_name)
        .ok_or_else(|| signal("coding-system-error", vec![args[0]]))?;
    let display = display_base_name(strip_eol_suffix(&resolved_name)).to_string();
    let mut aliases = mgr.aliases_for(&canonical);
    aliases.sort_by(|a, b| {
        alias_sort_rank(&canonical, a)
            .cmp(&alias_sort_rank(&canonical, b))
            .then_with(|| a.cmp(b))
    });
    let mut result = vec![Value::symbol(display.clone())];
    for alias in aliases {
        if alias != display {
            result.push(Value::symbol(alias));
        }
    }
    if canonical != display {
        result.push(Value::symbol(canonical));
    }
    Ok(Value::list(result))
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

    if let Some(prop_name) = args[1].as_symbol_name() {
        if let Some(value) = info.properties.get(prop_name) {
            return Ok(*value);
        }
        if !prop_name.starts_with(':') {
            let colon_key = format!(":{prop_name}");
            if let Some(value) = info.properties.get(&colon_key) {
                return Ok(*value);
            }
        }
        return match prop_name {
            ":name" | "name" => Ok(Value::symbol(display_base_name(strip_eol_suffix(
                &resolved_name,
            )))),
            ":coding-type" | "coding-type" => Ok(Value::symbol(
                coding_type_for_base(strip_eol_suffix(&resolved_name))
                    .unwrap_or(info.coding_type.as_str()),
            )),
            ":type" | "type" => Ok(Value::Nil),
            ":mnemonic" | "mnemonic" => Ok(Value::Int(
                default_mnemonic_for_base(strip_eol_suffix(&resolved_name))
                    .unwrap_or(info.mnemonic as i64),
            )),
            ":eol-type" | "eol-type" => Ok(Value::Nil),
            _ => Ok(Value::Nil),
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

fn plist_push(plist: &mut Vec<Value>, key: &str, value: Value) {
    if key.starts_with(':') {
        plist.push(Value::keyword(key));
    } else {
        plist.push(Value::symbol(key));
    }
    plist.push(value);
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
        "latin-1" | "ascii" => "coding-category-charset",
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
        "latin-1" => Some("ISO 2022 based 8-bit encoding for Latin-1 (MIME:ISO-8859-1)."),
        "ascii" => Some("ASCII encoding."),
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
        "latin-1" => Some(vec![Value::symbol("iso-8859-1")]),
        "ascii" => Some(vec![Value::symbol("ascii")]),
        _ => None,
    }
}

fn coding_mime_charset_for_base(base: &str) -> Option<&'static str> {
    match base {
        "utf-8" | "utf-8-emacs" | "utf-8-auto" | "emacs-internal" => Some("utf-8"),
        "latin-1" => Some("iso-8859-1"),
        "ascii" => Some("us-ascii"),
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
    if matches!(args[0], Value::Str(_)) {
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
    let coding_type = coding_type_for_base(base).unwrap_or(info.coding_type.as_str());
    let mnemonic = default_mnemonic_for_base(base).unwrap_or(info.mnemonic as i64);

    let mut plist = Vec::new();
    plist_push(&mut plist, ":ascii-compatible-p", Value::True);
    plist_push(
        &mut plist,
        ":category",
        Value::symbol(coding_category_for_base(base)),
    );
    plist_push(&mut plist, ":name", Value::symbol(display_name));
    if let Some(doc) = coding_docstring_for_base(base) {
        plist_push(&mut plist, ":docstring", Value::string(doc));
    }
    plist_push(&mut plist, ":coding-type", Value::symbol(coding_type));
    plist_push(&mut plist, ":mnemonic", Value::Int(mnemonic));
    if let Some(charset_list) = coding_charset_list_for_base(base) {
        plist_push(&mut plist, ":charset-list", Value::list(charset_list));
    }
    if let Some(mime_charset) = coding_mime_charset_for_base(base) {
        plist_push(&mut plist, ":mime-charset", Value::symbol(mime_charset));
    }
    if matches!(base, "no-conversion" | "binary" | "raw-text") {
        plist_push(&mut plist, ":default-char", Value::Int(0));
        plist_push(&mut plist, ":for-unibyte", Value::True);
    }

    // Preserve caller-provided custom properties from coding-system-put.
    let mut custom_keys: Vec<String> = info.properties.keys().cloned().collect();
    custom_keys.sort();
    for key in custom_keys {
        if !plist_contains_key(&plist, &key) {
            if let Some(value) = info.properties.get(&key) {
                plist_push(&mut plist, &key, *value);
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
            vec![Value::symbol("coding-system-p"), Value::Nil],
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

    if let Some(prop_name) = args[1].as_symbol_name() {
        if matches!(prop_name, ":mnemonic" | "mnemonic") {
            let coerced = match &val {
                Value::Char(c) => Value::Int(*c as i64),
                Value::Int(n) if *n >= 0 => Value::Int(*n),
                Value::Str(id) => Value::Int(with_heap(|h| h.get_string(*id).chars().next().map(|ch| ch as i64).unwrap_or(0))),
                other => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("characterp"), *other],
                    ));
                }
            };
            info.properties
                .insert(prop_name.to_string(), coerced);
            return Ok(coerced);
        }
        info.properties.insert(prop_name.to_string(), val);
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
        return Ok(Value::Nil);
    };
    let resolved_name = match resolve_runtime_name(mgr, name) {
        Some(resolved) => resolved,
        None => return Ok(Value::Nil),
    };
    if let Some(eol) = EolType::from_suffix(&resolved_name) {
        return Ok(Value::Int(eol.to_int()));
    }
    let bucket = match runtime_bucket_name(mgr, &resolved_name) {
        Some(bucket) => bucket,
        None => return Ok(Value::Nil),
    };
    let Some(info) = mgr.get(&bucket) else {
        return Ok(Value::Nil);
    };

    match info.eol_type {
        EolType::Unix => Ok(Value::Int(0)),
        EolType::Dos => Ok(Value::Int(1)),
        EolType::Mac => Ok(Value::Int(2)),
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
    Ok(Value::symbol(info.coding_type.clone()))
}

/// `(coding-system-change-eol-conversion CODING-SYSTEM EOL-TYPE)` -- return
/// a coding system derived from CODING-SYSTEM but with a different EOL type.
/// EOL-TYPE is 0 (unix), 1 (dos), or 2 (mac), or a symbol.
pub(crate) fn builtin_coding_system_change_eol_conversion(
    mgr: &CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("coding-system-change-eol-conversion", &args, 2)?;
    if matches!(args[0], Value::Str(_)) {
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
    let resolved_name = normalize_coding_name_for_lookup(&raw_name).to_string();
    let resolved_base = strip_eol_suffix(&resolved_name);
    let no_conversion_family = is_nil_coding || matches!(resolved_base, "no-conversion" | "binary");

    if no_conversion_family {
        let out = match &args[1] {
            Value::Nil => Value::symbol("no-conversion"),
            Value::Int(n) => {
                if *n == 0 {
                    if is_nil_coding {
                        Value::Nil
                    } else if resolved_base == "binary" {
                        Value::symbol("binary")
                    } else {
                        Value::symbol("no-conversion")
                    }
                } else {
                    Value::Nil
                }
            }
            Value::Float(f, _) => {
                if *f == 0.0 {
                    if is_nil_coding {
                        Value::Nil
                    } else if resolved_base == "binary" {
                        Value::symbol("binary")
                    } else {
                        Value::symbol("no-conversion")
                    }
                } else {
                    Value::Nil
                }
            }
            Value::Symbol(id) if resolve_sym(*id) == "unix" => {
                if is_nil_coding {
                    Value::Nil
                } else if resolved_base == "binary" {
                    Value::symbol("binary")
                } else {
                    Value::symbol("no-conversion")
                }
            }
            Value::Symbol(id) if { let n = resolve_sym(*id); n == "dos" || n == "mac" } => Value::Nil,
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("number-or-marker-p"), *other],
                ));
            }
        };
        return Ok(out);
    }

    let target_eol = match &args[1] {
        Value::Nil => None,
        Value::Int(n) => Some(*n),
        Value::Symbol(id) if resolve_sym(*id) == "unix" => Some(0),
        Value::Symbol(id) if resolve_sym(*id) == "dos" => Some(1),
        Value::Symbol(id) if resolve_sym(*id) == "mac" => Some(2),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("fixnump"), *other],
            ));
        }
    };

    let raw_base = strip_eol_suffix(&raw_name);
    if target_eol.is_none() {
        if EolType::from_suffix(&raw_name).is_some() {
            return Ok(Value::symbol(display_base_name(raw_base)));
        }
        if raw_base == "emacs-internal" {
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
            vec![
                Value::vector(variants),
                Value::Int(eol),
            ],
        ));
    }

    if let Some(derived) = derive_coding_for_eol(resolved_base, eol) {
        Ok(Value::symbol(derived))
    } else {
        Ok(Value::Nil)
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
    let first_eol = match &args[0] {
        Value::Nil => Some(0),
        Value::Str(_) => None,
        other => match other.as_symbol_name() {
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
        return Ok(Value::Nil);
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
    Ok(Value::bool(known))
}

/// `(check-coding-system CODING-SYSTEM)` -- validate CODING-SYSTEM.
/// Returns CODING-SYSTEM when valid, nil for nil, and signals on invalid input.
pub(crate) fn builtin_check_coding_system(
    mgr: &CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("check-coding-system", &args, 1)?;
    match &args[0] {
        Value::Nil => Ok(Value::Nil),
        Value::Symbol(id) => {
            if is_known_or_derived_coding_system(mgr, resolve_sym(*id)) {
                Ok(args[0])
            } else {
                Err(signal("coding-system-error", vec![args[0]]))
            }
        }
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *other],
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
    Ok(Value::Nil)
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
        return Ok(Value::Nil);
    }
    match mgr.resolve(&name) {
        Some(canonical) => Ok(Value::symbol(canonical)),
        None => Ok(Value::Nil),
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
    let mnemonic = match &args[1] {
        Value::Char(c) => *c,
        Value::Int(n) if *n > 0 && *n <= 0x10FFFF => char::from_u32(*n as u32).unwrap_or('?'),
        _ => '?',
    };

    // arg[2]: coding-type (symbol)
    let coding_type = match &args[2] {
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        _ => "undecided".to_string(),
    };

    // arg[3]: charset-list (list of symbols, or special symbol like 'iso-2022)
    let charset_list = match &args[3] {
        Value::Symbol(id) => vec![resolve_sym(*id).to_owned()],
        _ => {
            if let Some(items) = super::value::list_to_vec(&args[3]) {
                items
                    .iter()
                    .filter_map(|v| v.as_symbol_name().map(|s| s.to_string()))
                    .collect()
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
    let post_read_conversion = match &args[7] {
        Value::Symbol(id) => {
            let s = resolve_sym(*id);
            if s == "nil" {
                None
            } else {
                Some(s.to_owned())
            }
        }
        _ => None,
    };

    // arg[8]: pre-write-conversion
    let pre_write_conversion = match &args[8] {
        Value::Symbol(id) => {
            let s = resolve_sym(*id);
            if s == "nil" {
                None
            } else {
                Some(s.to_owned())
            }
        }
        _ => None,
    };

    // arg[9]: default-char
    let default_char = match &args[9] {
        Value::Char(c) => Some(*c),
        Value::Int(n) if *n > 0 => char::from_u32(*n as u32),
        _ => None,
    };

    // arg[10]: for-unibyte
    let for_unibyte = args[10].is_truthy();

    // arg[11]: plist
    let mut properties = HashMap::new();
    if let Some(items) = super::value::list_to_vec(&args[11]) {
        let mut i = 0;
        while i + 1 < items.len() {
            if let Some(key) = items[i].as_symbol_name() {
                properties.insert(key.to_string(), items[i + 1]);
            }
            i += 2;
        }
    }

    // arg[12]: eol-type (symbol: unix/dos/mac, or vector for auto-detect)
    let eol_type = match &args[12] {
        Value::Symbol(id) => {
            let s = resolve_sym(*id);
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
    let mut info = CodingSystemInfo::new(&name, &coding_type, mnemonic, eol_type.clone());
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
                    CodingSystemInfo::new(&variant_name, &coding_type, mnemonic, et);
                mgr.register(variant);
            }
        }
    }

    Ok(Value::Nil)
}

/// Extract a coding system name from a symbol argument, signaling on non-symbol.
fn coding_symbol_name_required(val: &Value) -> Result<String, Flow> {
    match val {
        Value::Symbol(id) => Ok(resolve_sym(*id).to_owned()),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *other],
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

    let alias = match &args[0] {
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        Value::Nil => "nil".to_string(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), *other],
            ));
        }
    };

    let target = match &args[1] {
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        Value::Nil => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("coding-system-p"), Value::Nil],
            ));
        }
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), *other],
            ));
        }
    };

    let canonical = mgr
        .resolve(&target)
        .ok_or_else(|| signal("coding-system-error", vec![Value::symbol(&target)]))?
        .to_string();
    mgr.add_alias(&alias, &canonical);
    Ok(Value::Nil)
}

/// `(set-coding-system-priority &rest CODING-SYSTEMS)` -- move CODING-SYSTEMS
/// to the front of the detection priority list in order, keeping relative order
/// of the remaining systems.
pub(crate) fn builtin_set_coding_system_priority(
    mgr: &mut CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    if args.is_empty() {
        return Ok(Value::Nil);
    }

    let mut requested: Vec<(String, String)> = Vec::with_capacity(args.len());
    for arg in &args {
        if arg.is_nil() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("coding-system-p"), Value::Nil],
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
        let canonical = mgr.resolve(&resolved).unwrap_or(&resolved).to_string();
        requested.push((name, canonical));
    }

    let mut seen_canonicals: HashSet<String> =
        HashSet::with_capacity(mgr.priority.len() + requested.len());
    let mut reordered: Vec<String> = Vec::with_capacity(mgr.priority.len() + requested.len());

    for (display, canonical) in requested {
        if seen_canonicals.insert(canonical) {
            reordered.push(display);
        }
    }

    for name in &mgr.priority {
        let canonical = mgr.resolve(name).unwrap_or(name.as_str()).to_string();
        if seen_canonicals.insert(canonical) {
            reordered.push(name.clone());
        }
    }

    mgr.priority = reordered;
    Ok(Value::Nil)
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
    match &args[0] {
        Value::Str(_) => {}
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("stringp"), *other],
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
    Ok(Value::symbol(&mgr.keyboard_coding))
}

/// `(terminal-coding-system &optional TERMINAL)` -- return the current
/// terminal coding system. The TERMINAL argument is ignored.
pub(crate) fn builtin_terminal_coding_system(
    mgr: &CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("terminal-coding-system", &args, 1)?;
    Ok(Value::symbol(&mgr.terminal_coding))
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
        mgr.keyboard_coding = "no-conversion".to_string();
        return Ok(Value::Nil);
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
    let base = strip_eol_suffix(&name);
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
                "Unsupported coding system for keyboard: {name}"
            ))],
        ));
    }
    let normalized = normalize_keyboard_coding_system(&name);
    mgr.keyboard_coding = normalized.clone();
    Ok(Value::symbol(normalized))
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
        mgr.terminal_coding = "nil".to_string();
        return Ok(Value::Nil);
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
    mgr.terminal_coding = name;
    Ok(Value::Nil)
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
    Ok(Value::Nil)
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
    Ok(Value::Nil)
}

/// `(set-safe-terminal-coding-system-internal CODING-SYSTEM)` -- internal safe
/// terminal coding setter.
pub(crate) fn builtin_set_safe_terminal_coding_system_internal(
    mgr: &mut CodingSystemManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("set-safe-terminal-coding-system-internal", &args, 1)?;
    let _ = builtin_set_terminal_coding_system(mgr, args)?;
    Ok(Value::Nil)
}

/// `(text-quoting-style)` -- return current text quoting style.
pub(crate) fn builtin_text_quoting_style(args: Vec<Value>) -> EvalResult {
    expect_args("text-quoting-style", &args, 0)?;
    Ok(Value::symbol("curve"))
}

/// `(set-text-conversion-style STYLE &optional WHERE)` -- set conversion style.
/// NeoVM currently accepts all values and returns nil.
pub(crate) fn builtin_set_text_conversion_style(args: Vec<Value>) -> EvalResult {
    expect_min_args("set-text-conversion-style", &args, 1)?;
    expect_max_args("set-text-conversion-style", &args, 2)?;
    Ok(Value::Nil)
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
            Ok(Value::list(vec![Value::symbol(first)]))
        } else {
            Ok(Value::Nil)
        }
    } else {
        let items: Vec<Value> = mgr.priority.iter().map(Value::symbol).collect();
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
    if name == "nil" {
        "no-conversion"
    } else {
        name
    }
}

fn display_base_name(base: &str) -> &str {
    match base {
        "latin-1" | "iso-8859-1" | "iso-latin-1" => "iso-latin-1",
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
        "latin-1" | "iso-8859-1" | "iso-latin-1" | "ascii" | "us-ascii" => Some("charset"),
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
        "ascii" | "us-ascii" | "undecided" | "prefer-utf-8" => Some('-' as i64),
        "raw-text" => Some('t' as i64),
        "binary" | "no-conversion" => Some('=' as i64),
        _ => None,
    }
}

fn properties_bucket_base(base: &str) -> &str {
    match base {
        "latin-1" | "iso-8859-1" | "iso-latin-1" => "latin-1",
        "ascii" | "us-ascii" => "ascii",
        "binary" | "no-conversion" | "nil" => "no-conversion",
        "emacs-internal" | "utf-8-emacs" => "utf-8-emacs",
        "mule-utf-8" => "utf-8",
        other => other,
    }
}

fn eol_vector_base(base: &str) -> &str {
    match base {
        "latin-1" | "iso-8859-1" | "iso-latin-1" => "iso-latin-1",
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
    if is_known_or_derived_coding_system(mgr, normalized) {
        Some(normalized.to_string())
    } else {
        None
    }
}

fn runtime_bucket_name(mgr: &CodingSystemManager, resolved_name: &str) -> Option<String> {
    let base = strip_eol_suffix(resolved_name);
    let bucket_base = properties_bucket_base(base);
    let bucket_name = mgr.resolve(bucket_base).unwrap_or(bucket_base).to_string();
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
        "ascii" => match alias {
            "iso-safe" => 0,
            "us-ascii" => 1,
            _ => 2,
        },
        _ => 0,
    }
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
        Value::vector(vec![Value::Nil; 256]),
    );

    // coding.c:11927 — DEFVAR_LISP (Vcoding_system_list)
    obarray.set_symbol_value("coding-system-list", Value::Nil);
    // coding.c:11930 — DEFVAR_LISP (Vcoding_system_alist)
    obarray.set_symbol_value("coding-system-alist", Value::Nil);
    // coding.c:11935 — DEFVAR_LISP (Vcoding_category_list)
    obarray.set_symbol_value("coding-category-list", Value::Nil);
    // coding.c:11941 — DEFVAR_LISP (Vcoding_system_for_read)
    obarray.set_symbol_value("coding-system-for-read", Value::Nil);
    // coding.c:11949 — DEFVAR_LISP (Vcoding_system_for_write)
    obarray.set_symbol_value("coding-system-for-write", Value::Nil);
    // coding.c:11959 — DEFVAR_LISP (Vlast_code_conversion_error)
    obarray.set_symbol_value("last-code-conversion-error", Value::Nil);
    // coding.c:11999 — DEFVAR_LISP (Vlocale_coding_system)
    obarray.set_symbol_value("locale-coding-system", Value::Nil);
    // coding.c:12014 — DEFVAR_LISP (Veol_mnemonic_unix)
    obarray.set_symbol_value("eol-mnemonic-unix", Value::string(":"));
    // coding.c:12019 — DEFVAR_LISP (Veol_mnemonic_dos)
    obarray.set_symbol_value("eol-mnemonic-dos", Value::string("\\"));
    // coding.c:12024 — DEFVAR_LISP (Veol_mnemonic_mac)
    obarray.set_symbol_value("eol-mnemonic-mac", Value::string("/"));
    // coding.c:12029 — DEFVAR_LISP (Veol_mnemonic_undecided)
    obarray.set_symbol_value("eol-mnemonic-undecided", Value::string(":"));
    // coding.c:12036 — DEFVAR_LISP (Venable_character_translation)
    obarray.set_symbol_value("enable-character-translation", Value::True);
    // coding.c:12046 — DEFVAR_LISP (Vstandard_translation_table_for_decode)
    obarray.set_symbol_value("standard-translation-table-for-decode", Value::Nil);
    // coding.c:12050 — DEFVAR_LISP (Vstandard_translation_table_for_encode)
    obarray.set_symbol_value("standard-translation-table-for-encode", Value::Nil);
    // coding.c:12054 — DEFVAR_LISP (Vcharset_revision_table)
    obarray.set_symbol_value("charset-revision-table", Value::Nil);
    // coding.c:12072 — DEFVAR_LISP (Vselect_safe_coding_system_function)
    obarray.set_symbol_value("select-safe-coding-system-function", Value::Nil);
    // coding.c:12085 — DEFVAR_LISP (Vtranslation_table_for_input)
    obarray.set_symbol_value("translation-table-for-input", Value::Nil);
    // coding.c:11993 — DEFVAR_LISP (Vnetwork_coding_system_alist)
    obarray.set_symbol_value("network-coding-system-alist", Value::Nil);
    // coding.c:11996 — DEFVAR_LISP (Vprocess_coding_system_alist)
    obarray.set_symbol_value("process-coding-system-alist", Value::Nil);
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn mgr() -> CodingSystemManager {
        CodingSystemManager::new()
    }

    fn plist_get(value: &Value, key: &str) -> Option<Value> {
        let needle = key.trim_start_matches(':');
        let items = list_to_vec(value)?;
        let mut idx = 0;
        while idx + 1 < items.len() {
            if items[idx]
                .as_symbol_name()
                .is_some_and(|name| name.trim_start_matches(':') == needle)
            {
                return Some(items[idx + 1]);
            }
            idx += 2;
        }
        None
    }

    // ----- CodingSystemManager construction -----

    #[test]
    fn new_manager_has_standard_systems() {
        let m = mgr();
        assert!(m.is_known("utf-8"));
        assert!(m.is_known("utf-8-unix"));
        assert!(m.is_known("utf-8-dos"));
        assert!(m.is_known("utf-8-mac"));
        assert!(m.is_known("latin-1"));
        assert!(m.is_known("ascii"));
        assert!(m.is_known("binary"));
        assert!(m.is_known("raw-text"));
        assert!(m.is_known("undecided"));
        assert!(m.is_known("emacs-internal"));
        assert!(m.is_known("no-conversion"));
    }

    #[test]
    fn aliases_resolve() {
        let m = mgr();
        assert!(m.is_known("iso-8859-1")); // alias for latin-1
        assert!(m.is_known("us-ascii")); // alias for ascii
        assert!(m.is_known("mule-utf-8")); // alias for utf-8
        assert_eq!(m.resolve("iso-8859-1"), Some("latin-1"));
        assert_eq!(m.resolve("us-ascii"), Some("ascii"));
    }

    #[test]
    fn unknown_system_not_known() {
        let m = mgr();
        assert!(!m.is_known("martian-encoding"));
        assert_eq!(m.resolve("martian-encoding"), None);
    }

    #[test]
    fn add_alias_works() {
        let mut m = mgr();
        m.add_alias("my-utf8", "utf-8");
        assert!(m.is_known("my-utf8"));
        assert_eq!(m.resolve("my-utf8"), Some("utf-8"));
    }

    // ----- CodingSystemInfo -----

    #[test]
    fn base_name_strips_suffix() {
        let info = CodingSystemInfo::new("utf-8-unix", "utf-8", 'U', EolType::Unix);
        assert_eq!(info.base_name(), "utf-8");

        let info2 = CodingSystemInfo::new("utf-8", "utf-8", 'U', EolType::Undecided);
        assert_eq!(info2.base_name(), "utf-8");
    }

    // ----- coding-system-list -----

    #[test]
    fn coding_system_list_all() {
        let m = mgr();
        let result = builtin_coding_system_list(&m, vec![]).unwrap();
        let items = list_to_vec(&result).unwrap();
        assert!(items.len() >= 11); // at least the 11 pre-registered systems
    }

    #[test]
    fn coding_system_list_base_only() {
        let m = mgr();
        let result = builtin_coding_system_list(&m, vec![Value::True]).unwrap();
        let items = list_to_vec(&result).unwrap();
        // Should not contain utf-8-unix, utf-8-dos, utf-8-mac
        for item in &items {
            if let Value::Symbol(id) = item {
                let s = resolve_sym(*id);
                assert!(
                    !s.ends_with("-unix") && !s.ends_with("-dos") && !s.ends_with("-mac"),
                    "base-only list should not contain: {}",
                    s
                );
            }
        }
    }

    #[test]
    fn coding_system_list_rejects_too_many_args() {
        let m = mgr();
        let result = builtin_coding_system_list(&m, vec![Value::Nil, Value::Nil]);
        assert!(result.is_err());
    }

    // ----- coding-system-aliases -----

    #[test]
    fn coding_system_aliases_found() {
        let m = mgr();
        let result = builtin_coding_system_aliases(&m, vec![Value::symbol("utf-8")]).unwrap();
        let items = list_to_vec(&result).unwrap();
        // First element should be the canonical name
        assert!(matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "utf-8"));
        // Should include aliases like mule-utf-8
        assert!(items.len() > 1);
    }

    #[test]
    fn coding_system_aliases_unknown() {
        let m = mgr();
        let result = builtin_coding_system_aliases(&m, vec![Value::symbol("nonexistent")]);
        assert!(result.is_err());
    }

    #[test]
    fn coding_system_aliases_nil_maps_to_no_conversion_family() {
        let m = mgr();
        let result = builtin_coding_system_aliases(&m, vec![Value::Nil]).unwrap();
        assert_eq!(
            result,
            Value::list(vec![
                Value::symbol("no-conversion"),
                Value::symbol("binary")
            ])
        );
    }

    #[test]
    fn coding_system_aliases_string_is_type_error() {
        let m = mgr();
        let result = builtin_coding_system_aliases(&m, vec![Value::string("utf-8")]);
        assert!(result.is_err());
    }

    // ----- coding-system-get -----

    #[test]
    fn coding_system_get_name() {
        let m = mgr();
        let result =
            builtin_coding_system_get(&m, vec![Value::symbol("utf-8"), Value::symbol(":name")])
                .unwrap();
        assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "utf-8"));
    }

    #[test]
    fn coding_system_get_type() {
        let m = mgr();
        let result = builtin_coding_system_get(
            &m,
            vec![Value::symbol("latin-1"), Value::symbol(":coding-type")],
        )
        .unwrap();
        assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "charset"));
    }

    #[test]
    fn coding_system_get_mnemonic() {
        let m = mgr();
        let result =
            builtin_coding_system_get(&m, vec![Value::symbol("utf-8"), Value::symbol(":mnemonic")])
                .unwrap();
        assert!(eq_value(&result, &Value::Int('U' as i64)));
    }

    #[test]
    fn coding_system_get_eol_type() {
        let m = mgr();
        let result = builtin_coding_system_get(
            &m,
            vec![Value::symbol("utf-8-unix"), Value::symbol(":eol-type")],
        )
        .unwrap();
        assert!(result.is_nil());
    }

    #[test]
    fn coding_system_get_unknown_prop() {
        let m = mgr();
        let result = builtin_coding_system_get(
            &m,
            vec![Value::symbol("utf-8"), Value::symbol(":nonexistent")],
        )
        .unwrap();
        assert!(result.is_nil());
    }

    #[test]
    fn coding_system_get_unknown_system() {
        let m = mgr();
        let result =
            builtin_coding_system_get(&m, vec![Value::symbol("bogus"), Value::symbol(":name")]);
        assert!(result.is_err());
    }

    // ----- coding-system-plist -----

    #[test]
    fn coding_system_plist_utf8_core_fields() {
        let m = mgr();
        let plist = builtin_coding_system_plist(&m, vec![Value::symbol("utf-8")]).unwrap();
        assert_eq!(plist_get(&plist, ":name"), Some(Value::symbol("utf-8")));
        assert_eq!(
            plist_get(&plist, ":coding-type"),
            Some(Value::symbol("utf-8"))
        );
        assert_eq!(plist_get(&plist, ":mnemonic"), Some(Value::Int('U' as i64)));
    }

    #[test]
    fn coding_system_plist_keyword_keys_work_with_builtin_plist_get() {
        let m = mgr();
        let plist = builtin_coding_system_plist(&m, vec![Value::symbol("utf-8")]).unwrap();

        let name =
            crate::emacs_core::builtins::builtin_plist_get(vec![plist, Value::keyword(":name")])
                .unwrap();
        assert_eq!(name, Value::symbol("utf-8"));

        let mnemonic =
            crate::emacs_core::builtins::builtin_plist_get(vec![plist, Value::keyword(":mnemonic")])
                .unwrap();
        assert_eq!(mnemonic, Value::Int('U' as i64));
    }

    #[test]
    fn coding_system_plist_normalizes_alias_and_eol_variant_name() {
        let m = mgr();
        let latin = builtin_coding_system_plist(&m, vec![Value::symbol("latin-1")]).unwrap();
        assert_eq!(
            plist_get(&latin, ":name"),
            Some(Value::symbol("iso-latin-1"))
        );

        let utf8_unix = builtin_coding_system_plist(&m, vec![Value::symbol("utf-8-unix")]).unwrap();
        assert_eq!(plist_get(&utf8_unix, ":name"), Some(Value::symbol("utf-8")));
    }

    #[test]
    fn coding_system_plist_nil_maps_to_no_conversion() {
        let m = mgr();
        let plist = builtin_coding_system_plist(&m, vec![Value::Nil]).unwrap();
        assert_eq!(
            plist_get(&plist, ":name"),
            Some(Value::symbol("no-conversion"))
        );
        assert_eq!(
            plist_get(&plist, ":coding-type"),
            Some(Value::symbol("raw-text"))
        );
    }

    #[test]
    fn coding_system_plist_type_and_unknown_errors() {
        let m = mgr();
        let type_err = builtin_coding_system_plist(&m, vec![Value::string("utf-8")]);
        assert!(type_err.is_err());

        let unknown = builtin_coding_system_plist(&m, vec![Value::symbol("bogus")]);
        assert!(unknown.is_err());
    }

    #[test]
    fn coding_system_plist_includes_custom_properties_from_put() {
        let mut m = mgr();
        builtin_coding_system_put(
            &mut m,
            vec![
                Value::symbol("utf-8"),
                Value::symbol(":foo"),
                Value::Int(42),
            ],
        )
        .unwrap();

        let plist = builtin_coding_system_plist(&m, vec![Value::symbol("utf-8")]).unwrap();
        assert_eq!(plist_get(&plist, ":foo"), Some(Value::Int(42)));
    }

    // ----- coding-system-put -----

    #[test]
    fn coding_system_put_custom_prop() {
        let mut m = mgr();
        let result = builtin_coding_system_put(
            &mut m,
            vec![
                Value::symbol("utf-8"),
                Value::symbol(":charset-list"),
                Value::list(vec![Value::symbol("unicode")]),
            ],
        )
        .unwrap();
        assert_eq!(result, Value::list(vec![Value::symbol("unicode")]));

        // Verify it was stored
        let get_result = builtin_coding_system_get(
            &m,
            vec![Value::symbol("utf-8"), Value::symbol(":charset-list")],
        )
        .unwrap();
        assert!(!get_result.is_nil());
    }

    #[test]
    fn coding_system_put_mnemonic() {
        let mut m = mgr();
        builtin_coding_system_put(
            &mut m,
            vec![
                Value::symbol("utf-8"),
                Value::symbol(":mnemonic"),
                Value::Char('X'),
            ],
        )
        .unwrap();

        let result =
            builtin_coding_system_get(&m, vec![Value::symbol("utf-8"), Value::symbol(":mnemonic")])
                .unwrap();
        assert!(eq_value(&result, &Value::Int('X' as i64)));
    }

    #[test]
    fn coding_system_put_unknown_system_errors() {
        let mut m = mgr();
        let result = builtin_coding_system_put(
            &mut m,
            vec![Value::symbol("bogus"), Value::symbol(":foo"), Value::Int(1)],
        );
        assert!(result.is_err());
    }

    // ----- coding-system-base -----

    #[test]
    fn coding_system_base_with_suffix() {
        let m = mgr();
        let result = builtin_coding_system_base(&m, vec![Value::symbol("utf-8-unix")]).unwrap();
        assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "utf-8"));
    }

    #[test]
    fn coding_system_base_without_suffix() {
        let m = mgr();
        let result = builtin_coding_system_base(&m, vec![Value::symbol("utf-8")]).unwrap();
        assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "utf-8"));
    }

    #[test]
    fn coding_system_base_unknown_still_strips() {
        let m = mgr();
        let result = builtin_coding_system_base(&m, vec![Value::symbol("foo-bar-unix")]);
        assert!(result.is_err());
    }

    // ----- coding-system-eol-type -----

    #[test]
    fn eol_type_unix() {
        let m = mgr();
        let result = builtin_coding_system_eol_type(&m, vec![Value::symbol("utf-8-unix")]).unwrap();
        assert!(eq_value(&result, &Value::Int(0)));
    }

    #[test]
    fn eol_type_dos() {
        let m = mgr();
        let result = builtin_coding_system_eol_type(&m, vec![Value::symbol("utf-8-dos")]).unwrap();
        assert!(eq_value(&result, &Value::Int(1)));
    }

    #[test]
    fn eol_type_mac() {
        let m = mgr();
        let result = builtin_coding_system_eol_type(&m, vec![Value::symbol("utf-8-mac")]).unwrap();
        assert!(eq_value(&result, &Value::Int(2)));
    }

    #[test]
    fn eol_type_undecided_returns_vector() {
        let m = mgr();
        let result = builtin_coding_system_eol_type(&m, vec![Value::symbol("utf-8")]).unwrap();
        // Should be a vector of [utf-8-unix utf-8-dos utf-8-mac]
        if let Value::Vector(v) = result {
            let locked = with_heap(|h| h.get_vector(v).clone());
            assert_eq!(locked.len(), 3);
            assert!(matches!(&locked[0], Value::Symbol(id) if resolve_sym(*id) == "utf-8-unix"));
            assert!(matches!(&locked[1], Value::Symbol(id) if resolve_sym(*id) == "utf-8-dos"));
            assert!(matches!(&locked[2], Value::Symbol(id) if resolve_sym(*id) == "utf-8-mac"));
        } else {
            panic!("expected vector for undecided eol-type");
        }
    }

    #[test]
    fn eol_type_latin_alias_uses_iso_latin_display_variants() {
        let m = mgr();
        let result = builtin_coding_system_eol_type(&m, vec![Value::symbol("latin-1")]).unwrap();
        if let Value::Vector(v) = result {
            let locked = with_heap(|h| h.get_vector(v).clone());
            assert_eq!(locked.len(), 3);
            assert_eq!(locked[0], Value::symbol("iso-latin-1-unix"));
            assert_eq!(locked[1], Value::symbol("iso-latin-1-dos"));
            assert_eq!(locked[2], Value::symbol("iso-latin-1-mac"));
        } else {
            panic!("expected vector for undecided latin-1 eol-type");
        }
    }

    #[test]
    fn eol_type_nil_maps_to_no_conversion() {
        let m = mgr();
        let result = builtin_coding_system_eol_type(&m, vec![Value::Nil]).unwrap();
        assert_eq!(result, Value::Int(0));
    }

    #[test]
    fn eol_type_non_symbol_designator_returns_nil() {
        let m = mgr();
        assert!(
            builtin_coding_system_eol_type(&m, vec![Value::string("utf-8")])
                .unwrap()
                .is_nil()
        );
        assert!(builtin_coding_system_eol_type(&m, vec![Value::Int(1)])
            .unwrap()
            .is_nil());
    }

    #[test]
    fn eol_type_unknown_returns_nil() {
        let m = mgr();
        let result =
            builtin_coding_system_eol_type(&m, vec![Value::symbol("nonexistent")]).unwrap();
        assert!(result.is_nil());
    }

    // ----- coding-system-type -----

    #[test]
    fn coding_system_type_utf8() {
        let m = mgr();
        let result = builtin_coding_system_type(&m, vec![Value::symbol("utf-8")]).unwrap();
        assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "utf-8"));
    }

    #[test]
    fn coding_system_type_raw_text() {
        let m = mgr();
        let result = builtin_coding_system_type(&m, vec![Value::symbol("raw-text")]).unwrap();
        assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "raw-text"));
    }

    #[test]
    fn coding_system_type_unknown() {
        let m = mgr();
        let result = builtin_coding_system_type(&m, vec![Value::symbol("bogus")]);
        assert!(result.is_err());
    }

    // ----- coding-system-change-eol-conversion -----

    #[test]
    fn change_eol_by_int() {
        let m = mgr();
        let result = builtin_coding_system_change_eol_conversion(
            &m,
            vec![Value::symbol("utf-8"), Value::Int(1)],
        )
        .unwrap();
        assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "utf-8-dos"));
    }

    #[test]
    fn change_eol_by_symbol() {
        let m = mgr();
        let result = builtin_coding_system_change_eol_conversion(
            &m,
            vec![Value::symbol("utf-8-unix"), Value::symbol("mac")],
        )
        .unwrap();
        assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "utf-8-mac"));
    }

    #[test]
    fn change_eol_strips_existing_suffix() {
        let m = mgr();
        let result = builtin_coding_system_change_eol_conversion(
            &m,
            vec![Value::symbol("utf-8-dos"), Value::Int(0)],
        )
        .unwrap();
        assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "utf-8-unix"));
    }

    // ----- coding-system-change-text-conversion -----

    #[test]
    fn change_text_conversion_preserves_eol() {
        let m = mgr();
        let result = builtin_coding_system_change_text_conversion(
            &m,
            vec![Value::symbol("utf-8-unix"), Value::symbol("latin-1")],
        )
        .unwrap();
        assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "iso-latin-1-unix"));
    }

    #[test]
    fn change_text_conversion_undecided_eol() {
        let m = mgr();
        let result = builtin_coding_system_change_text_conversion(
            &m,
            vec![Value::symbol("utf-8"), Value::symbol("latin-1")],
        )
        .unwrap();
        // utf-8 has undecided eol -> no suffix
        assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "latin-1"));
    }

    // ----- detect-coding-string -----

    #[test]
    fn detect_coding_string_highest() {
        let m = mgr();
        let result =
            builtin_detect_coding_string(&m, vec![Value::string("hello"), Value::True]).unwrap();
        assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "undecided"));
    }

    #[test]
    fn detect_coding_string_list() {
        let m = mgr();
        let result = builtin_detect_coding_string(&m, vec![Value::string("hello")]).unwrap();
        let items = list_to_vec(&result).unwrap();
        assert_eq!(items.len(), 1);
        assert!(matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "undecided"));
    }

    #[test]
    fn detect_coding_string_wrong_type() {
        let m = mgr();
        let result = builtin_detect_coding_string(&m, vec![Value::Int(42)]);
        assert!(result.is_err());
    }

    #[test]
    fn detect_coding_string_rejects_too_many_args() {
        let m = mgr();
        let result =
            builtin_detect_coding_string(&m, vec![Value::string("x"), Value::Nil, Value::Nil]);
        assert!(result.is_err());
    }

    // ----- detect-coding-region -----

    #[test]
    fn detect_coding_region_highest() {
        let m = mgr();
        let result =
            builtin_detect_coding_region(&m, vec![Value::Int(1), Value::Int(100), Value::True])
                .unwrap();
        assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "undecided"));
    }

    #[test]
    fn detect_coding_region_list() {
        let m = mgr();
        let result =
            builtin_detect_coding_region(&m, vec![Value::Int(1), Value::Int(100)]).unwrap();
        let items = list_to_vec(&result).unwrap();
        assert_eq!(items.len(), 1);
        assert!(matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "undecided"));
    }

    #[test]
    fn detect_coding_region_rejects_too_many_args() {
        let m = mgr();
        let result = builtin_detect_coding_region(
            &m,
            vec![Value::Int(1), Value::Int(100), Value::Nil, Value::Nil],
        );
        assert!(result.is_err());
    }

    #[test]
    fn detect_coding_region_rejects_non_integer_or_marker_bounds() {
        let m = mgr();
        assert!(builtin_detect_coding_region(&m, vec![Value::string("a"), Value::Int(1)]).is_err());
        assert!(builtin_detect_coding_region(&m, vec![Value::Int(1), Value::string("b")]).is_err());
        assert!(builtin_detect_coding_region(&m, vec![Value::Nil, Value::Int(1)]).is_err());
        assert!(builtin_detect_coding_region(&m, vec![Value::Int(1), Value::Nil]).is_err());
    }

    // ----- keyboard/terminal coding system -----

    #[test]
    fn keyboard_coding_system_default() {
        let m = mgr();
        let result = builtin_keyboard_coding_system(&m, vec![]).unwrap();
        assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "utf-8-unix"));
    }

    #[test]
    fn terminal_coding_system_default() {
        let m = mgr();
        let result = builtin_terminal_coding_system(&m, vec![]).unwrap();
        assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "utf-8-unix"));
    }

    #[test]
    fn coding_system_getters_validate_max_arity() {
        let m = mgr();
        assert!(builtin_keyboard_coding_system(&m, vec![Value::Nil]).is_ok());
        assert!(builtin_terminal_coding_system(&m, vec![Value::Nil]).is_ok());
        assert!(builtin_keyboard_coding_system(&m, vec![Value::Nil, Value::Nil]).is_err());
        assert!(builtin_terminal_coding_system(&m, vec![Value::Nil, Value::Nil]).is_err());
    }

    #[test]
    fn set_keyboard_coding_system() {
        let mut m = mgr();
        let set =
            builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("latin-1")]).unwrap();
        assert!(matches!(set, Value::Symbol(id) if resolve_sym(id) == "iso-latin-1-unix"));
        let get = builtin_keyboard_coding_system(&m, vec![]).unwrap();
        assert!(matches!(get, Value::Symbol(id) if resolve_sym(id) == "iso-latin-1-unix"));
    }

    #[test]
    fn set_keyboard_coding_system_canonicalizes_non_unix_alias_suffixes() {
        let mut m = mgr();

        let latin_dos =
            builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("latin-1-dos")]).unwrap();
        assert_eq!(latin_dos, Value::symbol("iso-latin-1-unix"));

        let latin_mac =
            builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("latin-1-mac")]).unwrap();
        assert_eq!(latin_mac, Value::symbol("iso-latin-1-unix"));

        let iso_dos =
            builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("iso-8859-1-dos")])
                .unwrap();
        assert_eq!(iso_dos, Value::symbol("iso-latin-1-unix"));

        let ascii_dos =
            builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("ascii-dos")]).unwrap();
        assert_eq!(ascii_dos, Value::symbol("us-ascii-unix"));

        let ascii_mac =
            builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("ascii-mac")]).unwrap();
        assert_eq!(ascii_mac, Value::symbol("us-ascii-unix"));
    }

    #[test]
    fn set_keyboard_coding_system_preserves_explicit_unix_spelling() {
        let mut m = mgr();

        let latin_unix =
            builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("latin-1-unix")])
                .unwrap();
        assert_eq!(latin_unix, Value::symbol("latin-1-unix"));

        let iso_unix =
            builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("iso-8859-1-unix")])
                .unwrap();
        assert_eq!(iso_unix, Value::symbol("iso-8859-1-unix"));

        let ascii_unix =
            builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("ascii-unix")]).unwrap();
        assert_eq!(ascii_unix, Value::symbol("ascii-unix"));
    }

    #[test]
    fn set_terminal_coding_system() {
        let mut m = mgr();
        let set = builtin_set_terminal_coding_system(&mut m, vec![Value::symbol("ascii")]).unwrap();
        assert!(set.is_nil());
        let get = builtin_terminal_coding_system(&m, vec![]).unwrap();
        assert!(matches!(get, Value::Symbol(id) if resolve_sym(id) == "ascii"));
    }

    #[test]
    fn set_keyboard_coding_nil_resets_to_no_conversion() {
        let mut m = mgr();
        builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("latin-1")]).unwrap();
        builtin_set_keyboard_coding_system(&mut m, vec![Value::Nil]).unwrap();
        let result = builtin_keyboard_coding_system(&m, vec![]).unwrap();
        assert!(matches!(result, Value::Symbol(id) if resolve_sym(id) == "no-conversion"));
    }

    #[test]
    fn set_terminal_coding_nil_sets_nil_symbol() {
        let mut m = mgr();
        builtin_set_terminal_coding_system(&mut m, vec![Value::symbol("utf-8")]).unwrap();
        builtin_set_terminal_coding_system(&mut m, vec![Value::Nil]).unwrap();
        let result = builtin_terminal_coding_system(&m, vec![]).unwrap();
        assert!(result.is_nil());
    }

    #[test]
    fn coding_system_setters_validate_symbol_and_known_names() {
        let mut m = mgr();
        assert!(builtin_set_keyboard_coding_system(&mut m, vec![Value::string("utf-8")]).is_err());
        assert!(builtin_set_terminal_coding_system(&mut m, vec![Value::string("utf-8")]).is_err());
        assert!(
            builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("no-such-coding")])
                .is_err()
        );
        assert!(
            builtin_set_terminal_coding_system(&mut m, vec![Value::symbol("no-such-coding")])
                .is_err()
        );
    }

    #[test]
    fn coding_system_setters_treat_keywords_as_symbol_designators() {
        let mut m = mgr();
        let keyword = Value::keyword(":utf-8");
        let kb = builtin_set_keyboard_coding_system(&mut m, vec![keyword]);
        let term = builtin_set_terminal_coding_system(&mut m, vec![keyword]);

        match kb {
            Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "coding-system-error"),
            other => panic!("expected coding-system-error for keyword keyboard set, got {other:?}"),
        }
        match term {
            Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "coding-system-error"),
            other => panic!("expected coding-system-error for keyword terminal set, got {other:?}"),
        }
    }

    #[test]
    fn coding_system_setters_validate_arity_edges() {
        let mut m = mgr();
        assert!(builtin_set_keyboard_coding_system(&mut m, vec![Value::Nil, Value::Nil]).is_ok());
        assert!(builtin_set_keyboard_coding_system(
            &mut m,
            vec![Value::Nil, Value::Nil, Value::Nil]
        )
        .is_err());

        assert!(builtin_set_terminal_coding_system(&mut m, vec![Value::Nil, Value::Nil]).is_ok());
        assert!(builtin_set_terminal_coding_system(
            &mut m,
            vec![Value::Nil, Value::Nil, Value::Nil]
        )
        .is_ok());
        assert!(builtin_set_terminal_coding_system(
            &mut m,
            vec![Value::Nil, Value::Nil, Value::Nil, Value::Nil]
        )
        .is_err());
    }

    // ----- coding-system-priority-list -----

    #[test]
    fn priority_list_full() {
        let m = mgr();
        let result = builtin_coding_system_priority_list(&m, vec![]).unwrap();
        let items = list_to_vec(&result).unwrap();
        assert!(!items.is_empty());
        // First should be utf-8
        assert!(matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "utf-8"));
    }

    #[test]
    fn priority_list_highest() {
        let m = mgr();
        let result = builtin_coding_system_priority_list(&m, vec![Value::True]).unwrap();
        let items = list_to_vec(&result).unwrap();
        assert_eq!(items.len(), 1);
        assert!(matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "utf-8"));
    }

    #[test]
    fn priority_list_rejects_too_many_args() {
        let m = mgr();
        let result = builtin_coding_system_priority_list(&m, vec![Value::Nil, Value::Nil]);
        assert!(result.is_err());
    }

    // ----- EolType -----

    #[test]
    fn eol_type_to_int() {
        assert_eq!(EolType::Unix.to_int(), 0);
        assert_eq!(EolType::Dos.to_int(), 1);
        assert_eq!(EolType::Mac.to_int(), 2);
        assert_eq!(EolType::Undecided.to_int(), 0);
    }

    #[test]
    fn eol_type_from_suffix() {
        assert_eq!(EolType::from_suffix("utf-8-unix"), Some(EolType::Unix));
        assert_eq!(EolType::from_suffix("utf-8-dos"), Some(EolType::Dos));
        assert_eq!(EolType::from_suffix("utf-8-mac"), Some(EolType::Mac));
        assert_eq!(EolType::from_suffix("utf-8"), None);
    }

    // ----- strip_eol_suffix -----

    #[test]
    fn strip_eol_suffix_works() {
        assert_eq!(strip_eol_suffix("utf-8-unix"), "utf-8");
        assert_eq!(strip_eol_suffix("utf-8-dos"), "utf-8");
        assert_eq!(strip_eol_suffix("utf-8-mac"), "utf-8");
        assert_eq!(strip_eol_suffix("utf-8"), "utf-8");
        assert_eq!(strip_eol_suffix("latin-1"), "latin-1");
    }

    // ----- argument validation -----

    #[test]
    fn coding_system_get_wrong_arg_count() {
        let m = mgr();
        let result = builtin_coding_system_get(&m, vec![Value::symbol("utf-8")]);
        assert!(result.is_err());
    }

    #[test]
    fn coding_system_base_wrong_arg_count() {
        let m = mgr();
        let result = builtin_coding_system_base(&m, vec![]);
        assert!(result.is_err());
    }

    #[test]
    fn coding_system_aliases_wrong_arg_count() {
        let m = mgr();
        let result = builtin_coding_system_aliases(&m, vec![]);
        assert!(result.is_err());
    }

    #[test]
    fn coding_system_p_reads_runtime_aliases() {
        let mut m = mgr();
        let before = builtin_coding_system_p(&m, vec![Value::symbol("vm-utf8")]).unwrap();
        assert!(before.is_nil());

        builtin_define_coding_system_alias(
            &mut m,
            vec![Value::symbol("vm-utf8"), Value::symbol("utf-8")],
        )
        .unwrap();
        let after = builtin_coding_system_p(&m, vec![Value::symbol("vm-utf8")]).unwrap();
        assert!(after.is_truthy());
    }

    #[test]
    fn coding_system_p_accepts_nil_and_supported_derived_variants() {
        let m = mgr();
        assert!(builtin_coding_system_p(&m, vec![Value::Nil])
            .unwrap()
            .is_truthy());
        assert!(
            builtin_coding_system_p(&m, vec![Value::symbol("ascii-dos")])
                .unwrap()
                .is_truthy()
        );
    }

    #[test]
    fn check_coding_system_signals_unknown_symbols() {
        let m = mgr();
        let result = builtin_check_coding_system(&m, vec![Value::symbol("vm-no-such")]);
        match result {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "coding-system-error");
                assert_eq!(sig.data, vec![Value::symbol("vm-no-such")]);
            }
            other => panic!("expected coding-system-error signal, got {other:?}"),
        }
    }

    #[test]
    fn check_coding_system_accepts_supported_derived_variants() {
        let m = mgr();
        assert_eq!(
            builtin_check_coding_system(&m, vec![Value::symbol("latin-1-unix")]).unwrap(),
            Value::symbol("latin-1-unix")
        );
        assert_eq!(
            builtin_check_coding_system(&m, vec![Value::symbol("ascii-unix")]).unwrap(),
            Value::symbol("ascii-unix")
        );
        assert_eq!(
            builtin_check_coding_system(&m, vec![Value::symbol("undecided-unix")]).unwrap(),
            Value::symbol("undecided-unix")
        );
        assert_eq!(
            builtin_check_coding_system(&m, vec![Value::symbol("utf-8-auto-unix")]).unwrap(),
            Value::symbol("utf-8-auto-unix")
        );
        assert_eq!(
            builtin_check_coding_system(&m, vec![Value::symbol("prefer-utf-8-unix")]).unwrap(),
            Value::symbol("prefer-utf-8-unix")
        );
    }

    #[test]
    fn check_coding_system_rejects_unsupported_derived_variants() {
        let m = mgr();
        assert!(
            builtin_check_coding_system(&m, vec![Value::symbol("no-conversion-unix")]).is_err()
        );
        assert!(builtin_check_coding_system(&m, vec![Value::symbol("binary-unix")]).is_err());
        assert!(
            builtin_check_coding_system(&m, vec![Value::symbol("emacs-internal-unix")]).is_err()
        );
    }

    #[test]
    fn check_coding_systems_region_semantics() {
        let m = mgr();
        assert!(builtin_check_coding_systems_region(
            &m,
            vec![
                Value::Int(1),
                Value::Int(1),
                Value::list(vec![Value::symbol("utf-8")])
            ]
        )
        .unwrap()
        .is_nil());
        assert!(builtin_check_coding_systems_region(
            &m,
            vec![Value::string("x"), Value::Int(1), Value::symbol("utf-8")]
        )
        .unwrap()
        .is_nil());

        let type_err = builtin_check_coding_systems_region(
            &m,
            vec![Value::Int(1), Value::string("x"), Value::symbol("utf-8")],
        )
        .unwrap_err();
        match type_err {
            Flow::Signal(sig) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(
                    sig.data,
                    vec![Value::symbol("integer-or-marker-p"), Value::string("x")]
                );
            }
            other => panic!("expected wrong-type-argument, got {other:?}"),
        }

        assert!(builtin_check_coding_systems_region(&m, vec![]).is_err());
        assert!(
            builtin_check_coding_systems_region(&m, vec![Value::Int(1), Value::Int(1)]).is_err()
        );
    }

    #[test]
    fn set_keyboard_coding_system_rejects_unsuitable_variants() {
        let mut m = mgr();
        let auto = builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("utf-8-auto")]);
        let auto_derived =
            builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("utf-8-auto-unix")]);
        let prefer =
            builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("prefer-utf-8")]);
        let prefer_derived =
            builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("prefer-utf-8-unix")]);
        let undecided =
            builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("undecided")]);
        let undecided_derived =
            builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("undecided-unix")]);

        assert!(auto.is_err());
        assert!(auto_derived.is_err());
        assert!(prefer.is_err());
        assert!(prefer_derived.is_err());
        assert!(undecided.is_err());
        assert!(undecided_derived.is_err());
    }

    #[test]
    fn set_keyboard_coding_system_preserves_emacs_internal() {
        let mut m = mgr();
        let set = builtin_set_keyboard_coding_system(&mut m, vec![Value::symbol("emacs-internal")])
            .unwrap();
        assert_eq!(set, Value::symbol("emacs-internal"));

        let get = builtin_keyboard_coding_system(&m, vec![]).unwrap();
        assert_eq!(get, Value::symbol("emacs-internal"));
    }

    #[test]
    fn find_coding_system_known_and_unknown() {
        let m = mgr();
        let known = builtin_find_coding_system(&m, vec![Value::symbol("utf-8")]).unwrap();
        assert_eq!(known, Value::symbol("utf-8"));

        let unknown =
            builtin_find_coding_system(&m, vec![Value::symbol("vm-no-such-coding")]).unwrap();
        assert_eq!(unknown, Value::Nil);
    }

    #[test]
    fn set_coding_system_priority_reorders_front_in_arg_order() {
        let mut m = mgr();
        builtin_set_coding_system_priority(
            &mut m,
            vec![Value::symbol("raw-text"), Value::symbol("utf-8")],
        )
        .unwrap();

        let list = builtin_coding_system_priority_list(&m, vec![]).unwrap();
        let items = list_to_vec(&list).unwrap();
        assert!(matches!(&items[0], Value::Symbol(id) if resolve_sym(*id) == "raw-text"));
        assert!(matches!(&items[1], Value::Symbol(id) if resolve_sym(*id) == "utf-8"));
    }

    #[test]
    fn set_coding_system_priority_rejects_nil_payload() {
        let mut m = mgr();
        let result = builtin_set_coding_system_priority(&mut m, vec![Value::Nil]);
        match result {
            Err(Flow::Signal(sig)) => {
                assert_eq!(sig.symbol_name(), "wrong-type-argument");
                assert_eq!(sig.data, vec![Value::symbol("coding-system-p"), Value::Nil]);
            }
            other => panic!("expected wrong-type-argument signal, got {other:?}"),
        }
    }

    #[test]
    fn set_coding_system_priority_keyword_signals_coding_system_error() {
        let mut m = mgr();
        let result = builtin_set_coding_system_priority(&mut m, vec![Value::keyword(":utf-8")]);
        match result {
            Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "coding-system-error"),
            other => panic!("expected coding-system-error signal, got {other:?}"),
        }
    }

    #[test]
    fn set_coding_system_priority_string_is_type_error() {
        let mut m = mgr();
        let result = builtin_set_coding_system_priority(&mut m, vec![Value::string("utf-8")]);
        match result {
            Err(Flow::Signal(sig)) => assert_eq!(sig.symbol_name(), "wrong-type-argument"),
            other => panic!("expected wrong-type-argument signal, got {other:?}"),
        }
    }

    #[test]
    fn internal_coding_system_setters_match_surface_validation() {
        let mut m = mgr();
        assert_eq!(
            builtin_set_keyboard_coding_system_internal(&mut m, vec![Value::symbol("utf-8")])
                .unwrap(),
            Value::Nil
        );
        assert_eq!(
            builtin_set_terminal_coding_system_internal(&mut m, vec![Value::symbol("utf-8")])
                .unwrap(),
            Value::Nil
        );
        assert_eq!(
            builtin_set_safe_terminal_coding_system_internal(&mut m, vec![Value::symbol("utf-8")])
                .unwrap(),
            Value::Nil
        );
        assert!(
            builtin_set_keyboard_coding_system_internal(&mut m, vec![Value::symbol("foo")])
                .is_err()
        );
        assert!(
            builtin_set_terminal_coding_system_internal(&mut m, vec![Value::symbol("foo")])
                .is_err()
        );
        assert!(builtin_set_safe_terminal_coding_system_internal(
            &mut m,
            vec![Value::symbol("foo")]
        )
        .is_err());
    }

    #[test]
    fn text_quoting_and_conversion_style_basics() {
        assert_eq!(
            builtin_text_quoting_style(vec![]).expect("text-quoting-style"),
            Value::symbol("curve")
        );
        assert!(builtin_text_quoting_style(vec![Value::Nil]).is_err());
        assert_eq!(
            builtin_set_text_conversion_style(vec![Value::symbol("latin-1")])
                .expect("set-text-conversion-style"),
            Value::Nil
        );
        assert_eq!(
            builtin_set_text_conversion_style(vec![Value::symbol("foo"), Value::symbol("bar")])
                .expect("set-text-conversion-style 2 args"),
            Value::Nil
        );
        assert!(builtin_set_text_conversion_style(vec![]).is_err());
    }
}
