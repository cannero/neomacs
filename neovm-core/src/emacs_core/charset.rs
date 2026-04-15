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
use super::intern::{SymId, intern, lookup_interned, resolve_sym};
use super::value::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

const RAW_BYTE_SENTINEL_MIN: u32 = 0xE080;
const RAW_BYTE_SENTINEL_MAX: u32 = 0xE0FF;
const UNIBYTE_BYTE_SENTINEL_MIN: u32 = 0xE300;
const UNIBYTE_BYTE_SENTINEL_MAX: u32 = 0xE3FF;

static CHARSET_MAP_CACHE: OnceLock<RwLock<HashMap<String, Option<CharsetMapData>>>> =
    OnceLock::new();

fn charset_map_cache() -> &'static RwLock<HashMap<String, Option<CharsetMapData>>> {
    CHARSET_MAP_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn charset_map_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../etc/charsets")
}

fn parse_hex_i64(value: &str) -> Option<i64> {
    i64::from_str_radix(value.trim().trim_start_matches("0x"), 16).ok()
}

fn parse_hex_range(value: &str) -> Option<(i64, i64)> {
    let value = value.trim();
    if let Some((from, to)) = value.split_once('-') {
        Some((parse_hex_i64(from)?, parse_hex_i64(to)?))
    } else {
        let code = parse_hex_i64(value)?;
        Some((code, code))
    }
}

fn parse_charset_map_file(path: &Path) -> Option<CharsetMapData> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut code_to_char = HashMap::new();
    let mut char_to_code = HashMap::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let source = fields.next()?;
        let target = fields.next()?;
        let (from, to) = parse_hex_range(source)?;
        let target_base = parse_hex_i64(target)?;
        for (index, code) in (from..=to).enumerate() {
            let ch = target_base + index as i64;
            code_to_char.insert(code, ch);
            char_to_code.insert(ch, code);
        }
    }

    Some(CharsetMapData {
        code_to_char,
        char_to_code,
    })
}

fn load_charset_map(map_name: &str) -> Option<CharsetMapData> {
    let key = map_name.to_string();
    if let Ok(cache) = charset_map_cache().read()
        && let Some(cached) = cache.get(&key)
    {
        return cached.clone();
    }

    let loaded = parse_charset_map_file(&charset_map_dir().join(format!("{map_name}.map")));
    if let Ok(mut cache) = charset_map_cache().write() {
        cache.insert(key, loaded.clone());
    }
    loaded
}

// ---------------------------------------------------------------------------
// Charset data types
// ---------------------------------------------------------------------------

/// How a charset maps code points to characters.
#[derive(Clone, Debug)]
enum CharsetMethod {
    /// code → code + offset (most common, e.g. ASCII, latin-iso8859-1)
    Offset(i64),
    /// Explicit mapping table backed by an Emacs `.map` file.
    Map(String),
    /// Subset of another charset
    Subset(CharsetSubsetSpec),
    /// Superset of other charsets
    Superset(Vec<(SymId, i64)>),
}

#[derive(Clone, Debug)]
pub(crate) enum CharsetMethodSnapshot {
    Offset(i64),
    Map(String),
    Subset(CharsetSubsetSpecSnapshot),
    Superset(Vec<(String, i64)>),
}

