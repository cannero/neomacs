//! Charset builtins for the Elisp interpreter.
//!
//! Charsets in Emacs define sets of characters with encoding properties.
//! For neovm we primarily support Unicode; other charsets are registered
//! for compatibility but map through to the Unicode code-point space.
//!
//! The `CharsetRegistry` stores known charset names, IDs, and plists.
//! It is initialized with the standard charsets: ascii, unicode,
//! unicode-bmp, latin-iso8859-1, emacs, and eight-bit.

use super::error::{EvalResult, Flow, signal};
use super::intern::resolve_sym;
use super::value::*;
use std::collections::{HashMap, HashSet};

const RAW_BYTE_SENTINEL_MIN: u32 = 0xE080;
const RAW_BYTE_SENTINEL_MAX: u32 = 0xE0FF;
const UNIBYTE_BYTE_SENTINEL_MIN: u32 = 0xE300;
const UNIBYTE_BYTE_SENTINEL_MAX: u32 = 0xE3FF;

// ---------------------------------------------------------------------------
// Charset data types
// ---------------------------------------------------------------------------

/// How a charset maps code points to characters.
#[derive(Clone, Debug)]
enum CharsetMethod {
    /// code → code + offset (most common, e.g. ASCII, latin-iso8859-1)
    Offset(i64),
    /// Explicit mapping table (currently unused beyond registration)
    Map,
    /// Subset of another charset
    Subset,
    /// Superset of other charsets
    Superset,
}

/// Information about a single charset.
#[derive(Clone, Debug)]
struct CharsetInfo {
    id: i64,
    name: String,
    dimension: i64,
    code_space: [i64; 8],
    min_code: i64,
    max_code: i64,
    iso_final_char: Option<i64>,
    iso_revision: Option<i64>,
    emacs_mule_id: Option<i64>,
    ascii_compatible_p: bool,
    supplementary_p: bool,
    invalid_code: Option<i64>,
    method: CharsetMethod,
    plist: Vec<(String, Value)>,
}

/// Registry of known charsets, keyed by name.
pub(crate) struct CharsetRegistry {
    charsets: HashMap<String, CharsetInfo>,
    /// Priority-ordered list of charset names.
    priority: Vec<String>,
    /// Next auto-assigned charset ID.
    next_id: i64,
}

impl CharsetRegistry {
    /// Create a new registry pre-populated with the standard charsets.
    pub fn new() -> Self {
        let mut reg = Self {
            charsets: HashMap::new(),
            priority: Vec::new(),
            next_id: 256, // start above the Emacs built-in range
        };
        reg.init_standard_charsets();
        reg
    }

    fn make_default(id: i64, name: &str) -> CharsetInfo {
        CharsetInfo {
            id,
            name: name.to_string(),
            dimension: 1,
            code_space: [0, 127, 0, 0, 0, 0, 0, 0],
            min_code: 0,
            max_code: 127,
            iso_final_char: None,
            iso_revision: None,
            emacs_mule_id: None,
            ascii_compatible_p: false,
            supplementary_p: false,
            invalid_code: None,
            method: CharsetMethod::Offset(0),
            plist: vec![],
        }
    }

    fn init_standard_charsets(&mut self) {
        let mut ascii = Self::make_default(0, "ascii");
        ascii.ascii_compatible_p = true;
        self.register(ascii);

        let mut unicode = Self::make_default(2, "unicode");
        unicode.dimension = 3;
        unicode.code_space = [0, 255, 0, 255, 0, 16, 0, 0];
        unicode.max_code = 0x10FFFF;
        self.register(unicode);

        let mut bmp = Self::make_default(144, "unicode-bmp");
        bmp.dimension = 2;
        bmp.code_space = [0, 255, 0, 255, 0, 0, 0, 0];
        bmp.max_code = 0xFFFF;
        self.register(bmp);

        let mut latin1 = Self::make_default(5, "latin-iso8859-1");
        latin1.code_space = [32, 127, 0, 0, 0, 0, 0, 0];
        latin1.min_code = 32;
        latin1.method = CharsetMethod::Offset(160);
        self.register(latin1);

        let mut emacs = Self::make_default(3, "emacs");
        emacs.dimension = 3;
        emacs.code_space = [0, 255, 0, 255, 0, 63, 0, 0];
        emacs.max_code = 0x3FFF7F;
        self.register(emacs);

        let mut eight_bit = Self::make_default(4, "eight-bit");
        eight_bit.code_space = [128, 255, 0, 0, 0, 0, 0, 0];
        eight_bit.min_code = 128;
        eight_bit.max_code = 255;
        eight_bit.supplementary_p = true;
        eight_bit.method = CharsetMethod::Offset(0x3FFF00);
        self.register(eight_bit);

        // Standard aliases matching official Emacs C charset.c registrations.
        self.define_alias("iso-8859-1", "latin-iso8859-1");
        self.define_alias("ucs", "unicode");

        // Default priority order.
        self.priority = vec![
            "unicode".to_string(),
            "emacs".to_string(),
            "ascii".to_string(),
            "unicode-bmp".to_string(),
            "latin-iso8859-1".to_string(),
            "eight-bit".to_string(),
        ];
    }

