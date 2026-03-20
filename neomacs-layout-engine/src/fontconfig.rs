//! Fontconfig-based font discovery aligned more closely with GNU Emacs.
//!
//! GNU Emacs does not use `fc-match` as its primary font fallback pipeline for
//! character lookup.  Instead, `fontset.c` chooses an ordered font-spec list,
//! `font.c` expands family alternatives and scores style closeness, and
//! `ftfont.c` discovers candidates with `FcFontList`.
//!
//! This module mirrors that shape:
//! - generic family resolution uses Fontconfig directly
//! - character fallback uses Neovm's shared fontset state
//! - candidate discovery uses Fontconfig's `FcFontList`
//! - style selection is scored in Rust instead of delegated to Fontconfig

use neovm_core::emacs_core::fontset::{
    FontSpecEntry, StoredFontSpec, fontset_generation, matching_entries_for_char,
};
use neovm_core::face::FontSlant;
use std::collections::HashMap;
#[cfg(unix)]
use std::ffi::{CStr, CString};
use std::process::Command;
#[cfg(unix)]
use std::ptr;
use std::sync::{Mutex, OnceLock};
#[cfg(unix)]
use {fontconfig::Pattern, fontconfig_sys};

/// Cached fontconfig resolution results.
static FC_CACHE: OnceLock<HashMap<String, String>> = OnceLock::new();
static FC_SPACING_CACHE: OnceLock<Mutex<HashMap<String, Option<i32>>>> = OnceLock::new();
static FC_CHAR_MATCH_CACHE: OnceLock<Mutex<HashMap<CharMatchCacheKey, Option<FontMatch>>>> =
    OnceLock::new();
#[cfg(unix)]
static FC_HANDLE: OnceLock<Option<fontconfig::Fontconfig>> = OnceLock::new();

/// Cached Xft.dpi value from X resources.
static XFT_DPI: OnceLock<f32> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontMatch {
    pub family: String,
    pub file: Option<String>,
    pub postscript_name: Option<String>,
    pub weight: Option<u16>,
    pub slant: FontSlant,
}

impl FontMatch {
    pub fn is_italic(&self) -> bool {
        self.slant.is_italic()
    }