#[derive(Clone, Debug)]
struct CharsetMapData {
    code_to_char: HashMap<i64, i64>,
    char_to_code: HashMap<i64, i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct CharsetSubsetSpec {
    pub parent: SymId,
    pub parent_min_code: i64,
    pub parent_max_code: i64,
    pub offset: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct CharsetSubsetSpecSnapshot {
    pub parent: String,
    pub parent_min_code: i64,
    pub parent_max_code: i64,
    pub offset: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct CharsetInfoSnapshot {
    pub id: i64,
    pub name: String,
    pub dimension: i64,
    pub code_space: [i64; 8],
    pub min_code: i64,
    pub max_code: i64,
    pub iso_final_char: Option<i64>,
    pub iso_revision: Option<i64>,
    pub emacs_mule_id: Option<i64>,
    pub ascii_compatible_p: bool,
    pub supplementary_p: bool,
    pub invalid_code: Option<i64>,
    pub unify_map: Value,
    pub method: CharsetMethodSnapshot,
    pub plist: Vec<(String, Value)>,
}

#[derive(Clone, Debug)]
pub(crate) struct CharsetRegistrySnapshot {
    pub charsets: Vec<CharsetInfoSnapshot>,
    pub priority: Vec<String>,
    pub next_id: i64,
}

/// Information about a single charset.
#[derive(Clone, Debug)]
struct CharsetInfo {
    id: i64,
    name: SymId,
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
    unify_map: Value,
    method: CharsetMethod,
    plist: Vec<(SymId, Value)>,
}

/// Registry of known charsets, keyed by name.
pub(crate) struct CharsetRegistry {
    charsets: HashMap<SymId, CharsetInfo>,
    /// Priority-ordered list of charset names.
    priority: Vec<SymId>,
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
            name: intern(name),
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
            unify_map: Value::NIL,
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

        // iso-8859-1 is a full 0-255 charset with identity mapping
        // (code_offset=0, min_code=0, max_code=255, ascii_compatible=true),
        // matching the built-in definition in GNU Emacs charset.c.
        // This is distinct from latin-iso8859-1 which only covers the
        // right-hand part (code points 32-127 mapping to characters 160-255).
        let mut iso_8859_1 = Self::make_default(1, "iso-8859-1");
        iso_8859_1.code_space = [0, 255, 0, 0, 0, 0, 0, 0];
        iso_8859_1.min_code = 0;
        iso_8859_1.max_code = 255;
        iso_8859_1.ascii_compatible_p = true;
        iso_8859_1.method = CharsetMethod::Offset(0);
        self.register(iso_8859_1);

        self.define_alias(intern("ucs"), intern("unicode"));

        // Default priority order.
        self.priority = vec![
            intern("unicode"),
            intern("emacs"),
            intern("ascii"),
            intern("unicode-bmp"),
            intern("latin-iso8859-1"),
            intern("eight-bit"),
        ];
    }

    fn register(&mut self, info: CharsetInfo) {
        self.charsets.insert(info.name, info);
    }

    /// Allocate the next auto-incrementing charset ID.
    fn alloc_id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Return true if a charset with the given name exists.
    pub fn contains(&self, name: &str) -> bool {
        lookup_interned(name).is_some_and(|id| self.charsets.contains_key(&id))
    }

    pub fn contains_symbol(&self, name: SymId) -> bool {
        self.charsets.contains_key(&name)
    }

    /// Return the list of all charset names (unordered).
    #[cfg(test)]
    pub fn names(&self) -> Vec<String> {
        self.charsets
            .keys()
            .map(|name| resolve_sym(*name).to_string())
            .collect()
    }

    /// Return the priority-ordered list of charset names.
    pub fn priority_list(&self) -> &[SymId] {
        &self.priority
    }

    /// Move the requested charset names to the front of the priority list
    /// (deduplicated, preserving relative order for remaining entries).
    pub fn set_priority(&mut self, requested: &[SymId]) {
        let mut seen = HashSet::with_capacity(self.priority.len() + requested.len());
        let mut reordered = Vec::with_capacity(self.priority.len() + requested.len());

        for &name in requested {
            if seen.insert(name) {
                reordered.push(name);
            }
        }

        for &name in &self.priority {
            if seen.insert(name) {
                reordered.push(name);
            }
        }

        self.priority = reordered;
    }

    /// Return the plist for a charset, or None if not found.
    pub fn plist(&self, name: SymId) -> Option<&[(SymId, Value)]> {
        self.charsets.get(&name).map(|info| info.plist.as_slice())
    }

    /// Return the internal ID for a charset, if known.
    pub fn id(&self, name: SymId) -> Option<i64> {
        self.charsets.get(&name).map(|info| info.id)
    }

    /// Register ALIAS as another name for TARGET.
    pub fn define_alias(&mut self, alias: SymId, target: SymId) {
        let Some(target_info) = self.charsets.get(&target) else {
            return;
        };
        let mut aliased = target_info.clone();
        aliased.name = alias;
        self.charsets.insert(alias, aliased);
    }

    fn snapshot(&self) -> CharsetRegistrySnapshot {
        let mut charsets = self
            .charsets
            .values()
            .cloned()
            .map(|info| CharsetInfoSnapshot {
                id: info.id,
                name: resolve_sym(info.name).to_string(),
                dimension: info.dimension,
                code_space: info.code_space,
                min_code: info.min_code,
                max_code: info.max_code,
                iso_final_char: info.iso_final_char,
                iso_revision: info.iso_revision,
                emacs_mule_id: info.emacs_mule_id,
                ascii_compatible_p: info.ascii_compatible_p,
                supplementary_p: info.supplementary_p,
                invalid_code: info.invalid_code,
                unify_map: info.unify_map,
                method: match info.method {
                    CharsetMethod::Offset(offset) => CharsetMethodSnapshot::Offset(offset),
                    CharsetMethod::Map(ref map_name) => {
                        CharsetMethodSnapshot::Map(map_name.clone())
                    }
                    CharsetMethod::Subset(ref subset) => {
                        CharsetMethodSnapshot::Subset(CharsetSubsetSpecSnapshot {
                            parent: resolve_sym(subset.parent).to_string(),
                            parent_min_code: subset.parent_min_code,
                            parent_max_code: subset.parent_max_code,
                            offset: subset.offset,
                        })
                    }
                    CharsetMethod::Superset(ref members) => CharsetMethodSnapshot::Superset(
                        members
                            .iter()
                            .map(|(name, offset)| (resolve_sym(*name).to_string(), *offset))
                            .collect(),
                    ),
                },
                plist: info
                    .plist
                    .iter()
                    .map(|(key, value)| (resolve_sym(*key).to_string(), *value))
                    .collect(),
            })
            .collect::<Vec<_>>();
        charsets.sort_by(|left, right| left.name.cmp(&right.name));

        CharsetRegistrySnapshot {
            charsets,
            priority: self
                .priority
                .iter()
                .map(|name| resolve_sym(*name).to_string())
                .collect(),
            next_id: self.next_id,
        }
    }

    fn restore(snapshot: CharsetRegistrySnapshot) -> Self {
        let mut charsets = HashMap::with_capacity(snapshot.charsets.len());
        for info in snapshot.charsets {
            let name = intern(&info.name);
            charsets.insert(
                name,
                CharsetInfo {
                    id: info.id,
                    name,
                    dimension: info.dimension,
                    code_space: info.code_space,
                    min_code: info.min_code,
                    max_code: info.max_code,
                    iso_final_char: info.iso_final_char,
                    iso_revision: info.iso_revision,
                    emacs_mule_id: info.emacs_mule_id,
                    ascii_compatible_p: info.ascii_compatible_p,
                    supplementary_p: info.supplementary_p,
                    invalid_code: info.invalid_code,
                    unify_map: info.unify_map,
                    method: match info.method {
                        CharsetMethodSnapshot::Offset(offset) => CharsetMethod::Offset(offset),
                        CharsetMethodSnapshot::Map(map_name) => CharsetMethod::Map(map_name),
                        CharsetMethodSnapshot::Subset(subset) => {
                            CharsetMethod::Subset(CharsetSubsetSpec {
                                parent: intern(&subset.parent),
                                parent_min_code: subset.parent_min_code,
                                parent_max_code: subset.parent_max_code,
                                offset: subset.offset,
                            })
                        }
                        CharsetMethodSnapshot::Superset(members) => CharsetMethod::Superset(
                            members
                                .into_iter()
                                .map(|(name, offset)| (intern(&name), offset))
                                .collect(),
                        ),
                    },
                    plist: info
                        .plist
                        .into_iter()
                        .map(|(key, value)| (intern(&key), value))
                        .collect(),
                },
            );
        }

        Self {
            charsets,
            priority: snapshot
                .priority
                .into_iter()
                .map(|name| intern(&name))
                .collect(),
            next_id: snapshot.next_id,
        }
    }

    /// Replace the plist for a charset.
    pub fn set_plist(&mut self, name: SymId, plist: Vec<(SymId, Value)>) {
        if let Some(info) = self.charsets.get_mut(&name) {
            info.plist = plist;
        }
    }

    fn superset_members(info: &CharsetInfo) -> Vec<(SymId, i64)> {
        match &info.method {
            CharsetMethod::Superset(members) => members.clone(),
            _ => Vec::new(),
        }
    }

    /// Decode a code-point in the given charset to an Emacs internal
    /// character code.  Returns `None` when the code-point is outside
    /// the charset's valid range or the charset method cannot handle it.
    pub fn decode_char(&self, name: SymId, code_point: i64) -> Option<i64> {
        let info = self.charsets.get(&name)?;
        // Check code-point is within charset's valid range.
        if code_point < info.min_code || code_point > info.max_code {
            return None;
        }
        if let Some(unify_map) = charset_value_text(&info.unify_map)
            && let Some(decoded) = load_charset_map(&unify_map)
                .and_then(|map| map.code_to_char.get(&code_point).copied())
        {
            return Some(decoded);
        }
        match &info.method {
            CharsetMethod::Offset(offset) => Some(code_point + offset),
            CharsetMethod::Map(map_name) => load_charset_map(map_name)
                .and_then(|map| map.code_to_char.get(&code_point).copied()),
            CharsetMethod::Subset(subset) => {
                let parent_code = code_point - subset.offset;
                if parent_code < subset.parent_min_code || parent_code > subset.parent_max_code {
                    None
                } else {
                    self.decode_char(subset.parent, parent_code)
                }
            }
            CharsetMethod::Superset(_) => {
                Self::superset_members(info)
                    .into_iter()
                    .find_map(|(parent_name, code_offset)| {
                        self.decode_char(parent_name, code_point - code_offset)
                    })
            }
        }
    }

    /// Encode an Emacs internal character code back to a code-point in
    /// the given charset.  Returns `None` when the character cannot be
    /// represented in the charset.
    pub fn encode_char(&self, name: SymId, ch: i64) -> Option<i64> {
        let info = self.charsets.get(&name)?;
        if let Some(unify_map) = charset_value_text(&info.unify_map)
            && let Some(encoded) =
                load_charset_map(&unify_map).and_then(|map| map.char_to_code.get(&ch).copied())
        {
            return Some(encoded);
        }
        match &info.method {
            CharsetMethod::Offset(offset) => {
                let code_point = ch - offset;
                if code_point >= info.min_code && code_point <= info.max_code {
                    Some(code_point)
                } else {
                    None
                }
            }
            CharsetMethod::Map(map_name) => {
                load_charset_map(map_name).and_then(|map| map.char_to_code.get(&ch).copied())
            }
            CharsetMethod::Subset(subset) => {
                let parent_code = self.encode_char(subset.parent, ch)?;
                if parent_code < subset.parent_min_code || parent_code > subset.parent_max_code {
                    None
                } else {
                    Some(parent_code + subset.offset)
                }
            }
            CharsetMethod::Superset(_) => {
                Self::superset_members(info)
                    .into_iter()
                    .find_map(|(parent_name, code_offset)| {
                        self.encode_char(parent_name, ch)
                            .map(|code| code + code_offset)
                    })
            }
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

/// Reset charset registry to default state (called from Context::new).
pub(crate) fn reset_charset_registry() {
    CHARSET_REGISTRY.with(|slot| *slot.borrow_mut() = CharsetRegistry::new());
    if let Ok(mut cache) = charset_map_cache().write() {
        cache.clear();
    }
}

/// Collect GC roots from charset runtime state.
///
/// GNU Emacs keeps charset Lisp attributes reachable from `Vcharset_hash_table`
/// and also marks `charset_table[i].attributes` in `mark_charset`.  Neomacs's
/// Rust-side charset registry stores the plist values directly, so those Lisp
/// values must be surfaced explicitly as GC roots.
pub(crate) fn collect_charset_gc_roots(roots: &mut Vec<Value>) {
    CHARSET_REGISTRY.with(|slot| {
        let reg = slot.borrow();
        for info in reg.charsets.values() {
            if !info.unify_map.is_nil() {
                roots.push(info.unify_map);
            }
            for (_, value) in &info.plist {
                roots.push(*value);
            }
        }
    });
}

pub(crate) fn snapshot_charset_registry() -> CharsetRegistrySnapshot {
    CHARSET_REGISTRY.with(|slot| slot.borrow().snapshot())
}

pub(crate) fn restore_charset_registry(snapshot: CharsetRegistrySnapshot) {
    CHARSET_REGISTRY.with(|slot| *slot.borrow_mut() = CharsetRegistry::restore(snapshot));
}

/// Set the plist for a charset (used by `set-charset-plist` builtin).
pub(crate) fn set_charset_plist_registry(name: SymId, plist: Vec<(SymId, Value)>) {
    CHARSET_REGISTRY.with(|slot| slot.borrow_mut().set_plist(name, plist));
}

pub(crate) fn charset_target_ranges(name: &str) -> Option<Vec<(u32, u32)>> {
    CHARSET_REGISTRY.with(|slot| {
        let reg = slot.borrow();
        let name = lookup_interned(name)?;
        let info = reg.charsets.get(&name)?;
        match info.method {
            CharsetMethod::Offset(offset) => {
                let from = info.min_code.checked_add(offset)?;
                let to = info.max_code.checked_add(offset)?;
                let from = u32::try_from(from).ok()?;
                let to = u32::try_from(to).ok()?;
                Some(vec![(from.min(to), from.max(to))])
            }
            CharsetMethod::Map(ref map_name) => {
                let values = load_charset_map(map_name)?
                    .code_to_char
                    .values()
                    .filter_map(|ch| u32::try_from(*ch).ok())
                    .collect();
                coalesce_u32_ranges(values)
            }
            CharsetMethod::Subset(ref subset) => {
                let values = (subset.parent_min_code..=subset.parent_max_code)
                    .filter_map(|parent_code| reg.decode_char(subset.parent, parent_code))
                    .filter_map(|ch| u32::try_from(ch).ok())
                    .collect();
                coalesce_u32_ranges(values)
            }
            CharsetMethod::Superset(_) => {
                let mut values = Vec::new();
                for (parent_name, _) in CharsetRegistry::superset_members(info) {
                    let ranges = charset_target_ranges(resolve_sym(parent_name))?;
                    for (from, to) in ranges {
                        values.extend(from..=to);
                    }
                }
                coalesce_u32_ranges(values)
            }
        }
    })
}

pub(crate) fn charset_exists(name: &str) -> bool {
    CHARSET_REGISTRY.with(|slot| slot.borrow().contains(name))
}

pub(crate) fn charset_contains_char(name: &str, ch: u32) -> Option<bool> {
    CHARSET_REGISTRY.with(|slot| {
        let reg = slot.borrow();
        let name = lookup_interned(name)?;
        reg.charsets.get(&name)?;
        Some(reg.encode_char(name, i64::from(ch)).is_some())
    })
}

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

fn expect_int_or_marker(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("integer-or-marker-p"), *value],
        )),
    }
}

fn require_known_charset(value: &Value) -> Result<SymId, Flow> {
    let name = match value.kind() {
        ValueKind::Symbol(id) => id,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("charsetp"), *value],
            ));
        }
    };
    let known = CHARSET_REGISTRY.with(|slot| slot.borrow().contains_symbol(name));
    if known {
        Ok(name)
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("charsetp"), Value::from_sym_id(name)],
        ))
    }
}

