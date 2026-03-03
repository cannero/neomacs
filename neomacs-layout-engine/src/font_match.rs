//! Shared font matching helpers.
//!
//! Keeps layout and rasterization behavior consistent.

use cosmic_text::FontSystem;
use fontdb::{Database, Style as DbStyle};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::RwLock;
use ttf_parser::{Face as TtfFace, Tag};

#[derive(Clone, Debug, Default)]
struct FamilyWeightInfo {
    discrete_weights: Vec<u16>,
    variable_weight_range: Option<(u16, u16)>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum StyleKey {
    Normal,
    Italic,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CacheKey {
    db_len: usize,
    family_lower: String,
    style: StyleKey,
}

// Cache family/style weight support because querying/parsing font metadata is expensive
// when done for every glyph.
static FAMILY_WEIGHT_CACHE: Lazy<RwLock<HashMap<CacheKey, FamilyWeightInfo>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

/// Resolve a requested weight to the closest available weight in the same family.
///
/// For explicit family names, this prevents cross-family jumps when a specific
/// weight instance is missing from the requested family.
pub fn resolve_weight_in_family(
    font_system: &FontSystem,
    family: &str,
    requested_weight: u16,
    italic: bool,
) -> u16 {
    let family_lc = family.to_lowercase();
    if matches!(
        family_lc.as_str(),
        "" | "mono" | "monospace" | "serif" | "sans" | "sansserif" | "sans-serif"
    ) {
        return requested_weight;
    }

    let style = if italic {
        StyleKey::Italic
    } else {
        StyleKey::Normal
    };
    let db = font_system.db();
    let key = CacheKey {
        db_len: db.len(),
        family_lower: family_lc,
        style,
    };

    if let Ok(cache) = FAMILY_WEIGHT_CACHE.read()
        && let Some(info) = cache.get(&key)
    {
        return resolve_requested_weight(info, requested_weight);
    }

    let info = family_weight_info_for_style(
        db,
        family,
        match style {
            StyleKey::Italic => DbStyle::Italic,
            StyleKey::Normal => DbStyle::Normal,
        },
    );
    let resolved = resolve_requested_weight(&info, requested_weight);

    if let Ok(mut cache) = FAMILY_WEIGHT_CACHE.write() {
        cache.insert(key, info);
    }

    resolved
}

fn resolve_requested_weight(info: &FamilyWeightInfo, requested_weight: u16) -> u16 {
    if let Some((min_w, max_w)) = info.variable_weight_range {
        // Variable fonts can synthesize intermediate weights; keep caller intent and
        // only clamp to the axis range.
        return requested_weight.clamp(min_w, max_w);
    }
    if info.discrete_weights.is_empty() {
        return requested_weight;
    }
    pick_nearest_css_weight(&info.discrete_weights, requested_weight)
}

fn family_weight_info_for_style(db: &Database, family: &str, style: DbStyle) -> FamilyWeightInfo {
    let style_pref = match style {
        DbStyle::Italic => [DbStyle::Italic, DbStyle::Oblique, DbStyle::Normal],
        DbStyle::Oblique => [DbStyle::Oblique, DbStyle::Italic, DbStyle::Normal],
        DbStyle::Normal => [DbStyle::Normal, DbStyle::Oblique, DbStyle::Italic],
    };

    for preferred_style in style_pref {
        let matching_faces: Vec<_> = db
            .faces()
            .filter(|face| face.style == preferred_style)
            .filter(|face| {
                face.families
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(family))
            })
            .collect();

        if matching_faces.is_empty() {
            continue;
        }

        let mut discrete_weights: Vec<u16> =
            matching_faces.iter().map(|face| face.weight.0).collect();
        discrete_weights.sort_unstable();
        discrete_weights.dedup();

        let mut variable_weight_range: Option<(u16, u16)> = None;
        for face in matching_faces {
            if let Some((min_w, max_w)) = face_weight_axis_range(db, face.id) {
                variable_weight_range = Some(match variable_weight_range {
                    None => (min_w, max_w),
                    Some((cur_min, cur_max)) => (cur_min.min(min_w), cur_max.max(max_w)),
                });
            }
        }

        return FamilyWeightInfo {
            discrete_weights,
            variable_weight_range,
        };
    }

    FamilyWeightInfo::default()
}

fn face_weight_axis_range(db: &Database, id: fontdb::ID) -> Option<(u16, u16)> {
    db.with_face_data(id, |font_data, face_index| {
        let face = TtfFace::parse(font_data, face_index).ok()?;
        let wght = Tag::from_bytes(b"wght");
        for axis in face.variation_axes() {
            if axis.tag == wght {
                let min_w = axis.min_value.round().clamp(1.0, 1000.0) as u16;
                let max_w = axis.max_value.round().clamp(1.0, 1000.0) as u16;
                return Some((min_w.min(max_w), min_w.max(max_w)));
            }
        }
        None
    })
    .flatten()
}

// Generic same-family weight fallback for static faces:
// 1) exact match when available
// 2) otherwise prefer nearest lower existing weight
// 3) if none lower exists, use the nearest upper existing weight
fn pick_nearest_css_weight(weights: &[u16], requested_weight: u16) -> u16 {
    if weights.contains(&requested_weight) {
        return requested_weight;
    }
    if let Some(w) = weights
        .iter()
        .copied()
        .filter(|w| *w <= requested_weight)
        .max()
    {
        return w;
    }

    weights
        .iter()
        .copied()
        .filter(|w| *w > requested_weight)
        .min()
        .unwrap_or(requested_weight)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_lower_then_upper_for_static_weights() {
        let ws = [400u16, 600, 800];
        assert_eq!(pick_nearest_css_weight(&ws, 700), 600);
        assert_eq!(pick_nearest_css_weight(&ws, 850), 800);
        assert_eq!(pick_nearest_css_weight(&ws, 300), 400);
    }

    #[test]
    fn variable_font_preserves_requested_weight_within_range() {
        let info = FamilyWeightInfo {
            discrete_weights: vec![400],
            variable_weight_range: Some((100, 900)),
        };
        assert_eq!(resolve_requested_weight(&info, 700), 700);
    }

    #[test]
    fn variable_font_clamps_only_to_axis_bounds() {
        let info = FamilyWeightInfo {
            discrete_weights: vec![400],
            variable_weight_range: Some((200, 750)),
        };
        assert_eq!(resolve_requested_weight(&info, 150), 200);
        assert_eq!(resolve_requested_weight(&info, 900), 750);
    }
}