    fn register(&mut self, info: CharsetInfo) {
        self.charsets.insert(info.name.clone(), info);
    }

    /// Allocate the next auto-incrementing charset ID.
    fn alloc_id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Return true if a charset with the given name exists.
    pub fn contains(&self, name: &str) -> bool {
        self.charsets.contains_key(name)
    }

    /// Return the list of all charset names (unordered).
    #[cfg(test)]
    pub fn names(&self) -> Vec<String> {
        self.charsets.keys().cloned().collect()
    }

    /// Return the priority-ordered list of charset names.
    pub fn priority_list(&self) -> &[String] {
        &self.priority
    }

    /// Move the requested charset names to the front of the priority list
    /// (deduplicated, preserving relative order for remaining entries).
    pub fn set_priority(&mut self, requested: &[String]) {
        let mut seen = HashSet::with_capacity(self.priority.len() + requested.len());
        let mut reordered = Vec::with_capacity(self.priority.len() + requested.len());

        for name in requested {
            if seen.insert(name.clone()) {
                reordered.push(name.clone());
            }
        }

        for name in &self.priority {
            if seen.insert(name.clone()) {
                reordered.push(name.clone());
            }
        }

        self.priority = reordered;
    }

    /// Return the plist for a charset, or None if not found.
    pub fn plist(&self, name: &str) -> Option<&[(String, Value)]> {
        self.charsets.get(name).map(|info| info.plist.as_slice())
    }

    /// Return the internal ID for a charset, if known.
    pub fn id(&self, name: &str) -> Option<i64> {
        self.charsets.get(name).map(|info| info.id)
    }

    /// Register ALIAS as another name for TARGET.
    pub fn define_alias(&mut self, alias: &str, target: &str) {
        let Some(target_info) = self.charsets.get(target) else {
            return;
        };
        let mut aliased = target_info.clone();
        aliased.name = alias.to_string();
        self.charsets.insert(alias.to_string(), aliased);
    }

    /// Replace the plist for a charset.
    pub fn set_plist(&mut self, name: &str, plist: Vec<(String, Value)>) {
        if let Some(info) = self.charsets.get_mut(name) {
            info.plist = plist;
        }
    }

    /// Decode a code-point in the given charset to an Emacs internal
    /// character code.  Returns `None` when the code-point is outside
    /// the charset's valid range or the charset method cannot handle it.
    pub fn decode_char(&self, name: &str, code_point: i64) -> Option<i64> {
        let info = self.charsets.get(name)?;
        // Check code-point is within charset's valid range.
        if code_point < info.min_code || code_point > info.max_code {
            return None;
        }
        match &info.method {
            CharsetMethod::Offset(offset) => Some(code_point + offset),
            // Map / Subset / Superset not yet supported — return None.
            _ => None,
        }
    }