fn decode_char_codepoint_arg(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) if n >= 0 => Ok(n),
        ValueKind::Float => {
            let f = value.as_float().unwrap();
            if f.is_finite() && f >= 0.0 && f.fract() == 0.0 && f <= i64::MAX as f64 {
                Ok(f as i64)
            } else {
                Err(signal(
                    "error",
                    vec![Value::string(
                        "Not an in-range integer, integral float, or cons of integers",
                    )],
                ))
            }
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
    match value.kind() {
        ValueKind::Fixnum(n) if n >= 0 => Ok(n),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("wholenump"), *value],
        )),
    }
}

fn expect_fixnump(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) => Ok(n),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("fixnump"), *value],
        )),
    }
}

fn encode_char_input(value: &Value) -> Result<i64, Flow> {
    match value.kind() {
        ValueKind::Fixnum(c) => Ok(c as i64),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *value],
        )),
    }
}

fn charset_value_text(value: &Value) -> Option<String> {
    match value.kind() {
        ValueKind::String => value.as_runtime_string_owned(),
        ValueKind::Symbol(id) => Some(resolve_sym(id).to_string()),
        _ => None,
    }
}

fn parse_map_name(value: &Value) -> Option<String> {
    charset_value_text(value)
}

fn parse_subset_spec(value: &Value) -> Option<CharsetSubsetSpec> {
    let items = list_to_vec(value)?;
    if items.len() != 4 {
        return None;
    }
    Some(CharsetSubsetSpec {
        parent: items[0].as_symbol_id()?,
        parent_min_code: decode_code_arg(&items[1]),
        parent_max_code: decode_code_arg(&items[2]),
        offset: int_or_zero(&items[3]),
    })
}