    pub fn is_oblique(&self) -> bool {
        matches!(self.slant, FontSlant::Oblique | FontSlant::ReverseOblique)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CharMatchCacheKey {
    family: String,
    codepoint: u32,
    prefer_monospace: bool,
    requested_weight: u16,
    italic: bool,
    fontset_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ListedFont {
    matched: FontMatch,
    style: String,
    weight_css: Option<u16>,
}

#[derive(Clone, Copy, Debug)]
struct RegistryHint {
    name: &'static str,
    uniquifiers: &'static [u32],
    lang: Option<&'static str>,
}

// Mirrors the FreeType/fontconfig registry hints in GNU Emacs' ftfont.c.
const REGISTRY_HINTS: &[RegistryHint] = &[
    RegistryHint {
        name: "iso8859-1",
        uniquifiers: &[0x00A0, 0x00A1, 0x00B4, 0x00BC, 0x00D0],
        lang: None,
    },
    RegistryHint {
        name: "iso8859-2",
        uniquifiers: &[0x00A0, 0x010E],
        lang: None,
    },
    RegistryHint {
        name: "iso8859-3",
        uniquifiers: &[0x00A0, 0x0108],
        lang: None,
    },
    RegistryHint {
        name: "iso8859-4",
        uniquifiers: &[0x00A0, 0x00AF, 0x0128, 0x0156, 0x02C7],
        lang: None,
    },
    RegistryHint {
        name: "iso8859-5",
        uniquifiers: &[0x00A0, 0x0401],
        lang: None,
    },
    RegistryHint {
        name: "iso8859-6",
        uniquifiers: &[0x00A0, 0x060C],
        lang: None,
    },
    RegistryHint {
        name: "iso8859-7",
        uniquifiers: &[0x00A0, 0x0384],
        lang: None,
    },
    RegistryHint {
        name: "iso8859-8",
        uniquifiers: &[0x00A0, 0x05D0],
        lang: None,
    },
    RegistryHint {
        name: "iso8859-9",
        uniquifiers: &[0x00A0, 0x00A1, 0x00BC, 0x011E],
        lang: None,
    },
    RegistryHint {
        name: "iso8859-10",
        uniquifiers: &[0x00A0, 0x00D0, 0x0128, 0x2015],
        lang: None,
    },
    RegistryHint {
        name: "iso8859-11",
        uniquifiers: &[0x00A0, 0x0E01],
        lang: None,
    },
    RegistryHint {
        name: "iso8859-13",
        uniquifiers: &[0x00A0, 0x201C],
        lang: None,
    },
    RegistryHint {
        name: "iso8859-14",
        uniquifiers: &[0x00A0, 0x0174],
        lang: None,
    },
    RegistryHint {
        name: "iso8859-15",
        uniquifiers: &[0x00A0, 0x00A1, 0x00D0, 0x0152],
        lang: None,
    },
    RegistryHint {
        name: "iso8859-16",
        uniquifiers: &[0x00A0, 0x0218],
        lang: None,
    },
    RegistryHint {
        name: "gb2312.1980-0",
        uniquifiers: &[0x4E13],
        lang: Some("zh-cn"),
    },
    RegistryHint {
        name: "big5-0",
        uniquifiers: &[0x9C21],
        lang: Some("zh-tw"),
    },
    RegistryHint {
        name: "jisx0208.1983-0",
        uniquifiers: &[0x4E55],
        lang: Some("ja"),
    },
    RegistryHint {
        name: "ksc5601.1985-0",
        uniquifiers: &[0xAC00],
        lang: Some("ko"),
    },
    RegistryHint {
        name: "cns11643.1992-1",
        uniquifiers: &[0xFE32],
        lang: Some("zh-tw"),
    },
    RegistryHint {
        name: "cns11643.1992-2",
        uniquifiers: &[0x4E33, 0x7934],
        lang: None,
    },
    RegistryHint {
        name: "cns11643.1992-3",
        uniquifiers: &[0x201A9],
        lang: None,
    },
    RegistryHint {
        name: "cns11643.1992-4",
        uniquifiers: &[0x20057],
        lang: None,
    },
    RegistryHint {
        name: "cns11643.1992-5",
        uniquifiers: &[0x20000],
        lang: None,
    },
    RegistryHint {
        name: "cns11643.1992-6",
        uniquifiers: &[0x20003],
        lang: None,
    },
    RegistryHint {
        name: "cns11643.1992-7",
        uniquifiers: &[0x20055],
        lang: None,
    },
    RegistryHint {
        name: "gbk-0",
        uniquifiers: &[0x4E06],
        lang: Some("zh-cn"),
    },
    RegistryHint {
        name: "jisx0212.1990-0",
        uniquifiers: &[0x4E44],
        lang: None,
    },
    RegistryHint {
        name: "jisx0213.2000-1",
        uniquifiers: &[0xFA10],
        lang: Some("ja"),
    },
    RegistryHint {
        name: "jisx0213.2000-2",
        uniquifiers: &[0xFA49],
        lang: None,
    },
    RegistryHint {
        name: "jisx0213.2004-1",
        uniquifiers: &[0x20B9F],
        lang: None,
    },
    RegistryHint {
        name: "viscii1.1-1",
        uniquifiers: &[0x1EA0, 0x1EAE, 0x1ED2],
        lang: Some("vi"),
    },
    RegistryHint {
        name: "tis620.2529-1",
        uniquifiers: &[0x0E01],
        lang: Some("th"),
    },
    RegistryHint {
        name: "microsoft-cp1251",
        uniquifiers: &[0x0401, 0x0490],
        lang: Some("ru"),
    },
    RegistryHint {
        name: "koi8-r",
        uniquifiers: &[0x0401, 0x2219],
        lang: Some("ru"),
    },
    RegistryHint {
        name: "mulelao-1",
        uniquifiers: &[0x0E81],
        lang: Some("lo"),
    },
    RegistryHint {
        name: "unicode-sip",
        uniquifiers: &[0x20000],
        lang: None,
    },
];

/// Resolve a generic font family name through fontconfig.
///
/// For generic names ("Monospace", "Serif", "Sans Serif"), queries fontconfig
/// to find the concrete family name. Returns the original name unchanged for
/// non-generic families or if fontconfig is unavailable.
pub fn resolve_family(generic_name: &str) -> &str {
    let cache = FC_CACHE.get_or_init(|| {
        let mut map = HashMap::new();
        for generic in &["monospace", "serif", "sans-serif", "sans serif"] {
            if let Some(concrete) = fc_match_family(generic) {
                tracing::info!("fontconfig: {} -> {}", generic, concrete);
                map.insert(generic.to_string(), concrete);
            }
        }
        map
    });

    let lower = generic_name.to_lowercase();
    if let Some(concrete) = cache.get(&lower) {
        concrete.as_str()
    } else {
        generic_name
    }
}

/// Return true when FAMILY should prefer a monospace fallback for uncovered
/// characters, matching GNU Emacs' fontset behavior more closely than
/// cosmic-text's generic fallback.
pub fn family_prefers_monospace(family: &str) -> bool {
    let lower = family.to_lowercase();
    if matches!(lower.as_str(), "" | "mono" | "monospace")
        || lower.contains(" mono")
        || lower.ends_with("mono")
    {
        return true;
    }

    family_spacing(family) == Some(100)
}

/// Resolve a character-capable fallback font through shared fontset state and
/// a Fontconfig candidate list, approximating GNU Emacs' `face_for_char`
/// pipeline.
pub fn match_font_for_char(
    family: &str,
    ch: char,
    prefer_monospace: bool,
    requested_weight: u16,
    italic: bool,
) -> Option<FontMatch> {
    if ch.is_ascii() {
        return None;
    }

    let key = CharMatchCacheKey {
        family: family.to_string(),
        codepoint: ch as u32,
        prefer_monospace,
        requested_weight,
        italic,
        fontset_generation: fontset_generation(),
    };
    let cache = FC_CHAR_MATCH_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache) = cache.lock()
        && let Some(cached) = cache.get(&key)
    {
        return cached.clone();
    }

    let matched =
        match_font_for_char_uncached(family, ch, prefer_monospace, requested_weight, italic);

    if let Ok(mut cache) = cache.lock() {
        cache.insert(key, matched.clone());
    }

    matched
}

/// Get the effective DPI for font sizing.
///
/// Reads `Xft.dpi` from X resources (same as GNU Emacs), falling back to 96.
pub fn xft_dpi() -> f32 {
    *XFT_DPI.get_or_init(|| {
        let dpi = query_xft_dpi().unwrap_or(96.0);
        tracing::info!("Xft.dpi: {}", dpi);
        dpi
    })
}

/// Convert a point size to pixels using GNU Emacs' X11 rule.
pub fn points_to_pixels(points: f32) -> f32 {
    points * xft_dpi() / 72.0
}

/// Convert a face height in 1/10 pt to pixels using GNU Emacs' X11 rule.
pub fn face_height_to_pixels(tenths: i32) -> f32 {
    points_to_pixels(tenths as f32 / 10.0)
}

fn match_font_for_char_uncached(
    family: &str,
    ch: char,
    prefer_monospace: bool,
    requested_weight: u16,
    italic: bool,
) -> Option<FontMatch> {
    for entry in matching_entries_for_char(ch) {
        match entry {
            FontSpecEntry::ExplicitNone => {
                tracing::debug!(
                    "fontconfig: U+{:04X} hit explicit nil fontset entry",
                    ch as u32
                );
                return None;
            }
            FontSpecEntry::Font(spec) => {
                if let Some(matched) = match_font_from_spec(
                    family,
                    ch,
                    prefer_monospace,
                    requested_weight,
                    italic,
                    &spec,
                ) {
                    tracing::debug!(
                        "fontconfig: fontset matched U+{:04X} req_family={} spec={:?} -> {:?}",
                        ch as u32,
                        family,
                        spec,
                        matched
                    );
                    return Some(matched);
                }
            }
        }
    }

    let fallback_spec = StoredFontSpec {
        family: None,
        registry: None,
        lang: None,
        weight: None,
        slant: None,
        width: None,
        repertory: None,
    };
    let matched = match_font_from_spec(
        family,
        ch,
        prefer_monospace,
        requested_weight,
        italic,
        &fallback_spec,
    );
    if let Some(ref matched) = matched {
        tracing::debug!(
            "fontconfig: generic fallback matched U+{:04X} req_family={} -> {:?}",
            ch as u32,
            family,
            matched
        );
    }
    matched
}

fn match_font_from_spec(
    requested_family: &str,
    ch: char,
    _prefer_monospace: bool,
    requested_weight: u16,
    italic: bool,
    spec: &StoredFontSpec,
) -> Option<FontMatch> {
    let effective_weight = spec
        .weight
        .map(|weight| weight.0)
        .unwrap_or(requested_weight);
    let query_chars = registry_query_chars(spec.registry.as_deref(), ch);
    let registry_lang = spec
        .registry
        .as_deref()
        .and_then(registry_hint)
        .and_then(|hint| hint.lang)
        .map(str::to_string);
    let query_langs = combined_query_langs(registry_lang.as_deref(), spec.lang.as_deref());
    for family_option in family_search_order(requested_family, spec) {
        let candidates = fc_list_candidates(family_option.as_deref(), &query_chars, &query_langs);
        if candidates.is_empty() {
            continue;
        }

        let best = candidates.into_iter().min_by_key(|candidate| {
            candidate_score(candidate, effective_weight, italic, spec.slant)
        })?;
        return Some(best.matched);
    }
    None
}

fn family_search_order(requested_family: &str, spec: &StoredFontSpec) -> Vec<Option<String>> {
    if let Some(spec_family) = spec.family.as_deref() {
        return vec![Some(resolve_family(spec_family).to_string())];
    }

    if requested_family.is_empty() {
        return vec![None];
    }

    let resolved = resolve_family(requested_family);
    if resolved == requested_family {
        vec![Some(requested_family.to_string()), None]
    } else {
        vec![
            Some(resolved.to_string()),
            Some(requested_family.to_string()),
            None,
        ]
    }
}

fn candidate_score(
    candidate: &ListedFont,
    requested_weight: u16,
    italic: bool,
    requested_slant: Option<neovm_core::face::FontSlant>,
) -> u32 {
    let style = candidate.style.to_ascii_lowercase();
    let candidate_weight = candidate
        .weight_css
        .or_else(|| style_weight(&style))
        .unwrap_or(400);
    let candidate_slant = style_slant(&style);
    let requested_slant = requested_slant.unwrap_or(if italic {
        neovm_core::face::FontSlant::Italic
    } else {
        neovm_core::face::FontSlant::Normal
    });

    let mut score = u32::from(candidate_weight.abs_diff(requested_weight));
    score += slant_distance(requested_slant, candidate_slant);
    score
}

fn slant_distance(
    requested: neovm_core::face::FontSlant,
    candidate: neovm_core::face::FontSlant,
) -> u32 {
    use neovm_core::face::FontSlant::{Italic, Normal, Oblique, ReverseItalic, ReverseOblique};

    match (requested, candidate) {
        (Normal, Normal) => 0,
        (Italic, Italic) | (Italic, Oblique) => 0,
        (Oblique, Oblique) | (Oblique, Italic) => 0,
        (ReverseItalic, ReverseItalic) | (ReverseItalic, ReverseOblique) => 0,
        (ReverseOblique, ReverseOblique) | (ReverseOblique, ReverseItalic) => 0,
        (Normal, _) => 350,
        (_, Normal) => 250,
        _ => 75,
    }
}

fn style_weight(style: &str) -> Option<u16> {
    let normalized = style.to_ascii_lowercase().replace([' ', '-'], "");
    for (needle, weight) in [
        ("thin", 100u16),
        ("ultralight", 100),
        ("extralight", 200),
        ("light", 300),
        ("semibold", 600),
        ("demibold", 600),
        ("bold", 700),
        ("extrabold", 800),
        ("ultrabold", 800),
        ("regular", 400),
        ("book", 400),
        ("medium", 500),
        ("black", 900),
        ("heavy", 900),
    ] {
        if normalized.contains(needle) {
            return Some(weight);
        }
    }
    None
}

fn style_slant(style: &str) -> neovm_core::face::FontSlant {
    if style.contains("oblique") {
        FontSlant::Oblique
    } else if style.contains("italic") {
        FontSlant::Italic
    } else {
        FontSlant::Normal
    }
}

#[cfg(unix)]
fn fontconfig_handle() -> Option<&'static fontconfig::Fontconfig> {
    FC_HANDLE.get_or_init(fontconfig::Fontconfig::new).as_ref()
}

#[cfg(unix)]
struct FcCharSetGuard(*mut fontconfig_sys::FcCharSet);

#[cfg(unix)]
impl FcCharSetGuard {
    fn new(query_chars: &[u32]) -> Option<Self> {
        let charset = unsafe { fontconfig_sys::FcCharSetCreate() };
        if charset.is_null() {
            return None;
        }
        for &codepoint in query_chars {
            let ok = unsafe {
                fontconfig_sys::FcCharSetAddChar(charset, codepoint as fontconfig_sys::FcChar32)
            };
            if ok == 0 {
                unsafe { fontconfig_sys::FcCharSetDestroy(charset) };
                return None;
            }
        }
        Some(Self(charset))
    }
}

#[cfg(unix)]
impl Drop for FcCharSetGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { fontconfig_sys::FcCharSetDestroy(self.0) };
        }
    }
}