    /// Encode an Emacs internal character code back to a code-point in
    /// the given charset.  Returns `None` when the character cannot be
    /// represented in the charset.
    pub fn encode_char(&self, name: &str, ch: i64) -> Option<i64> {
        let info = self.charsets.get(name)?;
        match &info.method {
            CharsetMethod::Offset(offset) => {
                let code_point = ch - offset;
                if code_point >= info.min_code && code_point <= info.max_code {
                    Some(code_point)
                } else {
                    None
                }
            }
            // Map / Subset / Superset not yet supported — return None.
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Singleton registry
// ---------------------------------------------------------------------------

use std::cell::RefCell;

thread_local! {
    static CHARSET_REGISTRY: RefCell<CharsetRegistry> = RefCell::new(CharsetRegistry::new());
}

/// Reset charset registry to default state (called from Evaluator::new).
pub(crate) fn reset_charset_registry() {
    CHARSET_REGISTRY.with(|slot| *slot.borrow_mut() = CharsetRegistry::new());
}

/// Set the plist for a charset (used by `set-charset-plist` builtin).
pub(crate) fn set_charset_plist_registry(name: &str, plist: Vec<(String, Value)>) {
    CHARSET_REGISTRY.with(|slot| slot.borrow_mut().set_plist(name, plist));
}

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

fn expect_int_or_marker(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *other],
        )),
    }
}

fn require_known_charset(value: &Value) -> Result<String, Flow> {
    let name = match value {
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("charsetp"), *other],
            ));
        }
    };
    let known = CHARSET_REGISTRY.with(|slot| slot.borrow().contains(&name));
    if known {
        Ok(name)
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("charsetp"), Value::symbol(name)],
        ))
    }
}

fn decode_char_codepoint_arg(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) if *n >= 0 => Ok(*n),
        Value::Float(f, _)
            if f.is_finite() && *f >= 0.0 && f.fract() == 0.0 && *f <= i64::MAX as f64 =>
        {
            Ok(*f as i64)
        }
        _ => Err(signal(
            "error",
            vec![Value::string(
                "Not an in-range integer, integral float, or cons of integers",
            )],
        )),
    }
}

fn expect_wholenump(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) if *n >= 0 => Ok(*n),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("wholenump"), *other],
        )),
    }
}

fn expect_fixnump(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Int(n) => Ok(*n),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("fixnump"), *other],
        )),
    }
}

fn encode_char_input(value: &Value) -> Result<i64, Flow> {
    match value {
        Value::Char(c) => Ok(*c as i64),
        Value::Int(n) if (0..=0x3FFFFF).contains(n) => Ok(*n),
        other => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *other],
        )),
    }
}

// ---------------------------------------------------------------------------
// Pure builtins (Vec<Value> -> EvalResult)
// ---------------------------------------------------------------------------

/// `(charsetp OBJECT)` -- return t if OBJECT names a known charset.
pub(crate) fn builtin_charsetp(args: Vec<Value>) -> EvalResult {
    expect_args("charsetp", &args, 1)?;
    let name = match &args[0] {
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        _ => return Ok(Value::Nil),
    };
    let found = CHARSET_REGISTRY.with(|slot| slot.borrow().contains(&name));
    Ok(Value::bool(found))
}

/// `(charset-list)` -- return charset symbols in priority order.
#[cfg(test)]
pub(crate) fn builtin_charset_list(args: Vec<Value>) -> EvalResult {
    expect_args("charset-list", &args, 0)?;
    let names: Vec<Value> = CHARSET_REGISTRY.with(|slot| {
        slot.borrow()
            .priority_list()
            .iter()
            .map(|name| Value::symbol(name.clone()))
            .collect()
    });
    Ok(Value::list(names))
}

/// `(unibyte-charset)` -- return the charset used for unibyte strings.
#[cfg(test)]
pub(crate) fn builtin_unibyte_charset(args: Vec<Value>) -> EvalResult {
    expect_args("unibyte-charset", &args, 0)?;
    Ok(Value::symbol("eight-bit"))
}

/// `(charset-priority-list &optional HIGHESTP)` -- return list of charsets
/// in priority order.  If HIGHESTP is non-nil, return only the highest
/// priority charset.
pub(crate) fn builtin_charset_priority_list(args: Vec<Value>) -> EvalResult {
    expect_max_args("charset-priority-list", &args, 1)?;
    let highestp = args.first().map(|v| v.is_truthy()).unwrap_or(false);
    CHARSET_REGISTRY.with(|slot| {
        let reg = slot.borrow();
        let priority = reg.priority_list();
        if highestp {
            if let Some(first) = priority.first() {
                Ok(Value::list(vec![Value::symbol(first.clone())]))
            } else {
                Ok(Value::Nil)
            }
        } else {
            let syms: Vec<Value> = priority.iter().map(|s| Value::symbol(s.clone())).collect();
            Ok(Value::list(syms))
        }
    })
}