fn parse_superset_spec(value: &Value) -> Option<Vec<(SymId, i64)>> {
    let items = list_to_vec(value)?;
    let members = items
        .into_iter()
        .map(|item| match item.kind() {
            ValueKind::Symbol(id) => Some((id, 0)),
            ValueKind::Cons => {
                let car = item.cons_car();
                let cdr = item.cons_cdr();
                let name = car.as_symbol_id()?;
                Some((name, int_or_zero(&cdr)))
            }
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;

    Some(members)
}

// ---------------------------------------------------------------------------
// Pure builtins (Vec<Value> -> EvalResult)
// ---------------------------------------------------------------------------

/// `(charsetp OBJECT)` -- return t if OBJECT names a known charset.
pub(crate) fn builtin_charsetp(args: Vec<Value>) -> EvalResult {
    expect_args("charsetp", &args, 1)?;
    let name = match args[0].kind() {
        ValueKind::Symbol(id) => id,
        _ => return Ok(Value::NIL),
    };
    let found = CHARSET_REGISTRY.with(|slot| slot.borrow().contains_symbol(name));
    Ok(Value::bool_val(found))
}

/// `(charset-list)` -- return charset symbols in priority order.
#[cfg(test)]
pub(crate) fn builtin_charset_list(args: Vec<Value>) -> EvalResult {
    expect_args("charset-list", &args, 0)?;
    let names: Vec<Value> = CHARSET_REGISTRY.with(|slot| {
        slot.borrow()
            .priority_list()
            .iter()
            .map(|name| Value::from_sym_id(*name))
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
                Ok(Value::list(vec![Value::from_sym_id(*first)]))
            } else {
                Ok(Value::NIL)
            }
        } else {
            let syms: Vec<Value> = priority.iter().map(|s| Value::from_sym_id(*s)).collect();
            Ok(Value::list(syms))
        }
    })
}