#[cfg(unix)]
struct FcObjectSetGuard(*mut fontconfig_sys::FcObjectSet);

#[cfg(unix)]
impl Drop for FcObjectSetGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { fontconfig_sys::FcObjectSetDestroy(self.0) };
        }
    }
}

#[cfg(unix)]
struct FcFontSetGuard(*mut fontconfig_sys::FcFontSet);

#[cfg(unix)]
impl Drop for FcFontSetGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { fontconfig_sys::FcFontSetDestroy(self.0) };
        }
    }
}

#[cfg(unix)]
fn add_string_property(pattern: &mut Pattern<'_>, key: &std::ffi::CStr, value: &str) -> bool {
    let Ok(value) = CString::new(value) else {
        return false;
    };
    pattern.add_string(key, &value);
    true
}

#[cfg(unix)]
fn add_charset_property(pattern: &mut Pattern<'_>, query_chars: &[u32]) -> bool {
    let Some(charset) = FcCharSetGuard::new(query_chars) else {
        return false;
    };
    let ok = unsafe {
        fontconfig_sys::FcPatternAddCharSet(
            pattern.as_mut_ptr(),
            fontconfig::FC_CHARSET.as_ptr(),
            charset.0,
        )
    };
    ok != 0
}

#[cfg(unix)]
fn pattern_family(pattern: &Pattern<'_>) -> Option<String> {
    pattern
        .get_string(fontconfig::FC_FAMILY)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(unix)]
