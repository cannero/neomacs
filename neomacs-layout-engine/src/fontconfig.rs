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

#[cfg(unix)]
use fontconfig_sys::constants::{
    FC_RGBA, FC_RGBA_BGR, FC_RGBA_NONE, FC_RGBA_RGB, FC_RGBA_VBGR, FC_RGBA_VRGB,
};
use neovm_core::emacs_core::font::alternative_font_families;
use neovm_core::emacs_core::fontset::{
    FontSpecEntry, StoredFontSpec, fontset_generation, matching_entries_for_char,
    repertory_target_ranges,
};
use neovm_core::face::{FontSlant, FontWidth};
use std::collections::HashMap;
#[cfg(unix)]
use std::ffi::{CStr, CString};
#[cfg(not(unix))]
use std::process::Command;
#[cfg(unix)]
use std::ptr;
use std::sync::{Mutex, OnceLock};
#[cfg(unix)]
use x11_dl::xlib;
#[cfg(unix)]
use {fontconfig::Pattern, fontconfig_sys};

/// Cached fontconfig resolution results.
static FC_CACHE: OnceLock<HashMap<String, String>> = OnceLock::new();
static FC_SPACING_CACHE: OnceLock<Mutex<HashMap<String, Option<i32>>>> = OnceLock::new();
static FC_RGBA_CACHE: OnceLock<FontconfigSubpixelOrder> = OnceLock::new();
static FC_CHAR_MATCH_CACHE: OnceLock<Mutex<HashMap<CharMatchCacheKey, Option<FontMatch>>>> =
    OnceLock::new();
#[cfg(unix)]
static FC_HANDLE: OnceLock<Option<fontconfig::Fontconfig>> = OnceLock::new();