/// `(set-charset-priority &rest CHARSETS)` -- set charset detection priority.
pub(crate) fn builtin_set_charset_priority(args: Vec<Value>) -> EvalResult {
    expect_min_args("set-charset-priority", &args, 1)?;

    let mut requested = Vec::with_capacity(args.len());
    for arg in &args {
        let name = match arg.kind() {
            ValueKind::Symbol(id) => id,
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("charsetp"), *arg],
                ));
            }
        };
        let known = CHARSET_REGISTRY.with(|slot| slot.borrow().contains_symbol(name));
        if !known {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("charsetp"), *arg],
            ));
        }
        requested.push(name);
    }
    CHARSET_REGISTRY.with(|slot| slot.borrow_mut().set_priority(&requested));
    Ok(Value::NIL)
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
        if let Some(pairs) = reg.plist(name) {
            let mut elems = Vec::with_capacity(pairs.len() * 2);
            for (key, val) in pairs {
                elems.push(Value::from_sym_id(*key));
                elems.push(*val);
            }
            Ok(Value::list(elems))
        } else {
            Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("charsetp"), Value::from_sym_id(name)],
            ))
        }
    })
}

/// `(charset-id-internal &optional CHARSET)` -- return internal charset id.
pub(crate) fn builtin_charset_id_internal(args: Vec<Value>) -> EvalResult {
    expect_max_args("charset-id-internal", &args, 1)?;
    let arg = args.first().cloned().unwrap_or(Value::NIL);
    let name = match arg.kind() {
        ValueKind::Symbol(id) => id,
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
            Ok(Value::fixnum(id))
        } else {
            Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("charsetp"), Value::from_sym_id(name)],
            ))
        }
    })
}