fn build_candidate_object_set() -> Option<FcObjectSetGuard> {
    let object_set = unsafe { fontconfig_sys::FcObjectSetCreate() };
    if object_set.is_null() {
        return None;
    }
    let guard = FcObjectSetGuard(object_set);
    for key in [
        fontconfig::FC_FAMILY,
        fontconfig::FC_STYLE,
        fontconfig::FC_WEIGHT,
        fontconfig::FC_SLANT,
        fontconfig::FC_POSTSCRIPT_NAME,
        fontconfig::FC_FILE,
    ] {
        let ok = unsafe { fontconfig_sys::FcObjectSetAdd(object_set, key.as_ptr()) };
        if ok == 0 {
            return None;
        }
    }
    Some(guard)
}

#[cfg(unix)]
fn raw_pattern_string(pattern: *mut fontconfig_sys::FcPattern, key: &CStr) -> Option<String> {
    let mut raw_value: *mut fontconfig_sys::FcChar8 = ptr::null_mut();
    let result =
        unsafe { fontconfig_sys::FcPatternGetString(pattern, key.as_ptr(), 0, &mut raw_value) };
    if result != fontconfig_sys::FcResultMatch || raw_value.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(raw_value.cast()) }
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(unix)]
fn raw_pattern_int(pattern: *mut fontconfig_sys::FcPattern, key: &CStr) -> Option<i32> {
    let mut value = 0;
    let result =
        unsafe { fontconfig_sys::FcPatternGetInteger(pattern, key.as_ptr(), 0, &mut value) };
    (result == fontconfig_sys::FcResultMatch).then_some(value)
}