/// `(set-charset-priority &rest CHARSETS)` -- set charset detection priority.
pub(crate) fn builtin_set_charset_priority(args: Vec<Value>) -> EvalResult {
    expect_min_args("set-charset-priority", &args, 1)?;

    let mut requested = Vec::with_capacity(args.len());
    for arg in &args {
        let name = match arg {
            Value::Symbol(id) => resolve_sym(*id).to_owned(),
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("charsetp"), *arg],
                ));
            }
        };
        let known = CHARSET_REGISTRY.with(|slot| slot.borrow().contains(&name));
        if !known {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("charsetp"), *arg],
            ));
        }
        requested.push(name);
    }
    CHARSET_REGISTRY.with(|slot| slot.borrow_mut().set_priority(&requested));
    Ok(Value::Nil)
}

/// `(char-charset CH &optional RESTRICTION)` -- return charset for character.
/// Mirrors Emacs baseline behavior:
/// - ASCII characters map to `ascii`
/// - BMP non-ASCII characters map to `unicode-bmp`
/// - non-BMP Unicode characters map to `unicode`
pub(crate) fn builtin_char_charset(args: Vec<Value>) -> EvalResult {
    expect_min_args("char-charset", &args, 1)?;
    expect_max_args("char-charset", &args, 2)?;
    let ch = encode_char_input(&args[0])?;
    let charset = if (0..=0x7F).contains(&ch) {
        "ascii"
    } else if ch <= 0xFFFF {
        "unicode-bmp"
    } else {
        "unicode"
    };
    Ok(Value::symbol(charset))
}

/// `(charset-plist CHARSET)` -- return property list for CHARSET.
pub(crate) fn builtin_charset_plist(args: Vec<Value>) -> EvalResult {
    expect_args("charset-plist", &args, 1)?;
    let name = require_known_charset(&args[0])?;
    CHARSET_REGISTRY.with(|slot| {
        let reg = slot.borrow();
        if let Some(pairs) = reg.plist(&name) {
            let mut elems = Vec::with_capacity(pairs.len() * 2);
            for (key, val) in pairs {
                elems.push(Value::symbol(key.clone()));
                elems.push(*val);
            }
            Ok(Value::list(elems))
        } else {
            Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("charsetp"), Value::symbol(name)],
            ))
        }
    })
}

/// `(charset-id-internal &optional CHARSET)` -- return internal charset id.
pub(crate) fn builtin_charset_id_internal(args: Vec<Value>) -> EvalResult {
    expect_max_args("charset-id-internal", &args, 1)?;
    let arg = args.first().cloned().unwrap_or(Value::Nil);
    let name = match &arg {
        Value::Symbol(id) => resolve_sym(*id),
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("charsetp"), arg],
            ));
        }
    };

    CHARSET_REGISTRY.with(|slot| {
        let reg = slot.borrow();
        if let Some(id) = reg.id(name) {
            Ok(Value::Int(id))
        } else {
            Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("charsetp"), Value::symbol(name)],
            ))
        }
    })
}

/// Extract an integer from a Value, or return 0 for nil.
fn int_or_zero(val: &Value) -> i64 {
    match val {
        Value::Int(n) => *n,
        Value::Char(c) => *c as i64,
        _ => 0,
    }
}

/// Extract an optional integer from a Value (nil → None).
fn opt_int(val: &Value) -> Option<i64> {
    match val {
        Value::Int(n) => Some(*n),
        Value::Char(c) => Some(*c as i64),
        Value::Nil => None,
        _ => None,
    }
}

/// Decode a code point argument that may be a plain int or a cons (HI . LO).
fn decode_code_arg(val: &Value) -> i64 {
    match val {
        Value::Int(n) => *n,
        Value::Char(c) => *c as i64,
        Value::Cons(id) => {
            let pair = read_cons(*id);
            let hi = int_or_zero(&pair.car);
            let lo = int_or_zero(&pair.cdr);
            (hi << 16) | lo
        }
        _ => 0,
    }
}