/// Extract an integer from a Value, or return 0 for nil.
fn int_or_zero(val: &Value) -> i64 {
    match val.kind() {
        ValueKind::Fixnum(n) => n,
        _ => 0,
    }
}

/// Extract an optional integer from a Value (nil → None).
fn opt_int(val: &Value) -> Option<i64> {
    match val.kind() {
        ValueKind::Fixnum(n) => Some(n),
        ValueKind::Nil => None,
        _ => None,
    }
}

/// Decode a code point argument that may be a plain int or a cons (HI . LO).
fn decode_code_arg(val: &Value) -> i64 {
    match val.kind() {
        ValueKind::Fixnum(n) => n,
        ValueKind::Cons => {
            let pair_car = val.cons_car();
            let pair_cdr = val.cons_cdr();
            let hi = int_or_zero(&pair_car);
            let lo = int_or_zero(&pair_cdr);
            (hi << 16) | lo
        }
        _ => 0,
    }
}

/// Parse a plist Value into a Vec of (key, value) pairs.
fn parse_plist(val: &Value) -> Vec<(SymId, Value)> {
    let mut result = Vec::new();
    let Some(items) = list_to_vec(val) else {
        return result;
    };
    let mut i = 0;
    while i + 1 < items.len() {
        if let Some(key) = items[i].as_symbol_id() {
            result.push((key, items[i + 1]));
        }
        i += 2;
    }
    result
}