#[cfg(unix)]
fn listed_font_from_raw_pattern(pattern: *mut fontconfig_sys::FcPattern) -> Option<ListedFont> {
    if pattern.is_null() {
        return None;
    }
    let style = raw_pattern_string(pattern, fontconfig::FC_STYLE).unwrap_or_default();
    let weight_css = raw_pattern_int(pattern, fontconfig::FC_WEIGHT).map(map_fontconfig_weight_raw);
    if style.is_empty() && weight_css.is_none() {
        return None;
    }
    let matched_family = raw_pattern_string(pattern, fontconfig::FC_FAMILY)?;
    Some(ListedFont {
        matched: FontMatch {
            family: matched_family,
            file: raw_pattern_string(pattern, fontconfig::FC_FILE),
            postscript_name: raw_pattern_string(pattern, fontconfig::FC_POSTSCRIPT_NAME),
            weight: weight_css,
            slant: raw_pattern_int(pattern, fontconfig::FC_SLANT)
                .map(map_fontconfig_slant_raw)
                .unwrap_or_else(|| style_slant(&style)),
        },
        style,
        weight_css,
    })
}

#[cfg(unix)]
fn map_fontconfig_weight_raw(weight: i32) -> u16 {
    match weight {
        i32::MIN..=20 => 100,
        21..=45 => 200,
        46..=62 => 300,
        63..=89 => 400,
        90..=139 => 500,
        140..=189 => 600,
        190..=204 => 700,
        205..=212 => 900,
        _ => 900,
    }
}