/// Parse a plist Value into a Vec of (key, value) pairs.
fn parse_plist(val: &Value) -> Vec<(String, Value)> {
    let mut result = Vec::new();
    let Some(items) = list_to_vec(val) else {
        return result;
    };
    let mut i = 0;
    while i + 1 < items.len() {
        if let Some(key) = items[i].as_symbol_name() {
            result.push((key.to_string(), items[i + 1]));
        }
        i += 2;
    }
    result
}

/// `(define-charset-internal NAME DIM CODE-SPACE MIN-CODE MAX-CODE
///    ISO-FINAL ISO-REVISION EMACS-MULE-ID ASCII-COMPAT-P SUPPLEMENTARY-P
///    INVALID-CODE CODE-OFFSET MAP SUBSET SUPERSET UNIFY-MAP PLIST)`
///
/// Internal charset initializer — registers a charset in the registry.
/// Accepts exactly 17 arguments matching the Emacs C function.
pub(crate) fn builtin_define_charset_internal(args: Vec<Value>) -> EvalResult {
    expect_args("define-charset-internal", &args, 17)?;

    // arg[0]: name (symbol)
    let name = match &args[0] {
        Value::Symbol(id) => resolve_sym(*id).to_owned(),
        other => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), *other],
            ));
        }
    };

    // arg[1]: dimension (vector or integer — the define-charset macro passes
    //         a vector of the form [dim ...], but we also accept a plain int)
    let dimension = match &args[1] {
        Value::Int(n) => *n,
        Value::Vector(id) => {
            let vec = with_heap(|h| h.get_vector(*id).clone());
            if vec.is_empty() {
                return Err(signal("args-out-of-range", vec![args[1], Value::Int(0)]));
            }
            int_or_zero(&vec[0])
        }
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("arrayp"), args[1]],
            ));
        }
    };

    // arg[2]: code-space (vector of 8 integers — byte ranges per dimension)
    let code_space = match &args[2] {
        Value::Vector(id) => {
            let vec = with_heap(|h| h.get_vector(*id).clone());
            if vec.len() < 2 {
                return Err(signal(
                    "args-out-of-range",
                    vec![args[2], Value::Int(vec.len() as i64)],
                ));
            }
            let mut cs = [0i64; 8];
            for (i, v) in vec.iter().enumerate().take(8) {
                cs[i] = int_or_zero(v);
            }
            cs
        }
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("arrayp"), args[2]],
            ));
        }
    };

    // Compute default min/max code from code-space, matching official Emacs
    // charset.c: min = cs[0] | cs[2]<<8 | cs[4]<<16 | cs[6]<<24
    let cs_min =
        code_space[0] | (code_space[2] << 8) | (code_space[4] << 16) | (code_space[6] << 24);
    let cs_max =
        code_space[1] | (code_space[3] << 8) | (code_space[5] << 16) | (code_space[7] << 24);

    // arg[3]: min-code, arg[4]: max-code (override from code-space if given)
    let min_code = if args[3].is_nil() {
        cs_min
    } else {
        decode_code_arg(&args[3])
    };
    let max_code = if args[4].is_nil() {
        cs_max
    } else {
        decode_code_arg(&args[4])
    };

    // arg[5]: iso-final-char (char or nil)
    let iso_final_char = opt_int(&args[5]);

    // arg[6]: iso-revision (int or nil)
    let iso_revision = opt_int(&args[6]);

    // arg[7]: emacs-mule-id (int or nil)
    let emacs_mule_id = opt_int(&args[7]);

    // arg[8]: ascii-compatible-p
    let ascii_compatible_p = args[8].is_truthy();

    // arg[9]: supplementary-p
    let supplementary_p = args[9].is_truthy();

    // arg[10]: invalid-code (int or nil)
    let invalid_code = opt_int(&args[10]);

    // arg[11]: code-offset  → CHARSET_METHOD_OFFSET
    // arg[12]: map           → CHARSET_METHOD_MAP
    // arg[13]: subset        → CHARSET_METHOD_SUBSET
    // arg[14]: superset      → CHARSET_METHOD_SUPERSET
    let method = if !args[11].is_nil() {
        CharsetMethod::Offset(int_or_zero(&args[11]))
    } else if !args[12].is_nil() {
        CharsetMethod::Map
    } else if !args[13].is_nil() {
        CharsetMethod::Subset
    } else if !args[14].is_nil() {
        CharsetMethod::Superset
    } else {
        // Default to offset 0 if nothing specified
        CharsetMethod::Offset(0)
    };

    // arg[15]: unify-map (ignored for now — used for Unicode unification)
    // arg[16]: plist
    let plist = parse_plist(&args[16]);

    CHARSET_REGISTRY.with(|slot| {
        let mut reg = slot.borrow_mut();
        // Use emacs-mule-id as the charset ID if provided and no collision,
        // otherwise auto-allocate.
        let id = if let Some(mule_id) = emacs_mule_id {
            mule_id
        } else {
            reg.alloc_id()
        };

        let info = CharsetInfo {
            id,
            name: name.clone(),
            dimension,
            code_space,
            min_code,
            max_code,
            iso_final_char,
            iso_revision,
            emacs_mule_id,
            ascii_compatible_p,
            supplementary_p,
            invalid_code,
            method,
            plist,
        };
        reg.register(info);
    });

    Ok(Value::Nil)
}