/// Cached Xft.dpi/frame DPI value from the active X display.
static XFT_DPI: OnceLock<f32> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontMatch {
    pub family: String,
    pub file: Option<String>,
    pub postscript_name: Option<String>,
    pub weight: Option<u16>,
    pub slant: FontSlant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontconfigSubpixelOrder {
    Unknown,
    None,
    Rgb,
    Bgr,
    VRgb,
    VBgr,
}

impl FontconfigSubpixelOrder {
    pub fn allows_horizontal_subpixel(self) -> bool {
        matches!(self, Self::Rgb | Self::Bgr)
    }
}

impl FontMatch {
    pub fn is_italic(&self) -> bool {
        self.slant.is_italic()
    }

    pub fn is_oblique(&self) -> bool {
        matches!(self.slant, FontSlant::Oblique | FontSlant::ReverseOblique)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpecFontMatch {
    pub family: String,
    pub registry: Option<String>,
    pub weight: Option<u16>,
    pub slant: FontSlant,
    pub width: Option<FontWidth>,
    pub spacing: Option<i32>,
    pub postscript_name: Option<String>,
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
    width: Option<FontWidth>,
    spacing: Option<i32>,
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

const FONT_SPACING_PROPORTIONAL: i32 = 0;
const FONT_SPACING_DUAL: i32 = 90;
const FONT_SPACING_MONO: i32 = 100;
const FONT_SPACING_CHARCELL: i32 = 110;
const FONT_SPACING_MONO_MAX: i32 = FONT_SPACING_CHARCELL - 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpacingClass {
    Proportional,
    Dual,
    Mono,
    Charcell,
}

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

pub fn find_font_for_spec(
    family: Option<&str>,
    registry: Option<&str>,
    lang: Option<&str>,
    weight: Option<u16>,
    slant: Option<FontSlant>,
) -> Option<SpecFontMatch> {
    let resolved_family = family
        .map(str::trim)
        .filter(|family| !family.is_empty())
        .map(resolve_family)
        .map(str::to_string);
    let spec = StoredFontSpec {
        family: resolved_family,
        registry: registry.map(str::to_string),
        lang: lang.map(str::to_string),
        weight: weight.map(neovm_core::face::FontWeight),
        slant,
        width: None,
        repertory: None,
    };
    let representative = representative_char_for_spec(&spec);
    let query_charset_ranges = query_charset_ranges(&spec, representative);
    let registry_lang = spec
        .registry
        .as_deref()
        .and_then(registry_hint)
        .and_then(|hint| hint.lang)
        .map(str::to_string);
    let query_langs = combined_query_langs(registry_lang.as_deref(), spec.lang.as_deref());
    let candidates = fc_list_candidates(
        spec.family.as_deref(),
        &query_charset_ranges,
        None,
        &query_langs,
    );

    candidates
        .into_iter()
        .find(|candidate| candidate_matches_find_font_spec(candidate, &spec))
        .map(|candidate| SpecFontMatch {
            family: candidate.matched.family,
            registry: Some("iso10646-1".to_string()),
            weight: candidate
                .weight_css
                .or_else(|| style_weight(&candidate.style)),
            slant: candidate.matched.slant,
            width: candidate.width,
            spacing: candidate.spacing,
            postscript_name: candidate.matched.postscript_name,
        })
}

fn points_to_pixels_for_dpi(points: f32, dpi: f32) -> f32 {
    (points * dpi / 72.0).round()
}

fn fallback_frame_res_y(display_height_px: i32, display_height_mm: i32) -> f32 {
    if display_height_mm < 1 {
        100.0
    } else {
        display_height_px as f32 * 25.4 / display_height_mm as f32
    }
}

/// Get the effective DPI for font sizing.
///
/// Mirrors GNU Emacs `xterm.c`:
/// - read `Xft.dpi` from X resources via `XGetDefault`
/// - otherwise fall back to display-height / display-height-mm
/// - if the X server reports bogus mm dimensions, fall back to 100 DPI
pub fn xft_dpi() -> f32 {
    *XFT_DPI.get_or_init(|| {
        let dpi = query_xft_dpi().unwrap_or(100.0);
        tracing::info!("Xft.dpi: {}", dpi);
        dpi
    })
}

/// Convert a point size to pixels using GNU Emacs' X11 rule.
pub fn points_to_pixels(points: f32) -> f32 {
    points_to_pixels_for_dpi(points, xft_dpi())
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
    prefer_monospace: bool,
    requested_weight: u16,
    italic: bool,
    spec: &StoredFontSpec,
) -> Option<FontMatch> {
    let effective_weight = spec
        .weight
        .map(|weight| weight.0)
        .unwrap_or(requested_weight);
    let query_charset_ranges = query_charset_ranges(spec, ch);
    let registry_lang = spec
        .registry
        .as_deref()
        .and_then(registry_hint)
        .and_then(|hint| hint.lang)
        .map(str::to_string);
    let query_langs = combined_query_langs(registry_lang.as_deref(), spec.lang.as_deref());
    let requested_spacing = requested_spacing(spec);

    for family_option in family_search_order(requested_family, spec) {
        let candidates = fc_list_candidates(
            family_option.as_deref(),
            &query_charset_ranges,
            Some(ch as u32),
            &query_langs,
        );
        if candidates.is_empty() {
            continue;
        }

        let best = best_candidate_for_pass(
            candidates,
            effective_weight,
            italic,
            spec.slant,
            requested_spacing,
            prefer_monospace,
            family_option.as_deref(),
        )?;
        return Some(best);
    }

    None
}

fn representative_char_for_spec(spec: &StoredFontSpec) -> char {
    spec.registry
        .as_deref()
        .and_then(|registry| registry_query_chars(Some(registry), 'a').into_iter().next())
        .and_then(char::from_u32)
        .unwrap_or('a')
}

fn candidate_matches_find_font_spec(candidate: &ListedFont, spec: &StoredFontSpec) -> bool {
    if let Some(weight) = spec.weight
        && candidate
            .weight_css
            .or_else(|| style_weight(&candidate.style))
            != Some(weight.0)
    {
        return false;
    }

    if let Some(slant) = spec.slant
        && candidate.matched.slant != slant
    {
        return false;
    }

    true
}

fn query_charset_ranges(spec: &StoredFontSpec, ch: char) -> Vec<(u32, u32)> {
    if let Some(registry) = spec.registry.as_deref() {
        if ftfont_registry_uses_unconstrained_charset(registry) {
            return Vec::new();
        }

        let mut codepoints = registry_query_chars(Some(registry), ch);
        if !codepoints.contains(&(ch as u32)) {
            codepoints.push(ch as u32);
        }
        return coalesce_ranges(
            codepoints
                .into_iter()
                .map(|codepoint| (codepoint, codepoint))
                .collect(),
        );
    }

    if let Some(mut ranges) = spec
        .repertory
        .as_ref()
        .and_then(repertory_target_ranges)
        .filter(|ranges| !ranges.is_empty())
    {
        ranges.push((ch as u32, ch as u32));
        return coalesce_ranges(ranges);
    }

    Vec::new()
}

fn coalesce_ranges(mut ranges: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    if ranges.is_empty() {
        return ranges;
    }

    ranges.sort_unstable_by_key(|(from, to)| (*from, *to));
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());

    for (from, to) in ranges {
        let from = from.min(to);
        let to = from.max(to);
        match merged.last_mut() {
            Some((current_from, current_to)) if from <= current_to.saturating_add(1) => {
                *current_from = (*current_from).min(from);
                *current_to = (*current_to).max(to);
            }
            _ => merged.push((from, to)),
        }
    }

    merged
}

fn best_candidate_for_pass(
    candidates: Vec<ListedFont>,
    requested_weight: u16,
    italic: bool,
    requested_slant: Option<neovm_core::face::FontSlant>,
    requested_spacing: Option<i32>,
    prefer_monospace: bool,
    queried_family: Option<&str>,
) -> Option<FontMatch> {
    let mut family_best: HashMap<(String, Option<String>), (usize, u32, FontMatch)> =
        HashMap::new();

    for (ordinal, candidate) in candidates.into_iter().enumerate() {
        let score = candidate_score(
            &candidate,
            requested_weight,
            italic,
            requested_slant,
            requested_spacing,
            prefer_monospace,
            queried_family,
        );
        let key = (
            candidate.matched.family.clone(),
            candidate.matched.file.clone(),
        );
        let matched = candidate.matched;
        match family_best.get_mut(&key) {
            Some((_, best_score, best_match)) if score < *best_score => {
                *best_score = score;
                *best_match = matched;
            }
            Some(_) => {}
            None => {
                family_best.insert(key, (ordinal, score, matched));
            }
        }
    }

    family_best
        .into_values()
        .min_by_key(|(ordinal, score, _)| (*score, *ordinal))
        .map(|(_, _, matched)| matched)
}

fn family_search_order(requested_family: &str, spec: &StoredFontSpec) -> Vec<Option<String>> {
    if let Some(spec_family) = spec.family.as_deref() {
        return vec![Some(resolve_family(spec_family).to_string())];
    }

    if requested_family.is_empty() {
        return vec![None];
    }

    // GNU font_find_for_lface consults face-alternative-font-family-alist
    // before retrying with an unspecified family.
    let mut order = Vec::new();
    for family in alternative_font_families(requested_family) {
        let resolved = resolve_family(&family);
        if resolved == family {
            order.push(Some(family));
        } else {
            order.push(Some(resolved.to_string()));
            order.push(Some(family));
        }
    }
    order.push(None);
    order
}

fn candidate_score(
    candidate: &ListedFont,
    requested_weight: u16,
    italic: bool,
    requested_slant: Option<neovm_core::face::FontSlant>,
    requested_spacing: Option<i32>,
    prefer_monospace: bool,
    queried_family: Option<&str>,
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

    let mut score = spacing_score(requested_spacing, candidate.spacing, prefer_monospace);
    score += family_affinity_score(queried_family, &candidate.matched.family);
    score += u32::from(candidate_weight.abs_diff(requested_weight));
    score += slant_distance(requested_slant, candidate_slant);
    score
}

fn requested_spacing(_spec: &StoredFontSpec) -> Option<i32> {
    // GNU ftfont only filters by FC_SPACING when the font-spec itself requests
    // spacing.  Inferring "monospace" from the current face family over-weights
    // Fontconfig spacing metadata and makes variable CJK companions lose to
    // unrelated fixed-pitch families.
    None
}

fn spacing_score(
    requested_spacing: Option<i32>,
    candidate_spacing: Option<i32>,
    prefer_monospace: bool,
) -> u32 {
    let requested = requested_spacing.and_then(normalize_spacing);
    let candidate = candidate_spacing.and_then(normalize_spacing);

    match (requested, candidate) {
        (Some(requested), Some(candidate)) if requested == candidate => 0,
        (Some(SpacingClass::Mono | SpacingClass::Charcell), Some(SpacingClass::Dual))
            if prefer_monospace =>
        {
            25
        }
        (Some(SpacingClass::Dual), Some(SpacingClass::Mono | SpacingClass::Charcell))
            if prefer_monospace =>
        {
            25
        }
        (Some(_), None) if prefer_monospace => 800,
        (Some(requested), Some(candidate)) => spacing_distance(requested, candidate),
        _ => 0,
    }
}

fn family_affinity_score(queried_family: Option<&str>, candidate_family: &str) -> u32 {
    let Some(queried_family) = queried_family.filter(|family| !family.is_empty()) else {
        return 0;
    };

    let queried = queried_family.to_ascii_lowercase();
    let candidate = candidate_family.to_ascii_lowercase();

    if candidate == queried {
        return 0;
    }

    if candidate.starts_with(&queried) || queried.starts_with(&candidate) {
        return 5;
    }

    if candidate.contains(&queried) || queried.contains(&candidate) {
        return 15;
    }

    80
}

fn normalize_spacing(spacing: i32) -> Option<SpacingClass> {
    match spacing {
        i32::MIN..=-1 => None,
        FONT_SPACING_PROPORTIONAL..=89 => Some(SpacingClass::Proportional),
        FONT_SPACING_DUAL..=99 => Some(SpacingClass::Dual),
        FONT_SPACING_MONO..=FONT_SPACING_MONO_MAX => Some(SpacingClass::Mono),
        _ => Some(SpacingClass::Charcell),
    }
}

fn spacing_distance(requested: SpacingClass, candidate: SpacingClass) -> u32 {
    use SpacingClass::{Charcell, Dual, Mono, Proportional};

    match (requested, candidate) {
        (Proportional, Dual) | (Dual, Proportional) => 500,
        (Proportional, Mono) | (Mono, Proportional) => 800,
        (Proportional, Charcell) | (Charcell, Proportional) => 1_000,
        (Dual, Mono) | (Mono, Dual) => 200,
        (Dual, Charcell) | (Charcell, Dual) => 250,
        (Mono, Charcell) | (Charcell, Mono) => 100,
        _ => 0,
    }
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
struct FcPatternGuard(*mut fontconfig_sys::FcPattern);

#[cfg(unix)]
impl Drop for FcPatternGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { fontconfig_sys::FcPatternDestroy(self.0) };
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
struct FcLangSetGuard(*mut fontconfig_sys::FcLangSet);

#[cfg(unix)]
impl Drop for FcLangSetGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { fontconfig_sys::FcLangSetDestroy(self.0) };
        }
    }
}

#[cfg(unix)]
struct FcCharSetGuard(*mut fontconfig_sys::FcCharSet);

#[cfg(unix)]
impl Drop for FcCharSetGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { fontconfig_sys::FcCharSetDestroy(self.0) };
        }
    }
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
fn build_candidate_object_set(include_charset: bool) -> Option<FcObjectSetGuard> {
    let object_set = unsafe { fontconfig_sys::FcObjectSetCreate() };
    if object_set.is_null() {
        return None;
    }
    let guard = FcObjectSetGuard(object_set);
    let keys: [&CStr; 15] = [
        fontconfig::FC_FOUNDRY,
        fontconfig::FC_FAMILY,
        fontconfig::FC_WEIGHT,
        fontconfig::FC_SLANT,
        fontconfig::FC_WIDTH,
        fontconfig::FC_PIXEL_SIZE,
        fontconfig::FC_SPACING,
        fontconfig::FC_SCALABLE,
        fontconfig::FC_STYLE,
        fontconfig::FC_FILE,
        fontconfig::FC_INDEX,
        fontconfig_sys::constants::FC_CAPABILITY,
        fontconfig::FC_FONTFORMAT,
        fontconfig::FC_COLOR,
        fontconfig::FC_VARIABLE,
    ];
    for key in keys {
        let ok = unsafe { fontconfig_sys::FcObjectSetAdd(object_set, key.as_ptr()) };
        if ok == 0 {
            return None;
        }
    }
    if include_charset {
        let ok =
            unsafe { fontconfig_sys::FcObjectSetAdd(object_set, fontconfig::FC_CHARSET.as_ptr()) };
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
fn raw_pattern_bool(pattern: *mut fontconfig_sys::FcPattern, key: &CStr) -> Option<bool> {
    let mut value = 0;
    let result = unsafe { fontconfig_sys::FcPatternGetBool(pattern, key.as_ptr(), 0, &mut value) };
    (result == fontconfig_sys::FcResultMatch).then_some(value != 0)
}

#[cfg(unix)]
fn raw_pattern_supports_any_char(
    pattern: *mut fontconfig_sys::FcPattern,
    query_chars: &[u32],
) -> bool {
    if query_chars.is_empty() {
        return true;
    }
    let mut charset: *mut fontconfig_sys::FcCharSet = ptr::null_mut();
    let result = unsafe {
        fontconfig_sys::FcPatternGetCharSet(
            pattern,
            fontconfig::FC_CHARSET.as_ptr(),
            0,
            &mut charset,
        )
    };
    if result != fontconfig_sys::FcResultMatch || charset.is_null() {
        return false;
    }
    query_chars.iter().any(|&codepoint| unsafe {
        fontconfig_sys::FcCharSetHasChar(charset, codepoint as fontconfig_sys::FcChar32) != 0
    })
}

#[cfg(unix)]
fn listed_font_from_raw_pattern(pattern: *mut fontconfig_sys::FcPattern) -> Option<ListedFont> {
    if pattern.is_null() {
        return None;
    }
    let file = raw_pattern_string(pattern, fontconfig::FC_FILE)?;
    let _index = raw_pattern_int(pattern, fontconfig::FC_INDEX)?;
    if raw_pattern_bool(pattern, fontconfig::FC_VARIABLE).unwrap_or(false)
        && raw_pattern_int(pattern, fontconfig::FC_WEIGHT).is_none()
    {
        return None;
    }
    let style = raw_pattern_string(pattern, fontconfig::FC_STYLE).unwrap_or_default();
    let weight_css = raw_pattern_int(pattern, fontconfig::FC_WEIGHT).map(map_fontconfig_weight_raw);
    let spacing = raw_pattern_int(pattern, fontconfig::FC_SPACING);
    let matched_family = raw_pattern_string(pattern, fontconfig::FC_FAMILY)?;
    Some(ListedFont {
        matched: FontMatch {
            family: matched_family,
            file: Some(file),
            postscript_name: raw_pattern_string(pattern, fontconfig::FC_POSTSCRIPT_NAME),
            weight: weight_css,
            slant: raw_pattern_int(pattern, fontconfig::FC_SLANT)
                .map(map_fontconfig_slant_raw)
                .unwrap_or_else(|| style_slant(&style)),
        },
        style,
        weight_css,
        width: raw_pattern_int(pattern, fontconfig::FC_WIDTH).map(map_fontconfig_width_raw),
        spacing,
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
        205..=209 => 800,
        210..=212 => 900,
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
fn map_fontconfig_width_raw(width: i32) -> FontWidth {
    match width {
        i32::MIN..=56 => FontWidth::UltraCondensed,
        57..=69 => FontWidth::ExtraCondensed,
        70..=81 => FontWidth::Condensed,
        82..=93 => FontWidth::SemiCondensed,
        94..=106 => FontWidth::Normal,
        107..=119 => FontWidth::SemiExpanded,
        120..=137 => FontWidth::Expanded,
        138..=175 => FontWidth::ExtraExpanded,
        _ => FontWidth::UltraExpanded,
    }
}

#[cfg(unix)]
fn fc_list_candidates(
    family: Option<&str>,
    query_charset_ranges: &[(u32, u32)],
    required_char: Option<u32>,
    langs: &[String],
) -> Vec<ListedFont> {
    if fontconfig_handle().is_none() {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    let query_langs = if langs.is_empty() {
        vec![None]
    } else {
        langs.iter().map(|lang| Some(lang.as_str())).collect()
    };

    for lang in query_langs {
        let pattern = unsafe { fontconfig_sys::FcPatternCreate() };
        if pattern.is_null() {
            continue;
        }
        let pattern = FcPatternGuard(pattern);
        if let Some(family) = family.filter(|family| !family.is_empty()) {
            let Ok(value) = CString::new(family) else {
                continue;
            };
            let ok = unsafe {
                fontconfig_sys::FcPatternAddString(
                    pattern.0,
                    fontconfig::FC_FAMILY.as_ptr(),
                    value.as_ptr().cast(),
                )
            };
            if ok == 0 {
                continue;
            }
        }
        let query_charset = if query_charset_ranges.is_empty() {
            None
        } else {
            let charset = unsafe { fontconfig_sys::FcCharSetCreate() };
            if charset.is_null() {
                continue;
            }
            let charset = FcCharSetGuard(charset);
            let mut ok = true;
            for &(from, to) in query_charset_ranges {
                for codepoint in from.min(to)..=from.max(to) {
                    let added = unsafe {
                        fontconfig_sys::FcCharSetAddChar(
                            charset.0,
                            codepoint as fontconfig_sys::FcChar32,
                        )
                    };
                    if added == 0 {
                        ok = false;
                        break;
                    }
                }
                if !ok {
                    break;
                }
            }
            if !ok {
                continue;
            }
            let added = unsafe {
                fontconfig_sys::FcPatternAddCharSet(
                    pattern.0,
                    fontconfig::FC_CHARSET.as_ptr(),
                    charset.0,
                )
            };
            if added == 0 {
                continue;
            }
            Some(charset)
        };
        let query_langset = if let Some(lang) = lang.filter(|lang| !lang.is_empty()) {
            let Ok(lang) = CString::new(lang) else {
                continue;
            };
            let langset = unsafe { fontconfig_sys::FcLangSetCreate() };
            if langset.is_null() {
                continue;
            }
            let langset = FcLangSetGuard(langset);
            let added = unsafe { fontconfig_sys::FcLangSetAdd(langset.0, lang.as_ptr().cast()) };
            if added == 0 {
                continue;
            }
            let added = unsafe {
                fontconfig_sys::FcPatternAddLangSet(
                    pattern.0,
                    fontconfig::FC_LANG.as_ptr(),
                    langset.0,
                )
            };
            if added == 0 {
                continue;
            }
            Some(langset)
        } else {
            None
        };
        let _keep_charset_alive = query_charset;
        let _keep_langset_alive = query_langset;
        let include_charset = required_char.is_some() && query_charset_ranges.is_empty();
        let Some(object_set) = build_candidate_object_set(include_charset) else {
            continue;
        };
        let fontset = unsafe {
            FcFontSetGuard(fontconfig_sys::FcFontList(
                ptr::null_mut(),
                pattern.0,
                object_set.0,
            ))
        };
        if fontset.0.is_null() {
            continue;
        }
        let nfont = unsafe { (*fontset.0).nfont };
        tracing::trace!(
            family = family.unwrap_or(""),
            lang = lang.unwrap_or(""),
            query_charset_ranges = ?query_charset_ranges,
            required_char,
            nfont,
            "fontconfig raw candidate list"
        );
        if nfont <= 0 {
            continue;
        }
        let fonts = unsafe { (*fontset.0).fonts };
        if fonts.is_null() {
            continue;
        }
        let patterns = unsafe { std::slice::from_raw_parts(fonts, nfont as usize) };
        let before = candidates.len();

        for &candidate_pattern in patterns {
            if let Some(required_char) = required_char
                && !raw_pattern_supports_any_char(candidate_pattern, &[required_char])
            {
                continue;
            }
            let Some(candidate) = listed_font_from_raw_pattern(candidate_pattern) else {
                continue;
            };
            candidates.push(candidate);
        }
        tracing::trace!(
            family = family.unwrap_or(""),
            lang = lang.unwrap_or(""),
            query_charset_ranges = ?query_charset_ranges,
            required_char,
            added = candidates.len().saturating_sub(before),
            total = candidates.len(),
            "fontconfig filtered candidate list"
        );
    }
    candidates
}

#[cfg(not(unix))]
fn fc_list_candidates(
    family: Option<&str>,
    query_charset_ranges: &[(u32, u32)],
    required_char: Option<u32>,
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
        let mut all_query_chars = Vec::new();
        for &(from, to) in query_charset_ranges {
            all_query_chars.extend(from.min(to)..=from.max(to));
        }
        if let Some(required_char) = required_char
            && !all_query_chars.contains(&required_char)
        {
            all_query_chars.push(required_char);
        }
        for (index, codepoint) in all_query_chars.iter().enumerate() {
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
            .arg(
                "--format=%{family}\t%{style}\t%{weight}\t%{spacing}\t%{postscriptname}\t%{file}\n",
            )
            .output()
        {
            Ok(output) if output.status.success() => output,
            _ => continue,
        };

        let Ok(stdout) = String::from_utf8(output.stdout) else {
            continue;
        };

        for line in stdout.lines() {
            let mut fields = line.splitn(6, '\t');
            let Some(families) = fields.next() else {
                continue;
            };
            let style = fields.next().unwrap_or_default().trim().to_string();
            let weight_css = parse_fontconfig_weight(fields.next().unwrap_or_default());
            let spacing = fields
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .and_then(|value| value.parse::<i32>().ok());
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
                width: None,
                spacing,
            };
            candidates.push(candidate);
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
        205..=209 => 800,
        210..=212 => 900,
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

fn ftfont_registry_uses_unconstrained_charset(registry: &str) -> bool {
    matches!(
        registry.trim().to_ascii_lowercase().as_str(),
        "ascii-0" | "iso10646-1" | "unicode-bmp"
    )
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

#[cfg(unix)]
/// Query `Xft.dpi` from the active X display, mirroring GNU Emacs `xterm.c`.
///
/// Runs `XOpenDisplay` in a background thread with a timeout to avoid blocking
/// indefinitely if the X server is unresponsive (stale socket, broken display).
fn query_xft_dpi() -> Option<f32> {
    // Skip X11 query entirely in batch/noninteractive mode or when DISPLAY is unset.
    if std::env::var("DISPLAY").unwrap_or_default().is_empty() {
        return None;
    }

    // XOpenDisplay can block indefinitely on a broken X server (stale socket
    // at /tmp/.X11-unix/X0 that never responds to the handshake).
    // Run it in a background thread with a timeout to avoid hanging startup.
    let (tx, rx) = std::sync::mpsc::channel();
    let _handle = std::thread::Builder::new()
        .name("xft-dpi-probe".into())
        .spawn(move || {
            let result = query_xft_dpi_inner();
            let _ = tx.send(result);
        });
    // Wait at most 3 seconds for the X server to respond.
    match rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!(
                "query_xft_dpi: X11 connection timed out (broken display?), using fallback DPI"
            );
            None
        }
    }
}

#[cfg(unix)]
fn query_xft_dpi_inner() -> Option<f32> {
    let xlib = xlib::Xlib::open().ok()?;
    let display = unsafe { (xlib.XOpenDisplay)(ptr::null()) };
    if display.is_null() {
        return None;
    }

    let class = CString::new("Xft").ok()?;
    let name = CString::new("dpi").ok()?;

    let dpi = unsafe {
        let resource = (xlib.XGetDefault)(display, class.as_ptr(), name.as_ptr());
        let parsed = if resource.is_null() {
            None
        } else {
            CStr::from_ptr(resource)
                .to_str()
                .ok()
                .and_then(|s| s.trim().parse::<f32>().ok())
        };

        match parsed {
            Some(dpi) if dpi.is_finite() && dpi > 0.0 => Some(dpi),
            _ => {
                let screen = (xlib.XDefaultScreen)(display);
                let pixels = (xlib.XDisplayHeight)(display, screen);
                let mm = (xlib.XDisplayHeightMM)(display, screen);
                Some(fallback_frame_res_y(pixels, mm))
            }
        }
    };

    unsafe {
        (xlib.XCloseDisplay)(display);
    }

    dpi
}

#[cfg(not(unix))]
fn query_xft_dpi() -> Option<f32> {
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

pub fn default_subpixel_order() -> FontconfigSubpixelOrder {
    *FC_RGBA_CACHE.get_or_init(query_default_subpixel_order)
}

#[cfg(unix)]
fn query_default_subpixel_order() -> FontconfigSubpixelOrder {
    let Some(fc) = fontconfig_handle() else {
        return FontconfigSubpixelOrder::Unknown;
    };

    let mut pattern = Pattern::new(fc);
    if !add_string_property(&mut pattern, fontconfig::FC_FAMILY, "Monospace") {
        return FontconfigSubpixelOrder::Unknown;
    }
    let matched = pattern.font_match();
    match matched.get_int(FC_RGBA) {
        Some(FC_RGBA_NONE) => FontconfigSubpixelOrder::None,
        Some(FC_RGBA_RGB) => FontconfigSubpixelOrder::Rgb,
        Some(FC_RGBA_BGR) => FontconfigSubpixelOrder::Bgr,
        Some(FC_RGBA_VRGB) => FontconfigSubpixelOrder::VRgb,
        Some(FC_RGBA_VBGR) => FontconfigSubpixelOrder::VBgr,
        _ => FontconfigSubpixelOrder::Unknown,
    }
}

#[cfg(not(unix))]
fn query_default_subpixel_order() -> FontconfigSubpixelOrder {
    FontconfigSubpixelOrder::Unknown
}

#[cfg(test)]
mod tests {
    use super::{
        FONT_SPACING_MONO, FONT_SPACING_PROPORTIONAL, FcCharSetGuard, FcFontSetGuard,
        FcLangSetGuard, FcPatternGuard, ListedFont, SpacingClass, build_candidate_object_set,
        candidate_score, combined_query_langs, fallback_frame_res_y, family_affinity_score,
        family_search_order, fc_list_candidates, fontconfig_handle, listed_font_from_raw_pattern,
        normalize_spacing, parse_fontconfig_weight, points_to_pixels_for_dpi, query_charset_ranges,
        registry_hint, registry_query_chars, representative_char_for_spec, spacing_score,
        style_weight, wildcard_casefold_match,
    };
    use neovm_core::emacs_core::fontset::{FontRepertory, StoredFontSpec};
    use neovm_core::face::{FontSlant, FontWeight, FontWidth};
    #[cfg(unix)]
    use std::ffi::CString;
    #[cfg(unix)]
    use std::ptr;

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
    fn query_charset_ranges_follow_gnu_registry_uniquifiers_when_registry_is_present() {
        let spec = StoredFontSpec {
            family: None,
            registry: Some("gb2312.1980-0".to_string()),
            lang: None,
            weight: None,
            slant: None,
            width: None,
            repertory: Some(FontRepertory::CharTableRanges(vec![(0x4E00, 0x9FFF)])),
        };

        assert_eq!(
            query_charset_ranges(&spec, '好'),
            vec![(0x4E13, 0x4E13), ('好' as u32, '好' as u32)]
        );
    }

    #[test]
    fn query_charset_ranges_use_repertory_when_registry_is_absent() {
        let spec = StoredFontSpec {
            family: None,
            registry: None,
            lang: None,
            weight: None,
            slant: None,
            width: None,
            repertory: Some(FontRepertory::CharTableRanges(vec![(0x4E00, 0x9FFF)])),
        };

        assert_eq!(query_charset_ranges(&spec, '好'), vec![(0x4E00, 0x9FFF)]);
    }

    #[test]
    fn query_charset_ranges_skip_generic_gnu_registries() {
        for registry in ["ascii-0", "iso10646-1", "unicode-bmp"] {
            let spec = StoredFontSpec {
                family: None,
                registry: Some(registry.to_string()),
                lang: None,
                weight: None,
                slant: None,
                width: None,
                repertory: None,
            };
            assert!(query_charset_ranges(&spec, '好').is_empty(), "{registry}");
        }
    }

    #[cfg(unix)]
    fn gb2312_registry_pattern() -> (
        FcPatternGuard,
        FcCharSetGuard,
        FcLangSetGuard,
        Vec<String>,
        Vec<(u32, u32)>,
    ) {
        let spec = StoredFontSpec {
            family: None,
            registry: Some("gb2312.1980-0".to_string()),
            lang: None,
            weight: None,
            slant: None,
            width: None,
            repertory: None,
        };
        let representative = representative_char_for_spec(&spec);
        let ranges = query_charset_ranges(&spec, representative);
        let langs = combined_query_langs(
            spec.registry
                .as_deref()
                .and_then(registry_hint)
                .and_then(|hint| hint.lang),
            spec.lang.as_deref(),
        );

        let _ = fontconfig_handle().expect("fontconfig");
        let pattern = unsafe { fontconfig_sys::FcPatternCreate() };
        assert!(!pattern.is_null());
        let pattern = FcPatternGuard(pattern);
        let charset = unsafe { fontconfig_sys::FcCharSetCreate() };
        assert!(!charset.is_null());
        let charset = FcCharSetGuard(charset);
        let langset = unsafe { fontconfig_sys::FcLangSetCreate() };
        assert!(!langset.is_null());
        let langset = FcLangSetGuard(langset);
        assert_ne!(
            unsafe { fontconfig_sys::FcCharSetAddChar(charset.0, 0x4E13) },
            0
        );
        let lang = CString::new("zh-cn").unwrap();
        assert_ne!(
            unsafe { fontconfig_sys::FcLangSetAdd(langset.0, lang.as_ptr().cast()) },
            0
        );
        assert_ne!(
            unsafe {
                fontconfig_sys::FcPatternAddCharSet(
                    pattern.0,
                    fontconfig::FC_CHARSET.as_ptr(),
                    charset.0,
                )
            },
            0
        );
        assert_ne!(
            unsafe {
                fontconfig_sys::FcPatternAddLangSet(
                    pattern.0,
                    fontconfig::FC_LANG.as_ptr(),
                    langset.0,
                )
            },
            0
        );
        (pattern, charset, langset, langs, ranges)
    }

    #[cfg(unix)]
    #[test]
    fn registry_charset_queries_keep_gnu_fontconfig_candidate_order() {
        let (pattern, _charset, _langset, langs, ranges) = gb2312_registry_pattern();
        let object_set = build_candidate_object_set(false).expect("gnu object set");
        let fontset =
            unsafe { fontconfig_sys::FcFontList(ptr::null_mut(), pattern.0, object_set.0) };
        assert!(!fontset.is_null());
        let fontset = FcFontSetGuard(fontset);
        let fonts =
            unsafe { std::slice::from_raw_parts((*fontset.0).fonts, (*fontset.0).nfont as usize) };
        let expected: Vec<(String, String)> = fonts
            .iter()
            .take(8)
            .filter_map(|pattern| listed_font_from_raw_pattern(*pattern))
            .map(|candidate| (candidate.matched.family, candidate.style))
            .collect();
        assert!(
            !expected.is_empty(),
            "raw GNU-shaped fontconfig query should yield candidates"
        );

        let actual: Vec<(String, String)> = fc_list_candidates(None, &ranges, None, &langs)
            .into_iter()
            .take(expected.len())
            .map(|candidate| (candidate.matched.family, candidate.style))
            .collect();

        assert_eq!(actual, expected);
    }

    #[test]
    fn spacing_score_is_neutral_without_requested_spacing() {
        assert_eq!(
            spacing_score(None, Some(FONT_SPACING_PROPORTIONAL), true),
            0
        );
        assert_eq!(spacing_score(None, Some(FONT_SPACING_MONO), true), 0);
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
    fn registry_only_fontset_specs_try_requested_family_before_unspecified_fallback() {
        let spec = StoredFontSpec {
            family: None,
            registry: Some("gb2312.1980-0".to_string()),
            lang: None,
            weight: None,
            slant: None,
            width: None,
            repertory: None,
        };
        let order = family_search_order("monospace", &spec);
        assert!(matches!(order.first(), Some(Some(_))));
        assert!(matches!(order.last(), Some(None)));
    }

    #[test]
    fn constrained_fontset_specs_without_family_try_requested_family_first() {
        let spec = StoredFontSpec {
            family: None,
            registry: None,
            lang: Some("zh-cn".to_string()),
            weight: Some(FontWeight(600)),
            slant: None,
            width: None,
            repertory: None,
        };
        let order = family_search_order("monospace", &spec);
        assert!(matches!(order.first(), Some(Some(_))));
        assert!(matches!(order.last(), Some(None)));
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
        assert_eq!(parse_fontconfig_weight("205"), Some(800));
        assert_eq!(parse_fontconfig_weight("210"), Some(900));
        assert_eq!(parse_fontconfig_weight("[80 200]"), None);
    }

    #[test]
    fn points_to_pixels_rounds_like_gnu_point_to_pixel() {
        assert_eq!(points_to_pixels_for_dpi(10.0, 100.0), 14.0);
        assert_eq!(points_to_pixels_for_dpi(12.0, 100.0), 17.0);
        assert_eq!(points_to_pixels_for_dpi(16.0, 100.0), 22.0);
    }

    #[test]
    fn frame_res_fallback_uses_display_height_and_mm() {
        let dpi = fallback_frame_res_y(1080, 274);
        assert!((dpi - 100.14).abs() < 0.1);
        assert_eq!(fallback_frame_res_y(1080, 0), 100.0);
    }

    #[test]
    fn style_weight_prefers_semibold_over_regular_alias() {
        assert_eq!(style_weight("SemiBold,Regular"), Some(600));
        assert_eq!(style_weight("SemiBold Italic,Italic"), Some(600));
    }

    #[test]
    fn spacing_categories_follow_gnu_numeric_ranges() {
        assert_eq!(normalize_spacing(0), Some(SpacingClass::Proportional));
        assert_eq!(normalize_spacing(90), Some(SpacingClass::Dual));
        assert_eq!(normalize_spacing(100), Some(SpacingClass::Mono));
        assert_eq!(normalize_spacing(110), Some(SpacingClass::Charcell));
    }

    #[test]
    fn monospace_preference_penalizes_proportional_candidates() {
        let mono_candidate = ListedFont {
            matched: super::FontMatch {
                family: "Noto Sans Mono CJK SC".to_string(),
                file: None,
                postscript_name: None,
                weight: Some(700),
                slant: FontSlant::Normal,
            },
            style: "Bold".to_string(),
            weight_css: Some(700),
            width: Some(FontWidth::Normal),
            spacing: Some(FONT_SPACING_MONO),
        };
        let proportional_candidate = ListedFont {
            matched: super::FontMatch {
                family: "Noto Sans CJK SC".to_string(),
                file: None,
                postscript_name: None,
                weight: Some(700),
                slant: FontSlant::Normal,
            },
            style: "Bold".to_string(),
            weight_css: Some(700),
            width: Some(FontWidth::Normal),
            spacing: Some(FONT_SPACING_PROPORTIONAL),
        };

        let mono_score = candidate_score(
            &mono_candidate,
            800,
            false,
            None,
            Some(FONT_SPACING_MONO),
            true,
            Some("Noto Sans Mono"),
        );
        let proportional_score = candidate_score(
            &proportional_candidate,
            800,
            false,
            None,
            Some(FONT_SPACING_MONO),
            true,
            Some("Noto Sans Mono"),
        );

        assert!(
            mono_score < proportional_score,
            "expected mono candidate to outrank proportional candidate: mono={mono_score} proportional={proportional_score}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn family_affinity_prefers_requested_family_over_unrelated_fixed_pitch_fallback() {
        let requested_family_candidate = ListedFont {
            matched: super::FontMatch {
                family: "Noto Sans Mono CJK SC".to_string(),
                file: None,
                postscript_name: None,
                weight: Some(400),
                slant: FontSlant::Normal,
            },
            style: "Regular".to_string(),
            weight_css: Some(400),
            width: Some(FontWidth::Normal),
            spacing: None,
        };
        let unrelated_fixed_candidate = ListedFont {
            matched: super::FontMatch {
                family: "Sarasa Fixed Slab SC".to_string(),
                file: None,
                postscript_name: None,
                weight: Some(400),
                slant: FontSlant::Normal,
            },
            style: "Regular".to_string(),
            weight_css: Some(400),
            width: Some(FontWidth::Normal),
            spacing: Some(90),
        };

        let requested_score = candidate_score(
            &requested_family_candidate,
            400,
            false,
            None,
            None,
            true,
            Some("Noto Sans Mono"),
        );
        let fallback_score = candidate_score(
            &unrelated_fixed_candidate,
            400,
            false,
            None,
            None,
            true,
            Some("Noto Sans Mono"),
        );

        assert!(
            requested_score < fallback_score,
            "expected requested-family CJK companion to outrank unrelated fixed fallback: requested={requested_score} fallback={fallback_score}"
        );
    }

    #[test]
    fn family_affinity_treats_cjk_companion_as_close_match() {
        assert_eq!(
            family_affinity_score(Some("Noto Sans Mono"), "Noto Sans Mono CJK SC"),
            5
        );
        assert_eq!(
            family_affinity_score(Some("Noto Sans Mono"), "Sarasa Fixed SC"),
            80
        );
    }

    #[test]
    fn best_candidate_for_pass_prefers_first_family_when_later_style_matches_catch_up() {
        let candidates = vec![
            ListedFont {
                matched: super::FontMatch {
                    family: "Noto Sans Mono CJK SC".to_string(),
                    file: Some("mono.ttc".to_string()),
                    postscript_name: Some("Mono-Medium".to_string()),
                    weight: Some(500),
                    slant: FontSlant::Normal,
                },
                style: "Medium".to_string(),
                weight_css: Some(500),
                width: Some(FontWidth::Normal),
                spacing: None,
            },
            ListedFont {
                matched: super::FontMatch {
                    family: "Noto Sans CJK JP".to_string(),
                    file: Some("sans.ttc".to_string()),
                    postscript_name: Some("Sans-Regular".to_string()),
                    weight: Some(400),
                    slant: FontSlant::Normal,
                },
                style: "Regular".to_string(),
                weight_css: Some(400),
                width: Some(FontWidth::Normal),
                spacing: None,
            },
            ListedFont {
                matched: super::FontMatch {
                    family: "Noto Sans Mono CJK SC".to_string(),
                    file: Some("mono.ttc".to_string()),
                    postscript_name: Some("Mono-Regular".to_string()),
                    weight: Some(400),
                    slant: FontSlant::Normal,
                },
                style: "Regular".to_string(),
                weight_css: Some(400),
                width: Some(FontWidth::Normal),
                spacing: None,
            },
        ];

        let matched =
            super::best_candidate_for_pass(candidates, 400, false, None, None, true, None)
                .expect("best candidate");
        assert_eq!(matched.family, "Noto Sans Mono CJK SC");
        assert_eq!(matched.postscript_name.as_deref(), Some("Mono-Regular"));
    }

    #[cfg(unix)]
    #[test]
    fn fc_list_candidates_tolerates_empty_fontsets() {
        let _ = fc_list_candidates(
            Some("definitely-missing-neomacs-font-family"),
            &[(0x10FFFF, 0x10FFFF)],
            Some(0x10FFFF),
            &[String::from("zz-zz")],
        );
    }

    #[cfg(unix)]
    #[test]
    fn fontconfig_handle_initializes() {
        assert!(super::fontconfig_handle().is_some(), "fontconfig handle");
    }

    #[test]
    fn default_subpixel_order_resolves_to_known_variant() {
        assert!(matches!(
            super::default_subpixel_order(),
            super::FontconfigSubpixelOrder::Unknown
                | super::FontconfigSubpixelOrder::None
                | super::FontconfigSubpixelOrder::Rgb
                | super::FontconfigSubpixelOrder::Bgr
                | super::FontconfigSubpixelOrder::VRgb
                | super::FontconfigSubpixelOrder::VBgr
        ));
    }
}