#[cfg(unix)]
fn map_fontconfig_slant_raw(slant: i32) -> FontSlant {
    match slant {
        fontconfig::FC_SLANT_ITALIC => FontSlant::Italic,
        fontconfig::FC_SLANT_OBLIQUE => FontSlant::Oblique,
        _ => FontSlant::Normal,
    }
}

#[cfg(unix)]
fn fc_list_candidates(
    family: Option<&str>,
    query_chars: &[u32],
    langs: &[String],
) -> Vec<ListedFont> {
    let Some(fc) = fontconfig_handle() else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    let query_langs = if langs.is_empty() {
        vec![None]
    } else {
        langs.iter().map(|lang| Some(lang.as_str())).collect()
    };

    for lang in query_langs {
        let mut pattern = Pattern::new(fc);
        if !add_charset_property(&mut pattern, query_chars) {
            continue;
        }
        if let Some(family) = family.filter(|family| !family.is_empty()) {
            if !add_string_property(&mut pattern, fontconfig::FC_FAMILY, family) {
                continue;
            }
        }
        if let Some(lang) = lang.filter(|lang| !lang.is_empty()) {
            if !add_string_property(&mut pattern, fontconfig::FC_LANG, lang) {
                continue;
            }
        }
        pattern.config_substitute();
        pattern.default_substitute();
        let Some(object_set) = build_candidate_object_set() else {
            continue;
        };
        let fontset = unsafe {
            FcFontSetGuard(fontconfig_sys::FcFontList(
                ptr::null_mut(),
                pattern.as_mut_ptr(),
                object_set.0,
            ))
        };
        if fontset.0.is_null() {
            continue;
        }
        let nfont = unsafe { (*fontset.0).nfont };
        if nfont <= 0 {
            continue;
        }
        let fonts = unsafe { (*fontset.0).fonts };
        if fonts.is_null() {
            continue;
        }
        let patterns = unsafe { std::slice::from_raw_parts(fonts, nfont as usize) };

        for &candidate_pattern in patterns {
            let Some(candidate) = listed_font_from_raw_pattern(candidate_pattern) else {
                continue;
            };
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    candidates
}

#[cfg(not(unix))]
fn fc_list_candidates(
    family: Option<&str>,
    query_chars: &[u32],
    langs: &[String],
) -> Vec<ListedFont> {
    let mut candidates = Vec::new();
    let query_langs = if langs.is_empty() {
        vec![None]
    } else {
        langs.iter().map(|lang| Some(lang.as_str())).collect()
    };

    for lang in query_langs {
        let mut pattern = String::from(":charset=");
        for (index, codepoint) in query_chars.iter().enumerate() {
            if index > 0 {
                pattern.push(' ');
            }
            pattern.push_str(&format!("0x{:x}", codepoint));
        }
        if let Some(family) = family.filter(|family| !family.is_empty()) {
            pattern.push_str(":family=");
            pattern.push_str(family);
        }
        if let Some(lang) = lang.filter(|lang| !lang.is_empty()) {
            pattern.push_str(":lang=");
            pattern.push_str(lang);
        }

        let output = match Command::new("fc-list")
            .arg(pattern)
            .arg("--format=%{family}\t%{style}\t%{weight}\t%{postscriptname}\t%{file}\n")
            .output()
        {
            Ok(output) if output.status.success() => output,
            _ => continue,
        };

        let Ok(stdout) = String::from_utf8(output.stdout) else {
            continue;
        };

        for line in stdout.lines() {
            let mut fields = line.splitn(5, '\t');
            let Some(families) = fields.next() else {
                continue;
            };
            let style = fields.next().unwrap_or_default().trim().to_string();
            let weight_css = parse_fontconfig_weight(fields.next().unwrap_or_default());
            let postscript_name = fields
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned);
            let file = fields
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned);
            if style.is_empty() && weight_css.is_none() {
                continue;
            }
            let Some(matched_family) = choose_display_family(families, family) else {
                continue;
            };
            let candidate = ListedFont {
                matched: FontMatch {
                    family: matched_family,
                    file,
                    postscript_name,
                    weight: weight_css,
                    slant: style_slant(&style),
                },
                style,
                weight_css,
            };
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    candidates
}

#[cfg(not(unix))]
fn choose_display_family(families: &str, requested_family: Option<&str>) -> Option<String> {
    let mut names = families
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(requested) = requested_family
        && let Some(exact) = names
            .clone()
            .find(|candidate| candidate.eq_ignore_ascii_case(requested))
    {
        return Some(exact.to_string());
    }
    names.next().map(ToOwned::to_owned)
}

#[cfg(any(not(unix), test))]
fn parse_fontconfig_weight(raw: &str) -> Option<u16> {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with('[') {
        return None;
    }
    let weight = raw.parse::<i32>().ok()?;
    Some(match weight {
        i32::MIN..=20 => 100,
        21..=45 => 200,
        46..=62 => 300,
        63..=89 => 400,
        90..=139 => 500,
        140..=189 => 600,
        190..=204 => 700,
        205..=212 => 900,
        _ => 900,
    })
}

fn combined_query_langs(registry_lang: Option<&str>, spec_lang: Option<&str>) -> Vec<String> {
    let mut langs = Vec::new();
    for lang in [registry_lang, spec_lang] {
        let Some(lang) = lang.map(str::trim).filter(|lang| !lang.is_empty()) else {
            continue;
        };
        let lang = lang.to_ascii_lowercase();
        if !langs.contains(&lang) {
            langs.push(lang);
        }
    }
    langs
}

fn registry_query_chars(registry: Option<&str>, ch: char) -> Vec<u32> {
    registry
        .and_then(registry_hint)
        .map(|hint| hint.uniquifiers.to_vec())
        .filter(|chars| !chars.is_empty())
        .unwrap_or_else(|| vec![ch as u32])
}

fn registry_hint(registry: &str) -> Option<&'static RegistryHint> {
    let registry = registry.trim().to_ascii_lowercase();
    REGISTRY_HINTS
        .iter()
        .find(|hint| wildcard_casefold_match(&registry, hint.name))
}

fn wildcard_casefold_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase().into_bytes();
    let text = text.to_ascii_lowercase().into_bytes();
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut star_t) = (None, 0usize);

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
            continue;
        }
        if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            star_t = t;
            continue;
        }
        if let Some(star_pos) = star {
            p = star_pos + 1;
            star_t += 1;
            t = star_t;
            continue;
        }
        return false;
    }

    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