fn coalesce_u32_ranges(mut values: Vec<u32>) -> Option<Vec<(u32, u32)>> {
    if values.is_empty() {
        return None;
    }

    values.sort_unstable();
    values.dedup();

    let mut ranges = Vec::new();
    let mut start = values[0];
    let mut end = values[0];

    for value in values.into_iter().skip(1) {
        if value == end.saturating_add(1) {
            end = value;
        } else {
            ranges.push((start, end));
            start = value;
            end = value;
        }
    }

    ranges.push((start, end));
    Some(ranges)
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
    let name = match args[0].kind() {
        ValueKind::Symbol(id) => id,
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), args[0]],
            ));
        }
    };

    // arg[1]: dimension (vector or integer — the define-charset macro passes
    //         a vector of the form [dim ...], but we also accept a plain int)
    let dimension = match args[1].kind() {
        ValueKind::Fixnum(n) => n,
        ValueKind::Veclike(VecLikeType::Vector) => {
            let vec = args[1].as_vector_data().unwrap().clone();
            if vec.is_empty() {
                return Err(signal("args-out-of-range", vec![args[1], Value::fixnum(0)]));
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
    let code_space = match args[2].kind() {
        ValueKind::Veclike(VecLikeType::Vector) => {
            let vec = args[2].as_vector_data().unwrap().clone();
            if vec.len() < 2 {
                return Err(signal(
                    "args-out-of-range",
                    vec![args[2], Value::fixnum(vec.len() as i64)],
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
        CharsetMethod::Map(parse_map_name(&args[12]).unwrap_or_default())
    } else if !args[13].is_nil() {
        CharsetMethod::Subset(parse_subset_spec(&args[13]).ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), args[13]],
            )
        })?)
    } else if !args[14].is_nil() {
        CharsetMethod::Superset(parse_superset_spec(&args[14]).ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("listp"), args[14]],
            )
        })?)
    } else {
        // Default to offset 0 if nothing specified
        CharsetMethod::Offset(0)
    };

    // arg[15]: unify-map
    // arg[16]: plist
    let unify_map = args[15];
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
            name,
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
            unify_map,
            method,
            plist,
        };
        reg.register(info);
    });

    Ok(Value::NIL)
}

/// Context-aware variant of `(find-charset-region BEG END &optional TABLE)`.
///
/// Returns charset symbols present in the region `[BEG, END)` where BEG/END are
/// Emacs 1-based character positions inside the accessible region.
pub(crate) fn builtin_find_charset_region(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_min_args("find-charset-region", &args, 2)?;
    expect_max_args("find-charset-region", &args, 3)?;
    let beg = expect_int_or_marker(&args[0])?;
    let end = expect_int_or_marker(&args[1])?;

    let buf = ctx
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    let point_min = buf.point_min_char() as i64 + 1;
    let point_max = buf.point_max_char() as i64 + 1;
    if beg < point_min || beg > point_max || end < point_min || end > point_max {
        return Err(signal(
            "args-out-of-range",
            vec![Value::fixnum(beg), Value::fixnum(end)],
        ));
    }

    let mut a = beg;
    let mut b = end;
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }

    let start_byte = buf.lisp_pos_to_accessible_byte(a);
    let end_byte = buf.lisp_pos_to_accessible_byte(b);
    if start_byte == end_byte {
        return Ok(Value::list(vec![Value::symbol("ascii")]));
    }

    let text = {
        let string = buf.buffer_substring_lisp_string(start_byte, end_byte);
        super::builtins::runtime_string_from_lisp_string(&string)
    };
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
    Ok(Value::fixnum(ch))
}

