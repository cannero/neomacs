//! Shared font matching helpers.
//!
//! Keeps layout and rasterization behavior consistent.

use cosmic_text::FontSystem;
use fontdb::{
    Family as DbFamily, Query as DbQuery, Stretch as DbStretch, Style as DbStyle,
    Weight as DbWeight,
};

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
        DbStyle::Italic
    } else {
        DbStyle::Normal
    };
    let families = [DbFamily::Name(family)];
    let query = DbQuery {
        families: &families,
        weight: DbWeight(requested_weight),
        stretch: DbStretch::Normal,
        style,
    };

    if let Some(id) = font_system.db().query(&query)
        && let Some(face) = font_system.db().face(id)
    {
        return face.weight.0;
    }

    requested_weight
}