/// Query Xft.dpi from X resources via `xrdb -query`.
fn query_xft_dpi() -> Option<f32> {
    let output = Command::new("xrdb").arg("-query").output().ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("Xft.dpi:") {
            return rest.trim().parse::<f32>().ok();
        }
    }
    None
}

#[cfg(unix)]
/// Query Fontconfig for the concrete family name matching a generic family.
fn fc_match_family(generic: &str) -> Option<String> {
    let fc = fontconfig_handle()?;
    let mut pattern = Pattern::new(fc);
    if !add_string_property(&mut pattern, fontconfig::FC_FAMILY, generic) {
        return None;
    }
    let matched = pattern.font_match();
    let family = pattern_family(&matched)?;
    if family.is_empty() {
        None
    } else {
        Some(family)
    }
}

#[cfg(not(unix))]
/// Query `fc-match` for the concrete family name matching a generic family.
fn fc_match_family(generic: &str) -> Option<String> {
    let output = Command::new("fc-match")
        .arg("--format=%{family}")
        .arg(generic)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let family = String::from_utf8(output.stdout).ok()?;
    let family = family.trim().to_string();
    let family = family.split(',').next()?.trim().to_string();

    if family.is_empty() {
        None
    } else {
        Some(family)
    }
}

#[cfg(unix)]
fn family_spacing(family: &str) -> Option<i32> {
    let cache = FC_SPACING_CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    if let Ok(cache) = cache.lock()
        && let Some(cached) = cache.get(family)
    {
        return *cached;
    }

    let spacing = fontconfig_handle().and_then(|fc| {
        let mut pattern = Pattern::new(fc);
        if !add_string_property(&mut pattern, fontconfig::FC_FAMILY, family) {
            return None;
        }
        let matched = pattern.font_match();
        matched.get_int(fontconfig::FC_SPACING)
    });

    if let Ok(mut cache) = cache.lock() {
        cache.insert(family.to_string(), spacing);
    }

    spacing
}