/// `(find-charset-region BEG END &optional TABLE)` -- returns a list of charsets
/// present in the buffer slice.
pub(crate) fn builtin_find_charset_region(args: Vec<Value>) -> EvalResult {
    expect_min_args("find-charset-region", &args, 2)?;
    expect_max_args("find-charset-region", &args, 3)?;
    Ok(Value::list(vec![Value::symbol("ascii")]))
}

/// Evaluator-aware variant of `(find-charset-region BEG END &optional TABLE)`.
///
/// Returns charset symbols present in the region `[BEG, END)` where BEG/END are
/// Emacs 1-based character positions inside the accessible region.
pub(crate) fn builtin_find_charset_region_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_find_charset_region_in_manager(&eval.buffers, args)
}

pub(crate) fn builtin_find_charset_region_in_manager(
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("find-charset-region", &args, 2)?;
    expect_max_args("find-charset-region", &args, 3)?;
    let beg = expect_int_or_marker(&args[0])?;
    let end = expect_int_or_marker(&args[1])?;

    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    let point_min = buf.point_min_char() as i64 + 1;
    let point_max = buf.point_max_char() as i64 + 1;
    if beg < point_min || beg > point_max || end < point_min || end > point_max {
        return Err(signal(
            "args-out-of-range",
            vec![Value::Int(beg), Value::Int(end)],
        ));
    }

    let mut a = beg;
    let mut b = end;
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }

    let start_byte = buf.text.char_to_byte((a - 1).max(0) as usize);
    let end_byte = buf.text.char_to_byte((b - 1).max(0) as usize);
    if start_byte == end_byte {
        return Ok(Value::list(vec![Value::symbol("ascii")]));
    }

    let text = buf.buffer_substring(start_byte, end_byte);
    let charsets = classify_string_charsets(&text);
    if charsets.is_empty() {
        return Ok(Value::list(vec![Value::symbol("ascii")]));
    }
    Ok(Value::list(
        charsets.into_iter().map(Value::symbol).collect::<Vec<_>>(),
    ))
}

/// `(encode-big5-char CH)` -- encode character CH in BIG5 space.
pub(crate) fn builtin_encode_big5_char(args: Vec<Value>) -> EvalResult {
    expect_args("encode-big5-char", &args, 1)?;
    let ch = encode_char_input(&args[0])?;
    Ok(Value::Int(ch))
}

/// `(decode-big5-char CODE)` -- decode BIG5 code to Emacs character code.
pub(crate) fn builtin_decode_big5_char(args: Vec<Value>) -> EvalResult {
    expect_args("decode-big5-char", &args, 1)?;
    let code = expect_wholenump(&args[0])?;
    Ok(Value::Int(code))
}

/// `(encode-sjis-char CH)` -- encode character CH in Shift-JIS space.
pub(crate) fn builtin_encode_sjis_char(args: Vec<Value>) -> EvalResult {
    expect_args("encode-sjis-char", &args, 1)?;
    let ch = encode_char_input(&args[0])?;
    Ok(Value::Int(ch))
}

/// `(decode-sjis-char CODE)` -- decode Shift-JIS code to Emacs character code.
pub(crate) fn builtin_decode_sjis_char(args: Vec<Value>) -> EvalResult {
    expect_args("decode-sjis-char", &args, 1)?;
    let code = expect_wholenump(&args[0])?;
    Ok(Value::Int(code))
}