/// `(decode-big5-char CODE)` -- decode BIG5 code to Emacs character code.
pub(crate) fn builtin_decode_big5_char(args: Vec<Value>) -> EvalResult {
    expect_args("decode-big5-char", &args, 1)?;
    let code = expect_wholenump(&args[0])?;
    Ok(Value::fixnum(code))
}

/// `(encode-sjis-char CH)` -- encode character CH in Shift-JIS space.
pub(crate) fn builtin_encode_sjis_char(args: Vec<Value>) -> EvalResult {
    expect_args("encode-sjis-char", &args, 1)?;
    let ch = encode_char_input(&args[0])?;
    Ok(Value::fixnum(ch))
}

/// `(decode-sjis-char CODE)` -- decode Shift-JIS code to Emacs character code.
pub(crate) fn builtin_decode_sjis_char(args: Vec<Value>) -> EvalResult {
    expect_args("decode-sjis-char", &args, 1)?;
    let code = expect_wholenump(&args[0])?;
    Ok(Value::fixnum(code))
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
    Ok(Value::fixnum(final_char))
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
    Ok(Value::NIL)
}

/// `(define-charset-alias ALIAS CHARSET)` -- add ALIAS for CHARSET.
pub(crate) fn builtin_define_charset_alias(args: Vec<Value>) -> EvalResult {
    expect_args("define-charset-alias", &args, 2)?;
    let target = require_known_charset(&args[1])?;
    if let Some(id) = args[0].as_symbol_id() {
        CHARSET_REGISTRY.with(|slot| slot.borrow_mut().define_alias(id, target));
    }
    Ok(Value::NIL)
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
    let text = args[0].as_runtime_string_owned().ok_or_else(|| {
        signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), args[0]],
        )
    })?;

    let charsets = classify_string_charsets(&text);
    if charsets.is_empty() {
        Ok(Value::NIL)
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

    let decoded = CHARSET_REGISTRY.with(|slot| slot.borrow().decode_char(name, code_point));

    Ok(decoded.map_or(Value::NIL, Value::fixnum))
}

/// `(encode-char CH CHARSET)` -- encode CH in CHARSET space.
///
/// Uses the charset's registered method to convert an Emacs internal
/// character code back to a charset-specific code-point.
pub(crate) fn builtin_encode_char(args: Vec<Value>) -> EvalResult {
    expect_args("encode-char", &args, 2)?;
    let ch = encode_char_input(&args[0])?;
    let name = require_known_charset(&args[1])?;

    let encoded = CHARSET_REGISTRY.with(|slot| slot.borrow().encode_char(name, ch));

    Ok(encoded.map_or(Value::NIL, Value::fixnum))
}

/// `(clear-charset-maps)` -- clear charset-related caches and return nil.
pub(crate) fn builtin_clear_charset_maps(args: Vec<Value>) -> EvalResult {
    expect_max_args("clear-charset-maps", &args, 0)?;
    if let Ok(mut cache) = charset_map_cache().write() {
        cache.clear();
    }
    Ok(Value::NIL)
}

/// Context-aware variant of `(charset-after &optional POS)`.
///
/// Returns the charset of the character at POS (1-based), or the character
/// after point when POS is omitted. Returns nil at end-of-buffer or for
/// out-of-range numeric positions.
pub(crate) fn builtin_charset_after(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_max_args("charset-after", &args, 1)?;
    let buf = ctx
        .buffers
        .current_buffer()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;

    let target_byte = if let Some(pos) = args.first() {
        let pos = expect_int_or_marker(pos)?;
        let point_min = buf.point_min_char() as i64 + 1;
        let point_max = buf.point_max_char() as i64 + 1;
        if pos < point_min || pos > point_max {
            return Ok(Value::NIL);
        }
        buf.lisp_pos_to_accessible_byte(pos)
    } else {
        buf.point()
    };

    let point_max_byte = buf.point_max();
    if target_byte >= point_max_byte {
        return Ok(Value::NIL);
    }

    let Some(ch) = buf.char_after(target_byte) else {
        return Ok(Value::NIL);
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
