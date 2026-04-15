use super::charset::{charset_contains_char, charset_exists, charset_target_ranges};
use super::chartable::{for_each_non_nil_char_table_run, is_char_table};
use super::error::{Flow, signal};
use super::intern::resolve_sym;
use super::value::*;
use crate::face::{FontSlant, FontWeight, FontWidth};
use regex::Regex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};
use std::thread::LocalKey;

pub const DEFAULT_FONTSET_NAME: &str = "-*-*-*-*-*-*-*-*-*-*-*-*-fontset-default";
pub const DEFAULT_FONTSET_ALIAS: &str = "fontset-default";

fn fontset_string_text(value: &Value) -> Option<String> {
    value.as_runtime_string_owned()
}

thread_local! {
    static FONTSET_WILDCARD_REGEX_CACHE: RefCell<HashMap<String, Regex>> = RefCell::new(HashMap::new());
    static FONTSET_REGEXP_CACHE: RefCell<HashMap<String, Regex>> = RefCell::new(HashMap::new());
    static FONT_ENCODING_REGEX_CACHE: RefCell<HashMap<String, Regex>> = RefCell::new(HashMap::new());
}

fn clear_regex_cache(cache: &'static LocalKey<RefCell<HashMap<String, Regex>>>) {
    cache.with(|cache| cache.borrow_mut().clear());
}