/// `(get-unused-iso-final-char DIMENSION CHARS)` -- return an available ISO
/// final-char code for the requested DIMENSION/CHARS class.
pub(crate) fn builtin_get_unused_iso_final_char(args: Vec<Value>) -> EvalResult {
    expect_args("get-unused-iso-final-char", &args, 2)?;
    let dimension = expect_fixnump(&args[0])?;
    let chars = expect_fixnump(&args[1])?;
    if !matches!(dimension, 1..=3) {
        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Invalid DIMENSION {dimension}, it should be 1, 2, or 3"
            ))],
        ));
    }
    if !matches!(chars, 94 | 96) {
        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Invalid CHARS {chars}, it should be 94 or 96"
            ))],
        ));
    }
    let final_char = match (dimension, chars) {
        (1, 94) => 54,
        (1, 96) => 51,
        (2, 94) => 50,
        (2, 96) | (3, 94) | (3, 96) => 48,
        _ => 48,
    };
    Ok(Value::Int(final_char))
}

/// `(declare-equiv-charset DIMENSION CHARS CH CHARSET)` -- declare an
/// equivalent charset mapping tuple.
pub(crate) fn builtin_declare_equiv_charset(args: Vec<Value>) -> EvalResult {
    expect_args("declare-equiv-charset", &args, 4)?;
    let _charset = require_known_charset(&args[3])?;
    let dimension = expect_fixnump(&args[0])?;
    let chars = expect_fixnump(&args[1])?;
    let _ch = encode_char_input(&args[2])?;
    if !matches!(dimension, 1..=3) {
        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Invalid DIMENSION {dimension}, it should be 1, 2, or 3"
            ))],
        ));
    }
    if !matches!(chars, 94 | 96) {
        return Err(signal(
            "error",
            vec![Value::string(format!(
                "Invalid CHARS {chars}, it should be 94 or 96"
            ))],
        ));
    }
    Ok(Value::Nil)
}

/// `(define-charset-alias ALIAS CHARSET)` -- add ALIAS for CHARSET.
pub(crate) fn builtin_define_charset_alias(args: Vec<Value>) -> EvalResult {
    expect_args("define-charset-alias", &args, 2)?;
    let target = require_known_charset(&args[1])?;
    if let Value::Symbol(id) = &args[0] {
        let alias = resolve_sym(*id);
        CHARSET_REGISTRY.with(|slot| slot.borrow_mut().define_alias(alias, &target));
    }
    Ok(Value::Nil)
}

/// `(find-charset-string STR &optional TABLE)` -- returns a list of charsets
/// present in STR.
pub(crate) fn builtin_find_charset_string(args: Vec<Value>) -> EvalResult {
    expect_min_args("find-charset-string", &args, 1)?;
    expect_max_args("find-charset-string", &args, 2)?;
    if !args[0].is_string() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        ));
    }
    let s_ref = args[0].as_str().unwrap();

    let charsets = classify_string_charsets(s_ref);
    if charsets.is_empty() {
        Ok(Value::Nil)
    } else {
        Ok(Value::list(
            charsets.into_iter().map(Value::symbol).collect::<Vec<_>>(),
        ))
    }
}

/// `(decode-char CHARSET CODE-POINT)` -- decode code-point in CHARSET space.
///
/// Uses the charset's registered method (Offset, Map, etc.) to convert
/// a charset-specific code-point to an Emacs internal character code.
pub(crate) fn builtin_decode_char(args: Vec<Value>) -> EvalResult {
    expect_args("decode-char", &args, 2)?;
    let name = require_known_charset(&args[0])?;
    let code_point = decode_char_codepoint_arg(&args[1])?;

    let decoded = CHARSET_REGISTRY.with(|slot| slot.borrow().decode_char(&name, code_point));

    Ok(decoded.map_or(Value::Nil, Value::Int))
}

/// `(encode-char CH CHARSET)` -- encode CH in CHARSET space.
///
/// Uses the charset's registered method to convert an Emacs internal
/// character code back to a charset-specific code-point.
pub(crate) fn builtin_encode_char(args: Vec<Value>) -> EvalResult {
    expect_args("encode-char", &args, 2)?;
    let ch = encode_char_input(&args[0])?;
    let name = require_known_charset(&args[1])?;

    let encoded = CHARSET_REGISTRY.with(|slot| slot.borrow().encode_char(&name, ch));

    Ok(encoded.map_or(Value::Nil, Value::Int))
}