#[cfg(not(unix))]
fn family_spacing(family: &str) -> Option<i32> {
    let cache = FC_SPACING_CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    if let Ok(cache) = cache.lock()
        && let Some(cached) = cache.get(family)
    {
        return *cached;
    }

    let spacing = Command::new("fc-match")
        .arg("--format=%{spacing}")
        .arg(family)
        .output()
        .ok()
        .and_then(|output| {
            if !output.status.success() {
                return None;
            }
            String::from_utf8(output.stdout).ok()
        })
        .and_then(|stdout| stdout.trim().parse::<i32>().ok());

    if let Ok(mut cache) = cache.lock() {
        cache.insert(family.to_string(), spacing);
    }

    spacing
}

#[cfg(test)]
mod tests {
    use super::{
        combined_query_langs, fc_list_candidates, parse_fontconfig_weight, registry_hint,
        registry_query_chars, style_weight, wildcard_casefold_match,
    };

    #[test]
    fn registry_hint_matches_wildcard_patterns() {
        let hint = registry_hint("JISX0208*").expect("jisx0208 wildcard");
        assert_eq!(hint.lang, Some("ja"));
        assert_eq!(hint.uniquifiers, &[0x4E55]);
    }

    #[test]
    fn registry_hint_matches_case_insensitively() {
        let hint = registry_hint("GB2312.1980-0").expect("gb2312");
        assert_eq!(hint.lang, Some("zh-cn"));
        assert_eq!(hint.uniquifiers, &[0x4E13]);
    }

    #[test]
    fn registry_query_chars_use_gnu_uniquifiers() {
        assert_eq!(
            registry_query_chars(Some("gb2312.1980-0"), '好'),
            vec![0x4E13]
        );
        assert_eq!(
            registry_query_chars(Some("cns11643.1992-2"), '好'),
            vec![0x4E33, 0x7934]
        );
        assert_eq!(registry_query_chars(None, '好'), vec!['好' as u32]);
    }

    #[test]
    fn registry_and_spec_langs_are_deduplicated() {
        assert_eq!(
            combined_query_langs(Some("zh-cn"), Some("zh-cn")),
            vec!["zh-cn"]
        );
        assert_eq!(
            combined_query_langs(Some("zh-cn"), Some("zh")),
            vec!["zh-cn", "zh"]
        );
    }

    #[test]
    fn wildcard_match_handles_star_and_question() {
        assert!(wildcard_casefold_match("jisx0208*", "jisx0208.1983-0"));
        assert!(wildcard_casefold_match("gb?-0", "gbk-0"));
        assert!(!wildcard_casefold_match("big5-0", "gbk-0"));
    }

    #[test]
    fn parse_fontconfig_weight_maps_known_ranges() {
        assert_eq!(parse_fontconfig_weight("0"), Some(100));
        assert_eq!(parse_fontconfig_weight("40"), Some(200));
        assert_eq!(parse_fontconfig_weight("50"), Some(300));
        assert_eq!(parse_fontconfig_weight("80"), Some(400));
        assert_eq!(parse_fontconfig_weight("100"), Some(500));
        assert_eq!(parse_fontconfig_weight("180"), Some(600));
        assert_eq!(parse_fontconfig_weight("200"), Some(700));
        assert_eq!(parse_fontconfig_weight("210"), Some(900));
        assert_eq!(parse_fontconfig_weight("[80 200]"), None);
    }

    #[test]
    fn style_weight_prefers_semibold_over_regular_alias() {
        assert_eq!(style_weight("SemiBold,Regular"), Some(600));
        assert_eq!(style_weight("SemiBold Italic,Italic"), Some(600));
    }

    #[cfg(unix)]
    #[test]
    fn fc_list_candidates_tolerates_empty_fontsets() {
        let _ = fc_list_candidates(
            Some("definitely-missing-neomacs-font-family"),
            &[0x10FFFF],
            &[String::from("zz-zz")],
        );
    }
}