fn cached_regex(
    cache: &'static LocalKey<RefCell<HashMap<String, Regex>>>,
    key: &str,
    build: impl FnOnce() -> Option<Regex>,
) -> Option<Regex> {
    if let Some(cached) = cache.with(|cache| cache.borrow().get(key).cloned()) {
        return Some(cached);
    }

    let compiled = build()?;
    cache.with(|cache| {
        cache.borrow_mut().insert(key.to_string(), compiled.clone());
    });
    Some(compiled)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredFontSpec {
    pub family: Option<String>,
    pub registry: Option<String>,
    pub lang: Option<String>,
    pub weight: Option<FontWeight>,
    pub slant: Option<FontSlant>,
    pub width: Option<FontWidth>,
    pub repertory: Option<FontRepertory>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FontSpecEntry {
    Font(StoredFontSpec),
    ExplicitNone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FontRepertory {
    Charset(String),
    CharTableRanges(Vec<(u32, u32)>),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum FontsetTarget {
    Range(u32, u32),
    Fallback,
}

#[derive(Clone, Debug)]
struct RangeEntry {
    from: u32,
    to: u32,
    entries: Vec<FontSpecEntry>,
}

#[derive(Clone, Debug, Default)]
struct FontsetData {
    ranges: Vec<RangeEntry>,
    fallback: Option<Vec<FontSpecEntry>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FontsetRangeEntrySnapshot {
    pub from: u32,
    pub to: u32,
    pub entries: Vec<FontSpecEntry>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FontsetDataSnapshot {
    pub ranges: Vec<FontsetRangeEntrySnapshot>,
    pub fallback: Option<Vec<FontSpecEntry>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FontsetRegistrySnapshot {
    pub ordered_names: Vec<String>,
    pub alias_to_name: Vec<(String, String)>,
    pub fontsets: Vec<(String, FontsetDataSnapshot)>,
    pub generation: u64,
}

#[derive(Clone, Debug)]
struct FontsetRegistry {
    ordered_names: Vec<String>,
    alias_to_name: HashMap<String, String>,
    fontsets: HashMap<String, FontsetData>,
    generation: u64,
}

impl FontsetRegistry {
    fn with_defaults() -> Self {
        let mut alias_to_name = HashMap::new();
        alias_to_name.insert(
            DEFAULT_FONTSET_ALIAS.to_string(),
            DEFAULT_FONTSET_NAME.to_string(),
        );
        let mut fontsets = HashMap::new();
        fontsets.insert(DEFAULT_FONTSET_NAME.to_string(), FontsetData::default());
        Self {
            ordered_names: vec![DEFAULT_FONTSET_NAME.to_string()],
            alias_to_name,
            fontsets,
            generation: 1,
        }
    }

    fn resolve_literal(&self, name: &str) -> Option<String> {
        if self.ordered_names.iter().any(|candidate| candidate == name) {
            Some(name.to_string())
        } else {
            self.alias_to_name.get(name).cloned()
        }
    }

    fn ensure_fontset(&mut self, name: &str) {
        self.fontsets.entry(name.to_string()).or_default();
        if !self.ordered_names.iter().any(|candidate| candidate == name) {
            self.ordered_names.push(name.to_string());
        }
    }

    fn register_fontset(&mut self, name: String, alias: Option<String>) -> String {
        self.ensure_fontset(&name);
        if let Some(alias_name) = alias {
            self.alias_to_name.insert(alias_name, name.clone());
        }
        name
    }

    fn replace_rules(&mut self, name: &str, rules: Vec<(FontsetTarget, Vec<FontSpecEntry>)>) {
        self.ensure_fontset(name);
        let mut data = FontsetData::default();
        for (target, entries) in rules {
            for entry in entries {
                data.update_target(target.clone(), entry, FontsetAddMode::Append);
            }
        }
        self.fontsets.insert(name.to_string(), data);
        self.generation = self.generation.wrapping_add(1);
    }

    fn update_target(
        &mut self,
        name: &str,
        target: FontsetTarget,
        entry: FontSpecEntry,
        add: FontsetAddMode,
    ) {
        self.ensure_fontset(name);
        let data = self.fontsets.entry(name.to_string()).or_default();
        data.update_target(target, entry, add);
        self.generation = self.generation.wrapping_add(1);
    }

    fn list_value(&self) -> Value {
        Value::list(
            self.ordered_names
                .iter()
                .cloned()
                .map(Value::string)
                .collect(),
        )
    }

    fn alias_alist_value(&self) -> Value {
        let mut entries = Vec::new();
        for name in &self.ordered_names {
            for (alias, canonical) in &self.alias_to_name {
                if canonical == name {
                    entries.push(Value::cons(
                        Value::string(name.clone()),
                        Value::string(alias),
                    ));
                }
            }
        }
        Value::list(entries)
    }

    fn matching_entries_for_char(&self, name: &str, ch: char) -> Vec<FontSpecEntry> {
        let code = ch as u32;
        let Some(data) = self.fontsets.get(name) else {
            return Vec::new();
        };

        let mut entries = data.matching_entries_for_char(code);
        if entries.is_empty() && name != DEFAULT_FONTSET_NAME {
            if let Some(default) = self.fontsets.get(DEFAULT_FONTSET_NAME) {
                entries = default.matching_entries_for_char(code);
            }
        }
        entries
    }
}

impl FontsetData {
    fn matching_entries_for_char(&self, code: u32) -> Vec<FontSpecEntry> {
        let mut entries = filter_entries_for_char(self.specific_entries_for_char(code), code);
        if let Some(fallback) = &self.fallback {
            entries.extend(filter_entries_for_char(fallback.clone(), code));
        }
        entries
    }

    fn update_target(&mut self, target: FontsetTarget, entry: FontSpecEntry, add: FontsetAddMode) {
        match target {
            FontsetTarget::Fallback => self.update_fallback(entry, add),
            FontsetTarget::Range(from, to) => self.update_range(from, to, entry, add),
        }
    }

    fn specific_entries_for_char(&self, code: u32) -> Vec<FontSpecEntry> {
        self.find_range(code)
            .map(|range| range.entries.clone())
            .unwrap_or_default()
    }

    fn find_range(&self, code: u32) -> Option<&RangeEntry> {
        let mut low = 0usize;
        let mut high = self.ranges.len();
        while low < high {
            let mid = low + (high - low) / 2;
            let range = &self.ranges[mid];
            if code < range.from {
                high = mid;
            } else if code > range.to {
                low = mid + 1;
            } else {
                return Some(range);
            }
        }
        None
    }

    fn update_fallback(&mut self, entry: FontSpecEntry, add: FontsetAddMode) {
        self.fallback = Some(apply_fontset_add(self.fallback.as_deref(), entry, add));
    }

    fn update_range(&mut self, from: u32, to: u32, entry: FontSpecEntry, add: FontsetAddMode) {
        let mut next = Vec::with_capacity(self.ranges.len() + 2);
        let mut cursor = from;

        for range in &self.ranges {
            if range.to < from {
                push_range_entry(&mut next, range.clone());
                continue;
            }
            if range.from > to {
                if cursor <= to {
                    push_range_entry(
                        &mut next,
                        RangeEntry {
                            from: cursor,
                            to,
                            entries: apply_fontset_add(None, entry.clone(), add),
                        },
                    );
                    cursor = to.saturating_add(1);
                }
                push_range_entry(&mut next, range.clone());
                continue;
            }

            if range.from < from {
                push_range_entry(
                    &mut next,
                    RangeEntry {
                        from: range.from,
                        to: from - 1,
                        entries: range.entries.clone(),
                    },
                );
            }

            if cursor < range.from {
                push_range_entry(
                    &mut next,
                    RangeEntry {
                        from: cursor,
                        to: range.from - 1,
                        entries: apply_fontset_add(None, entry.clone(), add),
                    },
                );
            }

            let overlap_from = range.from.max(from);
            let overlap_to = range.to.min(to);
            push_range_entry(
                &mut next,
                RangeEntry {
                    from: overlap_from,
                    to: overlap_to,
                    entries: apply_fontset_add(Some(&range.entries), entry.clone(), add),
                },
            );
            cursor = overlap_to.saturating_add(1);

            if range.to > to {
                push_range_entry(
                    &mut next,
                    RangeEntry {
                        from: to + 1,
                        to: range.to,
                        entries: range.entries.clone(),
                    },
                );
            }
        }

        if cursor <= to {
            push_range_entry(
                &mut next,
                RangeEntry {
                    from: cursor,
                    to,
                    entries: apply_fontset_add(None, entry, add),
                },
            );
        }

        self.ranges = next;
    }
}

impl StoredFontSpec {
    fn matches_char(&self, code: u32) -> bool {
        self.repertory
            .as_ref()
            .is_none_or(|repertory| repertory.matches_char(code))
    }
}

impl FontRepertory {
    fn matches_char(&self, code: u32) -> bool {
        match self {
            // GNU filters by charset repertory here. When Neomacs' charset
            // engine cannot yet answer membership for map/subset/superset
            // charsets, keep the candidate instead of producing a false
            // negative and dropping a valid font.
            Self::Charset(name) => charset_contains_char(name, code).unwrap_or(true),
            Self::CharTableRanges(ranges) => {
                ranges.iter().any(|(from, to)| code >= *from && code <= *to)
            }
        }
    }
}

pub fn repertory_target_ranges(repertory: &FontRepertory) -> Option<Vec<(u32, u32)>> {
    match repertory {
        FontRepertory::Charset(name) => charset_target_ranges(name),
        FontRepertory::CharTableRanges(ranges) => Some(ranges.clone()),
    }
}

fn filter_entries_for_char(entries: Vec<FontSpecEntry>, code: u32) -> Vec<FontSpecEntry> {
    entries
        .into_iter()
        .filter(|entry| match entry {
            FontSpecEntry::ExplicitNone => true,
            FontSpecEntry::Font(spec) => spec.matches_char(code),
        })
        .collect()
}

fn apply_fontset_add(
    existing: Option<&[FontSpecEntry]>,
    entry: FontSpecEntry,
    add: FontsetAddMode,
) -> Vec<FontSpecEntry> {
    match add {
        FontsetAddMode::Overwrite => vec![entry],
        FontsetAddMode::Append => {
            let mut entries = existing.map(ToOwned::to_owned).unwrap_or_default();
            entries.push(entry);
            entries
        }
        FontsetAddMode::Prepend => {
            let mut entries = vec![entry];
            if let Some(existing) = existing {
                entries.extend_from_slice(existing);
            }
            entries
        }
    }
}

fn push_range_entry(ranges: &mut Vec<RangeEntry>, entry: RangeEntry) {
    if entry.from > entry.to {
        return;
    }
    if let Some(last) = ranges.last_mut()
        && last.entries == entry.entries
        && last.to.checked_add(1) == Some(entry.from)
    {
        last.to = entry.to;
        return;
    }
    ranges.push(entry);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FontsetAddMode {
    Overwrite,
    Append,
    Prepend,
}

static FONTSET_REGISTRY: OnceLock<RwLock<FontsetRegistry>> = OnceLock::new();

fn registry() -> &'static RwLock<FontsetRegistry> {
    FONTSET_REGISTRY.get_or_init(|| RwLock::new(FontsetRegistry::with_defaults()))
}

fn clear_fontset_regex_caches() {
    clear_regex_cache(&FONTSET_WILDCARD_REGEX_CACHE);
    clear_regex_cache(&FONTSET_REGEXP_CACHE);
    clear_regex_cache(&FONT_ENCODING_REGEX_CACHE);
}

pub(crate) fn reset_fontset_registry() {
    if let Ok(mut slot) = registry().write() {
        *slot = FontsetRegistry::with_defaults();
    }
    clear_fontset_regex_caches();
}

pub(crate) fn snapshot_fontset_registry() -> FontsetRegistrySnapshot {
    registry()
        .read()
        .map(|slot| {
            let mut alias_to_name: Vec<_> = slot
                .alias_to_name
                .iter()
                .map(|(alias, name)| (alias.clone(), name.clone()))
                .collect();
            alias_to_name.sort();

            let mut fontsets: Vec<_> = slot
                .fontsets
                .iter()
                .map(|(name, data)| {
                    (
                        name.clone(),
                        FontsetDataSnapshot {
                            ranges: data
                                .ranges
                                .iter()
                                .map(|range| FontsetRangeEntrySnapshot {
                                    from: range.from,
                                    to: range.to,
                                    entries: range.entries.clone(),
                                })
                                .collect(),
                            fallback: data.fallback.clone(),
                        },
                    )
                })
                .collect();
            fontsets.sort_by(|left, right| left.0.cmp(&right.0));

            FontsetRegistrySnapshot {
                ordered_names: slot.ordered_names.clone(),
                alias_to_name,
                fontsets,
                generation: slot.generation,
            }
        })
        .unwrap_or_else(|_| FontsetRegistrySnapshot {
            ordered_names: vec![DEFAULT_FONTSET_NAME.to_string()],
            alias_to_name: vec![(
                DEFAULT_FONTSET_ALIAS.to_string(),
                DEFAULT_FONTSET_NAME.to_string(),
            )],
            fontsets: vec![(
                DEFAULT_FONTSET_NAME.to_string(),
                FontsetDataSnapshot::default(),
            )],
            generation: 1,
        })
}

pub(crate) fn restore_fontset_registry(snapshot: FontsetRegistrySnapshot) {
    let alias_to_name = snapshot.alias_to_name.into_iter().collect();
    let fontsets = snapshot
        .fontsets
        .into_iter()
        .map(|(name, data)| {
            (
                name,
                FontsetData {
                    ranges: data
                        .ranges
                        .into_iter()
                        .map(|range| RangeEntry {
                            from: range.from,
                            to: range.to,
                            entries: range.entries,
                        })
                        .collect(),
                    fallback: data.fallback,
                },
            )
        })
        .collect();
    let restored = FontsetRegistry {
        ordered_names: snapshot.ordered_names,
        alias_to_name,
        fontsets,
        generation: snapshot.generation.max(1),
    };
    if let Ok(mut slot) = registry().write() {
        *slot = restored;
    }
    clear_fontset_regex_caches();
}

pub fn fontset_generation() -> u64 {
    registry().read().map(|slot| slot.generation).unwrap_or(0)
}

pub(crate) fn fontset_alias_alist_startup_value() -> Value {
    registry()
        .read()
        .map(|slot| slot.alias_alist_value())
        .unwrap_or(Value::NIL)
}

pub(crate) fn fontset_list_value() -> Value {
    registry()
        .read()
        .map(|slot| slot.list_value())
        .unwrap_or(Value::NIL)
}

pub(crate) fn normalize_fontset_name(name: &str) -> String {
    name.to_ascii_lowercase()
}

pub(crate) fn fontset_registry_alias_from_xlfd(name: &str) -> Option<String> {
    let parts: Vec<&str> = name.split('-').collect();
    if parts.len() < 15 || parts.first().copied() != Some("") {
        return None;
    }
    let registry = parts.get(parts.len() - 2)?;
    let encoding = parts.last()?;
    let alias = format!(
        "{}-{}",
        registry.to_ascii_lowercase(),
        encoding.to_ascii_lowercase()
    );
    if alias.starts_with("fontset-") && alias.len() >= 9 {
        Some(alias)
    } else {
        None
    }
}

fn wildcard_fontset_pattern_to_regex(pattern: &str) -> Option<Regex> {
    cached_regex(&FONTSET_WILDCARD_REGEX_CACHE, pattern, || {
        let escaped = regex::escape(pattern);
        let wildcard = escaped.replace(r"\*", ".*").replace(r"\?", ".");
        Regex::new(&format!("^{wildcard}$")).ok()
    })
}

pub(crate) fn query_fontset_registry(pattern: &str, regexpp: bool) -> Option<String> {
    let pattern = normalize_fontset_name(pattern);
    registry().read().ok().and_then(|registry| {
        if regexpp {
            let regex = cached_regex(&FONTSET_REGEXP_CACHE, &pattern, || {
                Regex::new(&pattern).ok()
            })?;
            for name in &registry.ordered_names {
                if regex.is_match(name) {
                    return Some(name.clone());
                }
            }
            for (alias, name) in &registry.alias_to_name {
                if regex.is_match(alias) {
                    return Some(name.clone());
                }
            }
            return None;
        }

        if !pattern.contains('*') && !pattern.contains('?') {
            return registry.resolve_literal(&pattern);
        }

        let regex = wildcard_fontset_pattern_to_regex(&pattern)?;
        for name in &registry.ordered_names {
            if regex.is_match(name) {
                return Some(name.clone());
            }
        }
        for (alias, name) in &registry.alias_to_name {
            if regex.is_match(alias) {
                return Some(name.clone());
            }
        }
        None
    })
}

pub(crate) fn resolve_fontset_name_arg(value: &Value) -> Result<String, Flow> {
    match value.kind() {
        ValueKind::Nil | ValueKind::T => Ok(DEFAULT_FONTSET_NAME.to_string()),
        ValueKind::String => {
            let requested =
                normalize_fontset_name(&fontset_string_text(value).expect("checked string"));
            Ok(query_fontset_registry(&requested, false).unwrap_or(requested))
        }
        ValueKind::Symbol(id) => {
            let requested = normalize_fontset_name(resolve_sym(id));
            Ok(query_fontset_registry(&requested, false).unwrap_or(requested))
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("stringp"), *value],
        )),
    }
}

pub fn matching_entries_for_char(ch: char) -> Vec<FontSpecEntry> {
    matching_entries_for_fontset(DEFAULT_FONTSET_NAME, ch)
}

pub fn matching_entries_for_fontset(name: &str, ch: char) -> Vec<FontSpecEntry> {
    registry()
        .read()
        .map(|slot| slot.matching_entries_for_char(name, ch))
        .unwrap_or_default()
}

pub(crate) fn fontset_font(name: &Value, ch: char, all: bool) -> Result<Value, Flow> {
    let fontset_name = resolve_fontset_name_arg(name)?;
    let entries = matching_entries_for_fontset(&fontset_name, ch);

    let mut patterns = Vec::new();
    for entry in entries {
        match entry {
            FontSpecEntry::ExplicitNone => return Ok(Value::NIL),
            FontSpecEntry::Font(spec) => {
                let family = spec.family.map(Value::string).unwrap_or(Value::NIL);
                let registry = spec.registry.map(Value::string).unwrap_or(Value::NIL);
                let pattern = Value::cons(family, registry);
                if !all {
                    return Ok(pattern);
                }
                patterns.push(pattern);
            }
        }
    }

    if all {
        Ok(Value::list(patterns))
    } else {
        Ok(Value::NIL)
    }
}

pub(crate) fn new_fontset(
    name: &str,
    fontlist: &Value,
    char_script_table: Option<&Value>,
    charset_script_alist: Option<&Value>,
    font_encoding_alist: Option<&Value>,
) -> Result<String, Flow> {
    let requested_name = normalize_fontset_name(name);
    let canonical_name =
        query_fontset_registry(&requested_name, false).unwrap_or_else(|| requested_name.clone());
    let alias = if canonical_name != requested_name {
        None
    } else {
        Some(
            fontset_registry_alias_from_xlfd(&canonical_name).ok_or_else(|| {
                signal(
                    "error",
                    vec![Value::string("Fontset name must be in XLFD format")],
                )
            })?,
        )
    };

    let mut rules = Vec::new();
    for entry in list_to_vec(fontlist) {
        let parts = list_to_vec(&entry);
        if parts.is_empty() {
            continue;
        }
        let targets = expand_target(&parts[0], char_script_table, charset_script_alist, false)?;
        let mut entries = Vec::new();
        for spec in parts.iter().skip(1) {
            entries.push(parse_font_spec_entry(spec, font_encoding_alist)?);
        }
        for target in targets {
            rules.push((target, entries.clone()));
        }
    }

    let mut slot = registry().write().map_err(|_| {
        signal(
            "error",
            vec![Value::string("Fontset registry lock poisoned")],
        )
    })?;
    let registered = slot.register_fontset(canonical_name.clone(), alias);
    slot.replace_rules(&registered, rules);
    Ok(registered)
}

pub(crate) fn set_fontset_font(
    fontset: &Value,
    characters: &Value,
    font_spec: &Value,
    add: Option<&Value>,
    char_script_table: Option<&Value>,
    charset_script_alist: Option<&Value>,
    font_encoding_alist: Option<&Value>,
) -> Result<Value, Flow> {
    let fontset_name = resolve_fontset_name_arg(fontset)?;
    let add_mode = match add {
        Some(v) if v.is_symbol_named("append") => FontsetAddMode::Append,
        Some(v) if v.as_symbol_name().is_some_and(|n| n == ":append") => FontsetAddMode::Append,
        Some(v) if v.is_symbol_named("prepend") => FontsetAddMode::Prepend,
        Some(v) if v.as_symbol_name().is_some_and(|n| n == ":prepend") => FontsetAddMode::Prepend,
        _ => FontsetAddMode::Overwrite,
    };
    let entry = parse_font_spec_entry(font_spec, font_encoding_alist)?;
    let targets = expand_target(characters, char_script_table, charset_script_alist, true)?;

    let mut slot = registry().write().map_err(|_| {
        signal(
            "error",
            vec![Value::string("Fontset registry lock poisoned")],
        )
    })?;
    let canonical = slot.register_fontset(fontset_name, None);
    for target in targets {
        slot.update_target(&canonical, target, entry.clone(), add_mode);
    }
    Ok(Value::NIL)
}

fn parse_font_spec_entry(
    value: &Value,
    font_encoding_alist: Option<&Value>,
) -> Result<FontSpecEntry, Flow> {
    match value.kind() {
        ValueKind::Nil => Ok(FontSpecEntry::ExplicitNone),
        ValueKind::Cons => {
            let pair_car = value.cons_car();
            let pair_cdr = value.cons_cdr();
            let mut spec = StoredFontSpec {
                family: value_text(&pair_car),
                registry: value_text(&pair_cdr).map(|registry| registry.to_ascii_lowercase()),
                lang: None,
                weight: None,
                slant: None,
                width: None,
                repertory: None,
            };
            spec.repertory = resolve_font_repertory(&spec, font_encoding_alist);
            Ok(FontSpecEntry::Font(spec))
        }
        ValueKind::String => {
            let mut spec =
                parse_font_name_string(&fontset_string_text(value).expect("checked string"));
            spec.repertory = resolve_font_repertory(&spec, font_encoding_alist);
            Ok(FontSpecEntry::Font(spec))
        }
        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = value.as_vector_data().unwrap().clone();
            let mut spec = parse_font_vector(&items);
            spec.repertory = resolve_font_repertory(&spec, font_encoding_alist);
            Ok(FontSpecEntry::Font(spec))
        }
        _ => Err(signal(
            "font",
            vec![Value::string("Invalid font-spec"), *value],
        )),
    }
}

fn parse_font_vector(items: &[Value]) -> StoredFontSpec {
    let family = font_vector_get_flexible(items, "family")
        .and_then(|value| value_text(&value))
        .or_else(|| {
            font_vector_get_flexible(items, "name")
                .and_then(|value| value_text(&value))
                .map(|name| parse_font_name_string(&name))
                .and_then(|spec| spec.family)
        });
    let registry = font_vector_get_flexible(items, "registry")
        .and_then(|value| value_text(&value))
        .map(|registry| registry.to_ascii_lowercase())
        .or_else(|| {
            font_vector_get_flexible(items, "name")
                .and_then(|value| value_text(&value))
                .map(|name| parse_font_name_string(&name))
                .and_then(|spec| spec.registry)
        });
    let lang = font_vector_get_flexible(items, "lang")
        .and_then(|value| value_text(&value))
        .map(|lang| lang.to_ascii_lowercase());
    let weight = font_vector_get_flexible(items, "weight")
        .and_then(|value| value_text(&value))
        .and_then(|weight| FontWeight::from_symbol(&weight));
    let slant = font_vector_get_flexible(items, "slant")
        .and_then(|value| value_text(&value))
        .and_then(|slant| FontSlant::from_symbol(&slant));
    let width = font_vector_get_flexible(items, "width")
        .and_then(|value| value_text(&value))
        .and_then(|width| FontWidth::from_symbol(&width));

    StoredFontSpec {
        family,
        registry,
        lang,
        weight,
        slant,
        width,
        repertory: None,
    }
}

fn parse_font_name_string(name: &str) -> StoredFontSpec {
    let trimmed = name.trim();
    if trimmed.starts_with('-') {
        let parts: Vec<&str> = trimmed.split('-').collect();
        if parts.len() >= 15 {
            let family = parts
                .get(2)
                .copied()
                .filter(|value| !value.is_empty() && *value != "*");
            let registry = if parts.len() >= 3 {
                let registry = format!("{}-{}", parts[parts.len() - 2], parts[parts.len() - 1]);
                (!registry.contains('*')).then_some(registry.to_ascii_lowercase())
            } else {
                None
            };
            return StoredFontSpec {
                family: family.map(ToOwned::to_owned),
                registry,
                lang: None,
                weight: None,
                slant: None,
                width: None,
                repertory: None,
            };
        }
    }

    StoredFontSpec {
        family: (!trimmed.is_empty()).then_some(trimmed.to_string()),
        registry: None,
        lang: None,
        weight: None,
        slant: None,
        width: None,
        repertory: None,
    }
}

fn resolve_font_repertory(
    spec: &StoredFontSpec,
    font_encoding_alist: Option<&Value>,
) -> Option<FontRepertory> {
    let registry = spec.registry.as_deref()?;
    let font_name = match spec.family.as_deref() {
        Some(family) if !family.is_empty() => format!("{family}-{registry}"),
        _ => registry.to_string(),
    };

    lookup_font_encoding(font_encoding_alist?, &font_name)
        .and_then(|encoding| font_encoding_repertory(&encoding))
}

fn lookup_font_encoding(font_encoding_alist: &Value, font_name: &str) -> Option<Value> {
    for entry in list_to_vec(font_encoding_alist) {
        if !entry.is_cons() {
            continue;
        };
        let pair_car = entry.cons_car();
        let pair_cdr = entry.cons_cdr();
        let Some(pattern) = value_text(&pair_car) else {
            continue;
        };
        let translated = pattern
            .replace("\\|", "|")
            .replace("\\(", "(")
            .replace("\\)", ")");
        let Some(regex) = cached_regex(&FONT_ENCODING_REGEX_CACHE, &translated, || {
            regex::RegexBuilder::new(&translated)
                .case_insensitive(true)
                .build()
                .ok()
        }) else {
            continue;
        };
        if regex.is_match(font_name) {
            return Some(pair_cdr);
        }
    }
    None
}

fn font_encoding_repertory(value: &Value) -> Option<FontRepertory> {
    match value.kind() {
        ValueKind::Symbol(id) => {
            let name = resolve_sym(id);
            charset_exists(name).then(|| FontRepertory::Charset(name.to_string()))
        }
        ValueKind::Cons => {
            let pair_car = value.cons_car();
            let pair_cdr = value.cons_cdr();
            if pair_cdr.is_nil() {
                None
            } else {
                font_encoding_repertory(&pair_cdr)
            }
        }
        ValueKind::Veclike(VecLikeType::Vector) if is_char_table(value) => {
            let mut ranges = Vec::new();
            for_each_non_nil_char_table_run(value, |key, _| {
                if let Some((from, to)) = value_to_range(&key) {
                    ranges.push((from, to));
                }
            });
            (!ranges.is_empty()).then_some(FontRepertory::CharTableRanges(ranges))
        }
        _ => None,
    }
}

fn expand_target(
    target: &Value,
    char_script_table: Option<&Value>,
    charset_script_alist: Option<&Value>,
    enforce_ascii_rules: bool,
) -> Result<Vec<FontsetTarget>, Flow> {
    match target.kind() {
        ValueKind::Nil => Ok(vec![FontsetTarget::Fallback]),
        ValueKind::Fixnum(ch) => {
            let code = ch as u32;
            if enforce_ascii_rules && code < 0x80 {
                return Err(signal(
                    "error",
                    vec![Value::string("Can't set a font for partial ASCII range")],
                ));
            }
            Ok(vec![FontsetTarget::Range(code, code)])
        }
        ValueKind::Cons => {
            let pair_car = target.cons_car();
            let pair_cdr = target.cons_cdr();
            let from = expect_target_char(&pair_car)?;
            let to = expect_target_char(&pair_cdr)?;
            if from > to {
                return Ok(vec![FontsetTarget::Range(to, from)]);
            }
            if enforce_ascii_rules && from < 0x80 && !(from == 0 && to >= 0x7F) {
                return Err(signal(
                    "error",
                    vec![Value::string("Can't set a font for partial ASCII range")],
                ));
            }
            Ok(vec![FontsetTarget::Range(from, to)])
        }
        ValueKind::Symbol(id) => {
            let symbol_name = resolve_sym(id).to_string();
            let targets = expand_script_symbol(&symbol_name, char_script_table)
                .or_else(|| {
                    charset_target_ranges(&symbol_name).map(|ranges| {
                        ranges
                            .into_iter()
                            .map(|(from, to)| FontsetTarget::Range(from, to))
                            .collect()
                    })
                })
                .or_else(|| {
                    charset_script_alist
                        .and_then(|alist| lookup_charset_script(alist, &symbol_name))
                        .and_then(|script| expand_script_symbol(&script, char_script_table))
                })
                .unwrap_or_default();
            if targets.is_empty() {
                return Err(signal(
                    "error",
                    vec![Value::string(format!(
                        "Invalid script or charset name: {symbol_name}"
                    ))],
                ));
            }
            Ok(targets)
        }
        _ => Err(signal(
            "error",
            vec![Value::string(
                "Invalid second argument for setting a font in a fontset",
            )],
        )),
    }
}

fn expand_script_symbol(
    name: &str,
    char_script_table: Option<&Value>,
) -> Option<Vec<FontsetTarget>> {
    let table = char_script_table?;
    let target = Value::symbol(name);
    let mut ranges = Vec::new();
    for_each_non_nil_char_table_run(table, |key, value| {
        if value != target {
            return;
        }
        if let Some((from, to)) = value_to_range(&key) {
            ranges.push(FontsetTarget::Range(from, to));
        }
    });
    (!ranges.is_empty()).then_some(ranges)
}

fn lookup_charset_script(alist: &Value, charset_name: &str) -> Option<String> {
    let target = Value::symbol(charset_name);
    let mut cursor = *alist;
    loop {
        match cursor.kind() {
            ValueKind::Nil => return None,
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                if pair_car.is_cons() {
                    let entry_car = pair_car.cons_car();
                    let entry_cdr = pair_car.cons_cdr();
                    if entry_car == target {
                        return value_text(&entry_cdr);
                    }
                }
                cursor = pair_cdr;
            }
            _ => return None,
        }
    }
}

fn value_to_range(value: &Value) -> Option<(u32, u32)> {
    match value.kind() {
        ValueKind::Fixnum(ch) => Some((ch as u32, ch as u32)),
        ValueKind::Cons => {
            let pair_car = value.cons_car();
            let pair_cdr = value.cons_cdr();
            let from = expect_target_char(&pair_car).ok()?;
            let to = expect_target_char(&pair_cdr).ok()?;
            Some((from.min(to), from.max(to)))
        }
        _ => None,
    }
}

fn expect_target_char(value: &Value) -> Result<u32, Flow> {
    match value.kind() {
        ValueKind::Fixnum(ch) => Ok(ch as u32),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *value],
        )),
    }
}