/// `(clear-charset-maps)` -- clear charset-related caches (currently no cache
/// state is stored) and return nil.
pub(crate) fn builtin_clear_charset_maps(args: Vec<Value>) -> EvalResult {
    expect_max_args("clear-charset-maps", &args, 0)?;
    Ok(Value::Nil)
}

/// `(charset-after &optional POS)` -- currently returns 'unicode for compatibility.
pub(crate) fn builtin_charset_after(args: Vec<Value>) -> EvalResult {
    expect_max_args("charset-after", &args, 1)?;
    Ok(Value::symbol("unicode"))
}

/// Evaluator-aware variant of `(charset-after &optional POS)`.
///
/// Returns the charset of the character at POS (1-based), or the character
/// after point when POS is omitted. Returns nil at end-of-buffer or for
/// out-of-range numeric positions.
pub(crate) fn builtin_charset_after_eval(
    eval: &mut super::eval::Evaluator,
    args: Vec<Value>,
) -> EvalResult {
    builtin_charset_after_in_manager(&eval.buffers, args)
}

pub(crate) fn builtin_charset_after_in_manager(
    buffers: &crate::buffer::BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("charset-after", &args, 1)?;
    let buf = buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    let target_byte = if let Some(pos) = args.first() {
        let pos = expect_int_or_marker(pos)?;
        let point_min = buf.point_min_char() as i64 + 1;
        let point_max = buf.point_max_char() as i64 + 1;
        if pos < point_min || pos > point_max {
            return Ok(Value::Nil);
        }
        buf.text.char_to_byte((pos - 1).max(0) as usize)
    } else {
        buf.point()
    };

    let point_max_byte = buf.point_max();
    if target_byte >= point_max_byte {
        return Ok(Value::Nil);
    }

    let Some(ch) = buf.char_after(target_byte) else {
        return Ok(Value::Nil);
    };
    let cp = ch as u32;
    let charset = if (RAW_BYTE_SENTINEL_MIN..=RAW_BYTE_SENTINEL_MAX).contains(&cp) {
        "eight-bit"
    } else if (UNIBYTE_BYTE_SENTINEL_MIN..=UNIBYTE_BYTE_SENTINEL_MAX).contains(&cp) {
        let byte = cp - UNIBYTE_BYTE_SENTINEL_MIN;
        if byte <= 0x7F { "ascii" } else { "eight-bit" }
    } else if cp <= 0x7F {
        "ascii"
    } else if cp <= 0xFFFF {
        "unicode-bmp"
    } else {
        "unicode"
    };
    Ok(Value::symbol(charset))
}

fn classify_string_charsets(s: &str) -> Vec<&'static str> {
    if s.is_empty() {
        return Vec::new();
    }

    let mut has_ascii = false;
    let mut has_unicode = false;
    let mut has_eight_bit = false;
    let mut has_unicode_bmp = false;

    for ch in s.chars() {
        let cp = ch as u32;
        if (RAW_BYTE_SENTINEL_MIN..=RAW_BYTE_SENTINEL_MAX).contains(&cp) {
            has_eight_bit = true;
            continue;
        }
        if (UNIBYTE_BYTE_SENTINEL_MIN..=UNIBYTE_BYTE_SENTINEL_MAX).contains(&cp) {
            let byte = cp - UNIBYTE_BYTE_SENTINEL_MIN;
            if byte <= 0x7F {
                has_ascii = true;
            } else {
                has_eight_bit = true;
            }
            continue;
        }

        if cp <= 0x7F {
            has_ascii = true;
        } else if cp <= 0xFFFF {
            has_unicode_bmp = true;
        } else {
            has_unicode = true;
        }
    }

    // Match Emacs ordering observed for find-charset-string:
    // ascii, unicode, eight-bit, unicode-bmp.
    let mut out = Vec::new();
    if has_ascii {
        out.push("ascii");
    }
    if has_unicode {
        out.push("unicode");
    }
    if has_eight_bit {
        out.push("eight-bit");
    }
    if has_unicode_bmp {
        out.push("unicode-bmp");
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "charset_test.rs"]
mod tests;