fn list_to_vec(value: &Value) -> Vec<Value> {
    let mut cursor = *value;
    let mut items = Vec::new();
    loop {
        match cursor.kind() {
            ValueKind::Nil => return items,
            ValueKind::Cons => {
                let pair_car = cursor.cons_car();
                let pair_cdr = cursor.cons_cdr();
                items.push(pair_car);
                cursor = pair_cdr;
            }
            _ => {
                items.push(cursor);
                return items;
            }
        }
    }
}

fn value_text(value: &Value) -> Option<String> {
    match value.kind() {
        ValueKind::String => fontset_string_text(value),
        ValueKind::Symbol(id) => Some(resolve_sym(id).to_string()),
        _ => None,
    }
}

fn font_vector_get_flexible(items: &[Value], prop: &str) -> Option<Value> {
    let prop_norm = prop.trim_start_matches(':');
    let mut index = 1usize;
    while index + 1 < items.len() {
        let key_norm = match items[index].kind() {
            ValueKind::Symbol(id) => resolve_sym(id).trim_start_matches(':'),
            _ => {
                index += 2;
                continue;
            }
        };
        if key_norm == prop_norm {
            return Some(items[index + 1]);
        }
        index += 2;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emacs_core::charset::{builtin_define_charset_internal, reset_charset_registry};

    fn registry_spec(name: &str) -> FontSpecEntry {
        FontSpecEntry::Font(StoredFontSpec {
            family: None,
            registry: Some(name.to_string()),
            lang: None,
            weight: None,
            slant: None,
            width: None,
            repertory: None,
        })
    }

    #[test]
    fn overlapping_ranges_follow_char_table_semantics() {
        crate::test_utils::init_test_tracing();
        let mut data = FontsetData::default();
        data.update_target(
            FontsetTarget::Range(0x80, 0x10FFFF),
            registry_spec("iso8859-1"),
            FontsetAddMode::Overwrite,
        );
        data.update_target(
            FontsetTarget::Range(0x4E00, 0x9FFF),
            registry_spec("gb2312.1980-0"),
            FontsetAddMode::Overwrite,
        );

        assert_eq!(
            data.specific_entries_for_char('好' as u32),
            vec![registry_spec("gb2312.1980-0")]
        );
    }

    #[test]
    fn partial_overlap_append_splits_ranges() {
        crate::test_utils::init_test_tracing();
        let mut data = FontsetData::default();
        data.update_target(
            FontsetTarget::Range(0x1000, 0x1005),
            registry_spec("base"),
            FontsetAddMode::Overwrite,
        );
        data.update_target(
            FontsetTarget::Range(0x1002, 0x1003),
            registry_spec("extra"),
            FontsetAddMode::Append,
        );

        assert_eq!(
            data.specific_entries_for_char(0x1001),
            vec![registry_spec("base")]
        );
        assert_eq!(
            data.specific_entries_for_char(0x1002),
            vec![registry_spec("base"), registry_spec("extra")]
        );
        assert_eq!(
            data.specific_entries_for_char(0x1004),
            vec![registry_spec("base")]
        );
    }

    #[test]
    fn fallback_entries_append_after_specific_entries() {
        crate::test_utils::init_test_tracing();
        let mut data = FontsetData::default();
        data.update_target(
            FontsetTarget::Range(0x4E00, 0x9FFF),
            registry_spec("gb2312.1980-0"),
            FontsetAddMode::Overwrite,
        );
        data.update_target(
            FontsetTarget::Fallback,
            registry_spec("iso10646-1"),
            FontsetAddMode::Append,
        );

        assert_eq!(
            data.matching_entries_for_char('好' as u32),
            vec![registry_spec("gb2312.1980-0"), registry_spec("iso10646-1")]
        );
    }

    #[test]
    fn repertory_charset_filters_non_matching_entries() {
        crate::test_utils::init_test_tracing();
        let mut data = FontsetData::default();
        data.update_target(
            FontsetTarget::Range(0x80, 0x10FFFF),
            FontSpecEntry::Font(StoredFontSpec {
                family: None,
                registry: Some("iso8859-1".to_string()),
                lang: None,
                weight: None,
                slant: None,
                width: None,
                repertory: Some(FontRepertory::Charset("iso-8859-1".to_string())),
            }),
            FontsetAddMode::Append,
        );
        data.update_target(
            FontsetTarget::Range(0x80, 0x10FFFF),
            FontSpecEntry::Font(StoredFontSpec {
                family: None,
                registry: Some("iso10646-1".to_string()),
                lang: None,
                weight: None,
                slant: None,
                width: None,
                repertory: Some(FontRepertory::Charset("unicode-bmp".to_string())),
            }),
            FontsetAddMode::Append,
        );

        let registries: Vec<_> = data
            .matching_entries_for_char('好' as u32)
            .into_iter()
            .filter_map(|entry| match entry {
                FontSpecEntry::Font(spec) => spec.registry,
                FontSpecEntry::ExplicitNone => None,
            })
            .collect();

        assert_eq!(registries, vec!["iso10646-1".to_string()]);
    }

    #[test]
    fn repertory_subset_charset_filters_non_matching_entries() {
        crate::test_utils::init_test_tracing();
        reset_charset_registry();

        let mut parent_args = vec![Value::NIL; 17];
        parent_args[0] = Value::symbol("latin-iso8859-2-test");
        parent_args[1] = Value::fixnum(1);
        parent_args[2] = Value::vector(vec![Value::fixnum(0), Value::fixnum(255)]);
        parent_args[8] = Value::T;
        parent_args[12] = Value::string("8859-2");
        builtin_define_charset_internal(parent_args).unwrap();

        let mut subset_args = vec![Value::NIL; 17];
        subset_args[0] = Value::symbol("iso-8859-2-test");
        subset_args[1] = Value::fixnum(1);
        subset_args[2] = Value::vector(vec![Value::fixnum(32), Value::fixnum(127)]);
        subset_args[13] = Value::list(vec![
            Value::symbol("latin-iso8859-2-test"),
            Value::fixnum(160),
            Value::fixnum(255),
            Value::fixnum(-128),
        ]);
        builtin_define_charset_internal(subset_args).unwrap();

        let mut data = FontsetData::default();
        data.update_target(
            FontsetTarget::Range(0x80, 0x10FFFF),
            FontSpecEntry::Font(StoredFontSpec {
                family: None,
                registry: Some("iso8859-2".to_string()),
                lang: None,
                weight: None,
                slant: None,
                width: None,
                repertory: Some(FontRepertory::Charset("iso-8859-2-test".to_string())),
            }),
            FontsetAddMode::Append,
        );
        data.update_target(
            FontsetTarget::Range(0x80, 0x10FFFF),
            FontSpecEntry::Font(StoredFontSpec {
                family: None,
                registry: Some("iso10646-1".to_string()),
                lang: None,
                weight: None,
                slant: None,
                width: None,
                repertory: Some(FontRepertory::Charset("unicode-bmp".to_string())),
            }),
            FontsetAddMode::Append,
        );

        let registries: Vec<_> = data
            .matching_entries_for_char('好' as u32)
            .into_iter()
            .filter_map(|entry| match entry {
                FontSpecEntry::Font(spec) => spec.registry,
                FontSpecEntry::ExplicitNone => None,
            })
            .collect();

        assert_eq!(registries, vec!["iso10646-1".to_string()]);
    }

    #[test]
    fn repertory_target_ranges_support_subset_charsets() {
        crate::test_utils::init_test_tracing();
        reset_charset_registry();

        let mut parent_args = vec![Value::NIL; 17];
        parent_args[0] = Value::symbol("latin-iso8859-2-test");
        parent_args[1] = Value::fixnum(1);
        parent_args[2] = Value::vector(vec![Value::fixnum(0), Value::fixnum(255)]);
        parent_args[8] = Value::T;
        parent_args[12] = Value::string("8859-2");
        builtin_define_charset_internal(parent_args).unwrap();

        let mut subset_args = vec![Value::NIL; 17];
        subset_args[0] = Value::symbol("iso-8859-2-test");
        subset_args[1] = Value::fixnum(1);
        subset_args[2] = Value::vector(vec![Value::fixnum(32), Value::fixnum(127)]);
        subset_args[13] = Value::list(vec![
            Value::symbol("latin-iso8859-2-test"),
            Value::fixnum(160),
            Value::fixnum(255),
            Value::fixnum(-128),
        ]);
        builtin_define_charset_internal(subset_args).unwrap();

        let ranges = repertory_target_ranges(&FontRepertory::Charset("iso-8859-2-test".into()))
            .expect("subset repertory ranges");
        assert!(
            ranges
                .iter()
                .any(|(from, to)| *from <= 0x00A0 && 0x00A0 <= *to)
        );
        assert!(
            ranges
                .iter()
                .any(|(from, to)| *from <= 0x017D && 0x017D <= *to)
        );
    }

    #[test]
    fn parse_font_spec_entry_preserves_raw_unibyte_string_names() {
        crate::test_utils::init_test_tracing();
        let raw = Value::heap_string(crate::heap_types::LispString::from_unibyte(vec![0xFF]));
        let expected = raw.as_runtime_string_owned().expect("runtime string");
        let entry = parse_font_spec_entry(&raw, None).expect("parse raw font spec");
        match entry {
            FontSpecEntry::Font(spec) => {
                assert_eq!(spec.family.as_deref(), Some(expected.as_str()));
                assert_eq!(spec.registry, None);
            }
            FontSpecEntry::ExplicitNone => panic!("expected font entry"),
        }
    }
}
