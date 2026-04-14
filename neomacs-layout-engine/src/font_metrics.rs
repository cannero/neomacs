//! Cosmic-text based font metrics service for the layout engine.
//!
//! This module provides font measurement using cosmic-text, the same font
//! system used by the render thread for glyph rasterization. By using the
//! same font resolution logic for both measurement and rendering, we
//! guarantee that character widths computed during layout match the actual
//! rendered glyph widths — eliminating gaps and overlaps caused by the
//! C fontconfig and cosmic-text resolving different font files.

use crate::font_loader::FontFileCache;
use cosmic_text::{Attrs, Buffer, Family, FontSystem, Style, Weight};

/// Safe wrapper around cosmic_text::Metrics that ensures font_size and
/// line_height are never zero.  cosmic-text panics with "line height
/// cannot be 0" if either value is 0.0.  GNU Emacs TTY frames use
/// 1x1 cell metrics; we enforce a minimum of 1.0 for safety.
fn safe_metrics(font_size: f32, line_height: f32) -> cosmic_text::Metrics {
    cosmic_text::Metrics::new(font_size.max(1.0), line_height.max(1.0))
}
use neovm_core::face::{FontSlant, FontWeight, FontWidth};
use std::collections::HashMap;
use ttf_parser::Face as TtfFace;

/// Font metrics returned for a given face configuration.
#[derive(Debug, Clone, Copy)]
pub struct FontMetrics {
    /// Baseline offset from the top of the line box.
    pub ascent: f32,
    /// Distance from the baseline to the bottom of the line box.
    pub descent: f32,
    /// Total font height in pixels.
    pub line_height: f32,
    /// Default character width (space character width for monospace)
    pub char_width: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedFontInfo {
    pub family: String,
    pub postscript_name: Option<String>,
    pub weight: FontWeight,
    pub slant: FontSlant,
    pub width: FontWidth,
}

/// Cache key for font metrics lookups.
/// Groups: (family, weight, italic, font_size_centipx)
/// font_size is stored as integer centipixels (size * 100) to avoid float key issues.
#[derive(Hash, Eq, PartialEq, Clone)]
struct MetricsCacheKey {
    family: String,
    weight: u16,
    italic: bool,
    font_size_centipx: i32,
}

#[derive(Debug, Clone)]
struct ResolvedCharFont {
    family: String,
    weight: u16,
    slant: FontSlant,
}

impl MetricsCacheKey {
    fn new(family: &str, weight: u16, italic: bool, font_size: f32) -> Self {
        Self {
            family: family.to_string(),
            weight,
            italic,
            font_size_centipx: (font_size * 100.0) as i32,
        }
    }
}

/// Cosmic-text based font metrics service.
///
/// Runs on the Emacs/layout thread. Creates its own `FontSystem` which scans
/// the same fontconfig database as the render thread's `FontSystem`, ensuring
/// identical font resolution.
pub struct FontMetricsService {
    font_system: FontSystem,
    /// Cache: face attrs → ASCII advance widths (chars 0-127)
    ascii_cache: HashMap<MetricsCacheKey, [f32; 128]>,
    /// Cache: face attrs → single char width (for non-ASCII)
    char_cache: HashMap<(MetricsCacheKey, char), f32>,
    /// Cache: face attrs → font metrics (ascent, descent, etc.)
    metrics_cache: HashMap<MetricsCacheKey, FontMetrics>,
    /// Interned font family strings for cosmic-text Attrs (requires 'static)
    interned_families: HashMap<String, &'static str>,
    /// Cache for pre-loading font files and resolving fontdb family names
    font_file_cache: FontFileCache,
}

impl FontMetricsService {
    /// Create a new FontMetricsService.
    ///
    /// This scans the system font database via fontconfig, which takes ~50ms.
    /// Should be lazily initialized on first use.
    pub fn new() -> Self {
        tracing::info!("FontMetricsService: initializing cosmic-text FontSystem");
        let font_system = FontSystem::new();
        tracing::info!("FontMetricsService: FontSystem ready");
        Self {
            font_system,
            ascii_cache: HashMap::new(),
            char_cache: HashMap::new(),
            metrics_cache: HashMap::new(),
            interned_families: HashMap::new(),
            font_file_cache: FontFileCache::new(),
        }
    }

    /// Resolve the effective font family name for a face.
    ///
    /// If `font_file_path` is provided, pre-loads the exact font file into fontdb
    /// while preserving the exact family name that Fontconfig selected.
    pub fn resolve_family(&mut self, emacs_family: &str, font_file_path: Option<&str>) -> String {
        if let Some(path) = font_file_path {
            let _ = self.font_file_cache.prime_file(&mut self.font_system, path);
        }
        emacs_family.to_string()
    }

    /// Build cosmic-text `Attrs` from face parameters.
    /// Mirrors the logic in `glyph_atlas.rs:face_to_attrs()`.
    fn build_attrs(&mut self, family: &str, weight: u16, slant: FontSlant) -> Attrs<'static> {
        let mut attrs = Attrs::new();

        // Resolve generic family names through fontconfig so we use the same
        // font as GNU Emacs (e.g., "Monospace" → "Hack").
        let resolved = crate::fontconfig::resolve_family(family);
        let family_lower = resolved.to_lowercase();
        let is_generic = matches!(
            family_lower.as_str(),
            "monospace" | "mono" | "" | "serif" | "sans-serif" | "sans" | "sansserif"
        );

        attrs = if is_generic && resolved != family {
            // Fontconfig resolved to a concrete name — use it directly
            let interned = if let Some(&existing) = self.interned_families.get(resolved) {
                existing
            } else {
                let leaked: &'static str = Box::leak(resolved.to_string().into_boxed_str());
                self.interned_families.insert(resolved.to_string(), leaked);
                leaked
            };
            attrs.family(Family::Name(interned))
        } else if is_generic {
            // No fontconfig resolution — fall back to cosmic-text generic
            match family_lower.as_str() {
                "serif" => attrs.family(Family::Serif),
                "sans-serif" | "sans" | "sansserif" => attrs.family(Family::SansSerif),
                _ => attrs.family(Family::Monospace),
            }
        } else {
            let interned = if let Some(&existing) = self.interned_families.get(resolved) {
                existing
            } else {
                let leaked: &'static str = Box::leak(resolved.to_string().into_boxed_str());
                self.interned_families.insert(resolved.to_string(), leaked);
                leaked
            };
            attrs.family(Family::Name(interned))
        };

        // Font weight (CSS 100-900): clamp to closest available in this family.
        let effective_weight = crate::font_match::resolve_weight_in_family(
            &self.font_system,
            family,
            weight,
            slant.is_italic(),
        );
        attrs = attrs.weight(Weight(effective_weight));

        // Font style
        match font_slant_to_cosmic_style(slant) {
            Some(style) => attrs = attrs.style(style),
            None => {}
        }

        attrs
    }

    fn selected_font_id_and_space_width(
        &mut self,
        family: &str,
        weight: u16,
        italic: bool,
        font_size: f32,
    ) -> (Option<fontdb::ID>, f32) {
        let attrs = self.build_attrs(
            family,
            weight,
            if italic {
                FontSlant::Italic
            } else {
                FontSlant::Normal
            },
        );
        let metrics = safe_metrics(font_size, font_size * 1.3);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(
            &mut self.font_system,
            Some(font_size * 4.0),
            Some(font_size * 2.0),
        );
        buffer.set_text(
            &mut self.font_system,
            " ",
            &attrs,
            cosmic_text::Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut self.font_system, false);

        for run in buffer.layout_runs() {
            if let Some(glyph) = run.glyphs.first() {
                return (
                    Some(glyph.physical((0.0, 0.0), 1.0).cache_key.font_id),
                    glyph.w,
                );
            }
        }

        (None, font_size * 0.6)
    }

    fn font_metrics_from_selected_face(
        &mut self,
        font_id: fontdb::ID,
        font_size: f32,
    ) -> Option<FontMetrics> {
        self.font_system
            .db()
            .with_face_data(font_id, |font_data, face_index| {
                let face = TtfFace::parse(font_data, face_index).ok()?;
                let units_per_em = face.units_per_em().max(1) as f32;
                let scale = font_size / units_per_em;
                // GNU GUI backends publish frame line height as the font
                // backend's integer ascent plus integer descent.  Do the
                // same here instead of trusting the typographic height table
                // or a synthetic multiplier.
                let ascent = (face.ascender() as f32 * scale).ceil().max(0.0);
                let descent = (-(face.descender() as f32) * scale).ceil().max(0.0);
                let line_height = (ascent + descent).max(1.0);

                // GNU xdisp.c prefers font-global metrics (FONT_BASE /
                // FONT_DESCENT) and only falls back to per-glyph extents for
                // pathological fonts. Reject obviously bogus table data here
                // and let the caller fall back to glyph-box probing.
                if !ascent.is_finite()
                    || !descent.is_finite()
                    || !line_height.is_finite()
                    || ascent <= 0.0
                    || descent <= 0.0
                    || line_height <= 0.0
                    || line_height > font_size * 4.0
                {
                    return None;
                }

                Some(FontMetrics {
                    ascent,
                    descent,
                    line_height,
                    char_width: 0.0,
                })
            })
            .flatten()
    }

    pub fn select_font_for_char(
        &mut self,
        ch: char,
        family: &str,
        weight: u16,
        italic: bool,
        font_size: f32,
    ) -> Option<SelectedFontInfo> {
        let resolved = self.resolve_font_for_char(ch, family, weight, italic);
        let attrs = self.build_attrs(&resolved.family, resolved.weight, resolved.slant);
        let line_height = font_size * 1.3;
        let metrics = safe_metrics(font_size, line_height);

        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(
            &mut self.font_system,
            Some(font_size * 4.0),
            Some(font_size * 2.0),
        );

        let text = String::from(ch);
        buffer.set_text(
            &mut self.font_system,
            &text,
            &attrs,
            cosmic_text::Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut self.font_system, false);

        for run in buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                let face = self
                    .font_system
                    .db()
                    .face(glyph.physical((0.0, 0.0), 1.0).cache_key.font_id)?;
                return Some(SelectedFontInfo {
                    // TTC/variable collections frequently expose several
                    // regional aliases, and fontdb may report the file's first
                    // alias instead of the family we explicitly resolved for
                    // this character. Preserve the selector's family so
                    // `font-at` mirrors GNU Emacs' realized face semantics.
                    family: resolved.family.clone(),
                    postscript_name: Some(face.post_script_name.clone())
                        .filter(|name| !name.is_empty()),
                    // Variable fonts often report the container face's
                    // metadata weight here even when shaping used a different
                    // requested instance. Preserve the resolved CSS weight so
                    // `font-at` mirrors GNU Emacs' realized face semantics.
                    weight: FontWeight(resolved.weight),
                    slant: font_slant_from_fontdb(face.style),
                    width: font_width_from_stretch_number(face.stretch.to_number()),
                });
            }
        }

        None
    }

    /// Measure a single character's advance width using cosmic-text shaping.
    fn measure_char(
        &mut self,
        ch: char,
        family: &str,
        weight: u16,
        italic: bool,
        font_size: f32,
    ) -> f32 {
        let resolved = self.resolve_font_for_char(ch, family, weight, italic);
        let attrs = self.build_attrs(&resolved.family, resolved.weight, resolved.slant);
        let line_height = font_size * 1.3;
        let metrics = safe_metrics(font_size, line_height);

        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(
            &mut self.font_system,
            Some(font_size * 4.0),
            Some(font_size * 2.0),
        );

        let text = String::from(ch);
        buffer.set_text(
            &mut self.font_system,
            &text,
            &attrs,
            cosmic_text::Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut self.font_system, false);

        // Extract advance width from the first glyph in layout runs
        for run in buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                return glyph.w;
            }
        }

        // Fallback: return font_size * 0.6 as rough monospace estimate
        font_size * 0.6
    }

    fn resolve_font_for_char(
        &mut self,
        ch: char,
        family: &str,
        weight: u16,
        italic: bool,
    ) -> ResolvedCharFont {
        let requested_slant = if italic {
            FontSlant::Italic
        } else {
            FontSlant::Normal
        };
        if ch.is_ascii() {
            let resolved_family =
                self.resolve_family(crate::fontconfig::resolve_family(family), None);
            return ResolvedCharFont {
                family: resolved_family,
                weight,
                slant: requested_slant,
            };
        }

        let prefer_monospace = crate::fontconfig::family_prefers_monospace(family);
        if let Some(matched) =
            crate::fontconfig::match_font_for_char(family, ch, prefer_monospace, weight, italic)
        {
            let resolved_family = self.resolve_family(&matched.family, matched.file.as_deref());
            return ResolvedCharFont {
                weight: crate::font_match::resolve_weight_in_family(
                    &self.font_system,
                    &resolved_family,
                    weight,
                    italic,
                ),
                family: resolved_family,
                slant: requested_slant,
            };
        }

        ResolvedCharFont {
            family: family.to_string(),
            weight,
            slant: requested_slant,
        }
    }

    /// Get the advance width for a single character.
    pub fn char_width(
        &mut self,
        ch: char,
        family: &str,
        weight: u16,
        italic: bool,
        font_size: f32,
    ) -> f32 {
        let key = MetricsCacheKey::new(family, weight, italic, font_size);

        // For ASCII, check the ASCII cache first
        let cp = ch as u32;
        if cp < 128 {
            if let Some(widths) = self.ascii_cache.get(&key) {
                return widths[cp as usize];
            }
            // Fill the whole ASCII cache on miss
            let widths = self.fill_ascii_widths_inner(family, weight, italic, font_size);
            let w = widths[cp as usize];
            self.ascii_cache.insert(key, widths);
            return w;
        }

        // Non-ASCII: check char cache
        let char_key = (key.clone(), ch);
        if let Some(&w) = self.char_cache.get(&char_key) {
            return w;
        }

        let w = self.measure_char(ch, family, weight, italic, font_size);
        self.char_cache.insert(char_key, w);
        w
    }

    /// Fill ASCII width array (0-127) for given face attributes.
    /// Returns the cached array. Populates the cache on miss.
    pub fn fill_ascii_widths(
        &mut self,
        family: &str,
        weight: u16,
        italic: bool,
        font_size: f32,
    ) -> [f32; 128] {
        let key = MetricsCacheKey::new(family, weight, italic, font_size);
        if let Some(widths) = self.ascii_cache.get(&key) {
            return *widths;
        }

        let widths = self.fill_ascii_widths_inner(family, weight, italic, font_size);
        self.ascii_cache.insert(key, widths);
        widths
    }

    /// Internal: measure all 128 ASCII characters in a single buffer.
    fn fill_ascii_widths_inner(
        &mut self,
        family: &str,
        weight: u16,
        italic: bool,
        font_size: f32,
    ) -> [f32; 128] {
        let mut widths = [0.0f32; 128];
        let attrs = self.build_attrs(
            family,
            weight,
            if italic {
                FontSlant::Italic
            } else {
                FontSlant::Normal
            },
        );
        let line_height = font_size * 1.3;
        let metrics = safe_metrics(font_size, line_height);

        // Measure each printable ASCII character individually.
        // Characters 0-31 are control chars — use space width as fallback.
        let space_width = {
            let mut buffer = Buffer::new(&mut self.font_system, metrics);
            buffer.set_size(
                &mut self.font_system,
                Some(font_size * 4.0),
                Some(font_size * 2.0),
            );
            buffer.set_text(
                &mut self.font_system,
                " ",
                &attrs,
                cosmic_text::Shaping::Advanced,
                None,
            );
            buffer.shape_until_scroll(&mut self.font_system, false);
            let mut w = font_size * 0.6;
            for run in buffer.layout_runs() {
                for glyph in run.glyphs.iter() {
                    w = glyph.w;
                    break;
                }
                break;
            }
            w
        };

        // Control chars (0-31) and DEL (127) get space width
        for i in 0..32 {
            widths[i] = space_width;
        }
        widths[127] = space_width;

        // Measure printable ASCII (32-126) using a single buffer with all chars.
        // Shape them individually to get per-character advances.
        for cp in 32u32..127 {
            let ch = char::from_u32(cp).unwrap();
            let mut buffer = Buffer::new(&mut self.font_system, metrics);
            buffer.set_size(
                &mut self.font_system,
                Some(font_size * 4.0),
                Some(font_size * 2.0),
            );
            let text = String::from(ch);
            buffer.set_text(
                &mut self.font_system,
                &text,
                &attrs,
                cosmic_text::Shaping::Advanced,
                None,
            );
            buffer.shape_until_scroll(&mut self.font_system, false);

            let mut found = false;
            for run in buffer.layout_runs() {
                for glyph in run.glyphs.iter() {
                    widths[cp as usize] = glyph.w;
                    found = true;
                    break;
                }
                if found {
                    break;
                }
            }
            if !found {
                widths[cp as usize] = space_width;
            }
        }

        widths
    }

    /// Get font metrics (ascent, descent, line height, char width) for a face.
    pub fn font_metrics(
        &mut self,
        family: &str,
        weight: u16,
        italic: bool,
        font_size: f32,
    ) -> FontMetrics {
        let key = MetricsCacheKey::new(family, weight, italic, font_size);
        if let Some(m) = self.metrics_cache.get(&key) {
            return *m;
        }

        let (selected_font_id, char_width) =
            self.selected_font_id_and_space_width(family, weight, italic, font_size);

        let fm = if let Some(font_id) = selected_font_id {
            if let Some(mut metrics) = self.font_metrics_from_selected_face(font_id, font_size) {
                metrics.char_width = char_width.max(0.0);
                metrics
            } else {
                self.glyph_box_fallback_metrics(family, weight, italic, font_size, char_width)
            }
        } else {
            self.glyph_box_fallback_metrics(family, weight, italic, font_size, char_width)
        };

        self.metrics_cache.insert(key, fm);
        fm
    }

    fn glyph_box_fallback_metrics(
        &mut self,
        family: &str,
        weight: u16,
        italic: bool,
        font_size: f32,
        default_char_width: f32,
    ) -> FontMetrics {
        let attrs = self.build_attrs(
            family,
            weight,
            if italic {
                FontSlant::Italic
            } else {
                FontSlant::Normal
            },
        );
        let line_height = font_size * 1.3;
        let metrics = safe_metrics(font_size, line_height);

        // Fallback only: measure a representative glyph box when the selected
        // font's global tables are unavailable or obviously pathological.
        let sample = " Mg";
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(
            &mut self.font_system,
            Some(font_size * 8.0),
            Some(font_size * 2.0),
        );
        buffer.set_text(
            &mut self.font_system,
            sample,
            &attrs,
            cosmic_text::Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut self.font_system, false);

        let mut char_width = default_char_width.max(font_size * 0.6);
        let mut ascent = font_size.ceil().max(1.0);
        let mut descent = (line_height.ceil() - ascent).max(0.0);
        let mut actual_line_height = (ascent + descent).max(1.0);

        if let Some(layout) = buffer.line_layout(&mut self.font_system, 0) {
            if let Some(line) = layout.first() {
                ascent = line.max_ascent.ceil().max(1.0);
                descent = line.max_descent.ceil().max(0.0);
                actual_line_height = (ascent + descent).max(1.0);
                if let Some(space_glyph) = line.glyphs.iter().find(|glyph| glyph.start == 0) {
                    char_width = space_glyph.w;
                }
            }
        }

        FontMetrics {
            ascent,
            descent,
            line_height: actual_line_height,
            char_width,
        }
    }

    /// Clear all caches. Call when fonts change (e.g., text-scale-adjust).
    pub fn clear_caches(&mut self) {
        self.ascii_cache.clear();
        self.char_cache.clear();
        self.metrics_cache.clear();
    }
}

fn font_slant_from_fontdb(style: Style) -> FontSlant {
    match style {
        Style::Normal => FontSlant::Normal,
        Style::Italic => FontSlant::Italic,
        Style::Oblique => FontSlant::Oblique,
    }
}

fn font_slant_to_cosmic_style(slant: FontSlant) -> Option<Style> {
    match slant {
        FontSlant::Normal => None,
        FontSlant::Italic | FontSlant::ReverseItalic => Some(Style::Italic),
        FontSlant::Oblique | FontSlant::ReverseOblique => Some(Style::Oblique),
    }
}

fn font_width_from_stretch_number(stretch: u16) -> FontWidth {
    match stretch {
        1 => FontWidth::UltraCondensed,
        2 => FontWidth::ExtraCondensed,
        3 => FontWidth::Condensed,
        4 => FontWidth::SemiCondensed,
        5 => FontWidth::Normal,
        6 => FontWidth::SemiExpanded,
        7 => FontWidth::Expanded,
        8 => FontWidth::ExtraExpanded,
        9 => FontWidth::UltraExpanded,
        _ => {
            tracing::debug!(
                "font_metrics: unexpected OpenType width class {}, defaulting to normal",
                stretch
            );
            FontWidth::Normal
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::debug;

    // Helper: create a service (expensive — ~50ms for font scan)
    fn make_svc() -> FontMetricsService {
        FontMetricsService::new()
    }

    fn realized_face_info(
        svc: &mut FontMetricsService,
        ch: char,
        family: &str,
        weight: u16,
        italic: bool,
        font_size: f32,
    ) -> Option<SelectedFontInfo> {
        let resolved = svc.resolve_font_for_char(ch, family, weight, italic);
        let attrs = svc.build_attrs(&resolved.family, resolved.weight, resolved.slant);
        let line_height = font_size * 1.3;
        let metrics = safe_metrics(font_size, line_height);

        let mut buffer = Buffer::new(&mut svc.font_system, metrics);
        buffer.set_size(
            &mut svc.font_system,
            Some(font_size * 4.0),
            Some(font_size * 2.0),
        );

        let text = String::from(ch);
        buffer.set_text(
            &mut svc.font_system,
            &text,
            &attrs,
            cosmic_text::Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut svc.font_system, false);

        for run in buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                let face = svc
                    .font_system
                    .db()
                    .face(glyph.physical((0.0, 0.0), 1.0).cache_key.font_id)?;
                return Some(SelectedFontInfo {
                    family: resolved.family.clone(),
                    postscript_name: Some(face.post_script_name.clone())
                        .filter(|name| !name.is_empty()),
                    weight: FontWeight(face.weight.0),
                    slant: font_slant_from_fontdb(face.style),
                    width: font_width_from_stretch_number(face.stretch.to_number()),
                });
            }
        }

        None
    }

    // ---------------------------------------------------------------
    // Construction
    // ---------------------------------------------------------------

    #[test]
    fn service_construction() {
        let svc = make_svc();
        assert!(svc.ascii_cache.is_empty());
        assert!(svc.char_cache.is_empty());
        assert!(svc.metrics_cache.is_empty());
    }

    // ---------------------------------------------------------------
    // char_width: basic sanity
    // ---------------------------------------------------------------

    #[test]
    fn char_width_space_is_positive() {
        let mut svc = make_svc();
        let w = svc.char_width(' ', "monospace", 400, false, 14.0);
        assert!(w > 0.0, "space width should be positive, got {w}");
    }

    #[test]
    fn char_width_letter_a_is_positive() {
        let mut svc = make_svc();
        let w = svc.char_width('A', "monospace", 400, false, 14.0);
        assert!(w > 0.0, "'A' width should be positive, got {w}");
    }

    #[test]
    fn char_width_monospace_uniform() {
        // In a monospace font, all printable ASCII should have the same width
        let mut svc = make_svc();
        let w_a = svc.char_width('A', "monospace", 400, false, 14.0);
        let w_m = svc.char_width('M', "monospace", 400, false, 14.0);
        let w_i = svc.char_width('i', "monospace", 400, false, 14.0);
        let w_dot = svc.char_width('.', "monospace", 400, false, 14.0);

        // Allow tiny floating-point differences
        let eps = 0.01;
        assert!(
            (w_a - w_m).abs() < eps,
            "monospace A={w_a} vs M={w_m} differ by more than {eps}"
        );
        assert!(
            (w_a - w_i).abs() < eps,
            "monospace A={w_a} vs i={w_i} differ by more than {eps}"
        );
        assert!(
            (w_a - w_dot).abs() < eps,
            "monospace A={w_a} vs .={w_dot} differ by more than {eps}"
        );
    }

    #[test]
    fn char_width_scales_with_font_size() {
        let mut svc = make_svc();
        let w14 = svc.char_width('A', "monospace", 400, false, 14.0);
        let w28 = svc.char_width('A', "monospace", 400, false, 28.0);
        // Doubling font size should roughly double the width
        let ratio = w28 / w14;
        assert!(
            ratio > 1.5 && ratio < 2.5,
            "width ratio for 2x font size should be ~2.0, got {ratio} (w14={w14}, w28={w28})"
        );
    }

    // ---------------------------------------------------------------
    // char_width: specific fonts
    // ---------------------------------------------------------------

    #[test]
    fn char_width_jetbrains_mono() {
        let mut svc = make_svc();
        let w = svc.char_width('A', "JetBrains Mono", 400, false, 14.0);
        assert!(
            w > 0.0,
            "JetBrains Mono 'A' width should be positive, got {w}"
        );
        // JetBrains Mono is monospace — check uniformity
        let w2 = svc.char_width('W', "JetBrains Mono", 400, false, 14.0);
        assert!(
            (w - w2).abs() < 0.01,
            "JetBrains Mono: A={w} W={w2} should be equal"
        );
    }

    #[test]
    fn char_width_dejavu_sans_mono() {
        let mut svc = make_svc();
        let w = svc.char_width('x', "DejaVu Sans Mono", 400, false, 14.0);
        assert!(
            w > 0.0,
            "DejaVu Sans Mono 'x' width should be positive, got {w}"
        );
    }

    #[test]
    fn char_width_proportional_font_varies() {
        // In a proportional font, 'i' should be narrower than 'W'
        let mut svc = make_svc();
        let w_i = svc.char_width('i', "DejaVu Sans", 400, false, 14.0);
        let w_w = svc.char_width('W', "DejaVu Sans", 400, false, 14.0);
        assert!(
            w_w > w_i,
            "proportional font: W={w_w} should be wider than i={w_i}"
        );
    }

    // ---------------------------------------------------------------
    // char_width: non-ASCII
    // ---------------------------------------------------------------

    #[test]
    fn char_width_cjk() {
        let mut svc = make_svc();
        let w_cjk = svc.char_width('漢', "monospace", 400, false, 14.0);
        let w_a = svc.char_width('A', "monospace", 400, false, 14.0);
        // CJK characters are typically double-width
        assert!(
            w_cjk > 0.0,
            "CJK char width should be positive, got {w_cjk}"
        );
        // Don't assert exact 2x ratio as font fallback varies, but it should
        // be wider than a single-width char
        assert!(
            w_cjk > w_a * 1.2,
            "CJK char ({w_cjk}) should be wider than ASCII ({w_a})"
        );
    }

    #[test]
    fn char_width_accented_latin() {
        let mut svc = make_svc();
        let w = svc.char_width('é', "monospace", 400, false, 14.0);
        assert!(w > 0.0, "accented char width should be positive, got {w}");
    }

    // ---------------------------------------------------------------
    // fill_ascii_widths
    // ---------------------------------------------------------------

    #[test]
    fn fill_ascii_widths_all_positive_for_printable() {
        let mut svc = make_svc();
        let widths = svc.fill_ascii_widths("monospace", 400, false, 14.0);
        // Printable ASCII (32-126) should all have positive widths
        for cp in 32u32..127 {
            assert!(
                widths[cp as usize] > 0.0,
                "width for ASCII {} ('{}') should be positive, got {}",
                cp,
                char::from_u32(cp).unwrap(),
                widths[cp as usize]
            );
        }
    }

    #[test]
    fn fill_ascii_widths_control_chars_have_fallback() {
        let mut svc = make_svc();
        let widths = svc.fill_ascii_widths("monospace", 400, false, 14.0);
        // Control chars (0-31) should have space-width fallback
        let space_w = widths[32]; // space
        for cp in 0u32..32 {
            assert!(
                widths[cp as usize] > 0.0,
                "control char {} should have positive fallback width",
                cp
            );
            assert!(
                (widths[cp as usize] - space_w).abs() < 0.01,
                "control char {} width ({}) should match space width ({})",
                cp,
                widths[cp as usize],
                space_w
            );
        }
    }

    #[test]
    fn fill_ascii_widths_cached() {
        let mut svc = make_svc();
        let w1 = svc.fill_ascii_widths("monospace", 400, false, 14.0);
        let w2 = svc.fill_ascii_widths("monospace", 400, false, 14.0);
        // Second call should return exact same values from cache
        for i in 0..128 {
            assert_eq!(w1[i], w2[i], "cache mismatch at index {i}");
        }
    }

    #[test]
    fn fill_ascii_widths_different_sizes_differ() {
        let mut svc = make_svc();
        let w14 = svc.fill_ascii_widths("monospace", 400, false, 14.0);
        let w28 = svc.fill_ascii_widths("monospace", 400, false, 28.0);
        // At a larger size, 'A' (index 65) should be wider
        assert!(
            w28[65] > w14[65],
            "28px A ({}) should be wider than 14px A ({})",
            w28[65],
            w14[65]
        );
    }

    // ---------------------------------------------------------------
    // font_metrics
    // ---------------------------------------------------------------

    #[test]
    fn font_metrics_positive_values() {
        let mut svc = make_svc();
        let m = svc.font_metrics("monospace", 400, false, 14.0);
        assert!(
            m.ascent > 0.0,
            "ascent should be positive, got {}",
            m.ascent
        );
        assert!(
            m.descent > 0.0,
            "descent should be positive, got {}",
            m.descent
        );
        assert!(
            m.line_height > 0.0,
            "line_height should be positive, got {}",
            m.line_height
        );
        assert!(
            m.char_width > 0.0,
            "char_width should be positive, got {}",
            m.char_width
        );
    }

    #[test]
    fn font_metrics_line_height_gte_ascent() {
        let mut svc = make_svc();
        let m = svc.font_metrics("monospace", 400, false, 14.0);
        assert!(
            m.line_height >= m.ascent,
            "line_height ({}) should be >= ascent ({})",
            m.line_height,
            m.ascent
        );
    }

    #[test]
    fn font_metrics_scales_with_size() {
        let mut svc = make_svc();
        let m14 = svc.font_metrics("monospace", 400, false, 14.0);
        let m28 = svc.font_metrics("monospace", 400, false, 28.0);
        assert!(
            m28.char_width > m14.char_width,
            "28px char_width ({}) should be > 14px ({})",
            m28.char_width,
            m14.char_width
        );
        assert!(
            m28.line_height > m14.line_height,
            "28px line_height ({}) should be > 14px ({})",
            m28.line_height,
            m14.line_height
        );
    }

    #[test]
    fn font_metrics_cached() {
        let mut svc = make_svc();
        let m1 = svc.font_metrics("monospace", 400, false, 14.0);
        let m2 = svc.font_metrics("monospace", 400, false, 14.0);
        assert_eq!(m1.ascent, m2.ascent);
        assert_eq!(m1.descent, m2.descent);
        assert_eq!(m1.char_width, m2.char_width);
        assert_eq!(m1.line_height, m2.line_height);
    }

    #[test]
    fn font_metrics_match_selected_font_face_table_metrics() {
        let mut svc = make_svc();
        let family = "monospace";
        let weight = 400;
        let italic = false;
        let font_size = 14.0;
        let fm = svc.font_metrics(family, weight, italic, font_size);

        let (font_id, _) = svc.selected_font_id_and_space_width(family, weight, italic, font_size);
        let expected = svc
            .font_metrics_from_selected_face(font_id.expect("selected font id"), font_size)
            .expect("selected font table metrics");

        assert!(
            (fm.line_height - expected.line_height).abs() < 0.01,
            "expected line height {:.3}, got {:.3}",
            expected.line_height,
            fm.line_height
        );
        assert!(
            (fm.ascent - expected.ascent).abs() < 0.01,
            "expected ascent {:.3}, got {:.3}",
            expected.ascent,
            fm.ascent
        );
        assert!(
            (fm.descent - expected.descent).abs() < 0.01,
            "expected descent {:.3}, got {:.3}",
            expected.descent,
            fm.descent
        );
    }

    // ---------------------------------------------------------------
    // bold / italic variants
    // ---------------------------------------------------------------

    #[test]
    fn char_width_bold_vs_normal() {
        let mut svc = make_svc();
        let w_normal = svc.char_width('A', "DejaVu Sans", 400, false, 14.0);
        let w_bold = svc.char_width('A', "DejaVu Sans", 700, false, 14.0);
        // Both should be positive — bold may or may not be wider depending on font
        assert!(w_normal > 0.0, "normal width should be positive");
        assert!(w_bold > 0.0, "bold width should be positive");
    }

    #[test]
    fn char_width_italic() {
        let mut svc = make_svc();
        let w = svc.char_width('A', "monospace", 400, true, 14.0);
        assert!(w > 0.0, "italic width should be positive, got {w}");
    }

    // ---------------------------------------------------------------
    // clear_caches
    // ---------------------------------------------------------------

    #[test]
    fn clear_caches_empties_all() {
        let mut svc = make_svc();
        // Populate caches
        svc.fill_ascii_widths("monospace", 400, false, 14.0);
        svc.char_width('漢', "monospace", 400, false, 14.0);
        svc.font_metrics("monospace", 400, false, 14.0);

        assert!(!svc.ascii_cache.is_empty());
        assert!(!svc.char_cache.is_empty());
        assert!(!svc.metrics_cache.is_empty());

        svc.clear_caches();

        assert!(svc.ascii_cache.is_empty());
        assert!(svc.char_cache.is_empty());
        assert!(svc.metrics_cache.is_empty());
    }

    // ---------------------------------------------------------------
    // char_width consistency: individual vs fill_ascii
    // ---------------------------------------------------------------

    #[test]
    fn char_width_matches_fill_ascii() {
        let mut svc = make_svc();
        // Get widths via fill_ascii_widths
        let widths = svc.fill_ascii_widths("JetBrains Mono", 400, false, 14.0);

        // Clear caches so char_width computes fresh
        svc.clear_caches();

        // Check that char_width for individual chars matches
        for cp in 32u32..127 {
            let ch = char::from_u32(cp).unwrap();
            let individual = svc.char_width(ch, "JetBrains Mono", 400, false, 14.0);
            let eps = 0.01;
            assert!(
                (individual - widths[cp as usize]).abs() < eps,
                "char_width('{}') = {} but fill_ascii_widths[{}] = {} (diff={})",
                ch,
                individual,
                cp,
                widths[cp as usize],
                (individual - widths[cp as usize]).abs()
            );
        }
    }

    // ---------------------------------------------------------------
    // Print diagnostics (not a real assertion test, but useful
    // for visually inspecting font resolution)
    // ---------------------------------------------------------------

    #[test]
    fn diagnostic_print_widths() {
        let mut svc = make_svc();
        let families = [
            "monospace",
            "JetBrains Mono",
            "DejaVu Sans Mono",
            "DejaVu Sans",
        ];
        for family in families {
            let w_a = svc.char_width('A', family, 400, false, 14.0);
            let w_m = svc.char_width('M', family, 400, false, 14.0);
            let w_i = svc.char_width('i', family, 400, false, 14.0);
            let m = svc.font_metrics(family, 400, false, 14.0);
            debug!(
                "[font_metrics] {family:20} @ 14px: A={w_a:.2} M={w_m:.2} i={w_i:.2} | \
                 ascent={:.2} descent={:.2} line_h={:.2} char_w={:.2}",
                m.ascent, m.descent, m.line_height, m.char_width
            );
        }
    }

    #[test]
    fn opentype_width_class_maps_to_emacs_width_names() {
        assert_eq!(font_width_from_stretch_number(1), FontWidth::UltraCondensed);
        assert_eq!(font_width_from_stretch_number(4), FontWidth::SemiCondensed);
        assert_eq!(font_width_from_stretch_number(5), FontWidth::Normal);
        assert_eq!(font_width_from_stretch_number(7), FontWidth::Expanded);
        assert_eq!(font_width_from_stretch_number(9), FontWidth::UltraExpanded);
    }

    // ---------------------------------------------------------------
    // Cross-FontSystem verification: two independent FontSystem
    // instances (simulating layout thread vs render thread) must
    // produce identical glyph widths.  This is THE critical test —
    // it proves layout and rendering will agree.
    // ---------------------------------------------------------------

    /// Measure a character using a raw FontSystem + Buffer, exactly
    /// as the render thread's rasterize_text() does in glyph_atlas.rs.
    fn measure_with_raw_fontsystem(
        font_system: &mut FontSystem,
        font_file_cache: &mut FontFileCache,
        ch: char,
        requested_family: &str,
        weight: Weight,
        italic: bool,
        font_size: f32,
    ) -> f32 {
        let requested_slant = if italic {
            FontSlant::Italic
        } else {
            FontSlant::Normal
        };
        let (effective_family, effective_weight, effective_slant) = if ch.is_ascii() {
            (
                requested_family.to_string(),
                crate::font_match::resolve_weight_in_family(
                    font_system,
                    requested_family,
                    weight.0,
                    italic,
                ),
                requested_slant,
            )
        } else {
            let prefer_monospace = crate::fontconfig::family_prefers_monospace(requested_family);
            crate::fontconfig::match_font_for_char(
                requested_family,
                ch,
                prefer_monospace,
                weight.0,
                italic,
            )
            .map(|matched| {
                if let Some(path) = matched.file.as_deref() {
                    let _ = font_file_cache.prime_file(font_system, path);
                }
                let effective_family = matched.family;
                (
                    effective_family.clone(),
                    crate::font_match::resolve_weight_in_family(
                        font_system,
                        &effective_family,
                        weight.0,
                        italic,
                    ),
                    requested_slant,
                )
            })
            .unwrap_or_else(|| {
                (
                    requested_family.to_string(),
                    crate::font_match::resolve_weight_in_family(
                        font_system,
                        requested_family,
                        weight.0,
                        italic,
                    ),
                    requested_slant,
                )
            })
        };
        let resolved = crate::fontconfig::resolve_family(&effective_family);
        let family_lower = resolved.to_lowercase();
        let is_generic = matches!(
            family_lower.as_str(),
            "monospace" | "mono" | "" | "serif" | "sans-serif" | "sans" | "sansserif"
        );
        let mut attrs = Attrs::new();
        attrs = if is_generic && resolved != effective_family {
            attrs.family(Family::Name(Box::leak(
                resolved.to_string().into_boxed_str(),
            )))
        } else if is_generic {
            match family_lower.as_str() {
                "serif" => attrs.family(Family::Serif),
                "sans-serif" | "sans" | "sansserif" => attrs.family(Family::SansSerif),
                _ => attrs.family(Family::Monospace),
            }
        } else {
            attrs.family(Family::Name(Box::leak(
                resolved.to_string().into_boxed_str(),
            )))
        };
        attrs = attrs.weight(Weight(effective_weight));
        if let Some(style) = font_slant_to_cosmic_style(effective_slant) {
            attrs = attrs.style(style);
        }
        let line_height = font_size * 1.3;
        let metrics = safe_metrics(font_size, line_height);
        let mut buffer = Buffer::new(font_system, metrics);
        buffer.set_size(font_system, Some(font_size * 8.0), Some(font_size * 3.0));
        let text = String::from(ch);
        buffer.set_text(
            font_system,
            &text,
            &attrs,
            cosmic_text::Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(font_system, false);
        for run in buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                return glyph.w;
            }
        }
        0.0
    }

    #[test]
    fn two_fontsystems_produce_identical_widths() {
        // FontMetricsService (layout thread)
        let mut svc = make_svc();

        // Independent FontSystem (simulating render thread)
        let mut render_fs = FontSystem::new();
        let mut render_font_file_cache = FontFileCache::new();

        let test_cases: &[(&str, u16)] = &[
            ("JetBrains Mono", 400),
            ("JetBrains Mono", 700),
            ("DejaVu Sans Mono", 400),
            ("DejaVu Sans", 400),
            ("monospace", 400),
        ];

        for &(family_str, weight) in test_cases {
            for cp in 32u32..127 {
                let ch = char::from_u32(cp).unwrap();
                let layout_w = svc.char_width(ch, family_str, weight, false, 14.0);
                let render_w = measure_with_raw_fontsystem(
                    &mut render_fs,
                    &mut render_font_file_cache,
                    ch,
                    family_str,
                    Weight(weight),
                    false,
                    14.0,
                );
                assert_eq!(
                    layout_w, render_w,
                    "WIDTH MISMATCH: '{}' (U+{:04X}) in {} w{}: layout={} render={}",
                    ch, cp, family_str, weight, layout_w, render_w
                );
            }
        }
    }

    #[test]
    fn two_fontsystems_identical_for_cjk() {
        let mut svc = make_svc();
        let mut render_fs = FontSystem::new();
        let mut render_font_file_cache = FontFileCache::new();

        let cjk_chars = ['漢', '字', '日', '本', '語', '中', '文'];
        for &ch in &cjk_chars {
            let layout_w = svc.char_width(ch, "monospace", 400, false, 14.0);
            let render_w = measure_with_raw_fontsystem(
                &mut render_fs,
                &mut render_font_file_cache,
                ch,
                "monospace",
                Weight(400),
                false,
                14.0,
            );
            assert_eq!(
                layout_w, render_w,
                "CJK WIDTH MISMATCH: '{}' (U+{:04X}): layout={} render={}",
                ch, ch as u32, layout_w, render_w
            );
        }
    }

    #[test]
    fn explicit_mono_family_cjk_fallback_stays_wider_than_ascii() {
        let mut svc = make_svc();
        let ascii = svc.char_width('a', "Noto Sans Mono", 400, false, 14.0);
        let cjk = svc.char_width('好', "Noto Sans Mono", 400, false, 14.0);
        assert!(
            cjk > ascii * 1.2,
            "explicit mono CJK fallback should stay wider than ASCII: ascii={ascii} cjk={cjk}"
        );
    }

    #[test]
    fn explicit_mono_family_cjk_matches_renderer_across_face_matrix_sizes() {
        let mut svc = make_svc();
        let mut render_fs = FontSystem::new();
        let mut render_font_file_cache = FontFileCache::new();
        let families = [
            "JetBrains Mono",
            "Hack",
            "DejaVu Sans Mono",
            "Noto Sans Mono",
        ];
        let sizes = [24.0_f32, 26.666666_f32, 32.0_f32, 42.666668_f32];
        let weights = [400_u16, 600_u16, 700_u16, 800_u16];

        for family in families {
            for size in sizes {
                for weight in weights {
                    let layout_w = svc.char_width('好', family, weight, false, size);
                    let render_w = measure_with_raw_fontsystem(
                        &mut render_fs,
                        &mut render_font_file_cache,
                        '好',
                        family,
                        Weight(weight),
                        false,
                        size,
                    );
                    assert!(
                        (layout_w - render_w).abs() <= 0.01,
                        "CJK renderer/layout mismatch for family={family} weight={weight} size={size}: layout={layout_w} render={render_w}"
                    );
                }
            }
        }
    }

    #[test]
    fn select_font_for_char_reports_realized_face_metadata() {
        let mut svc = make_svc();
        let selected = svc
            .select_font_for_char('好', "JetBrains Mono", 800, false, 24.0)
            .expect("selected font for fallback char");
        let realized = realized_face_info(&mut svc, '好', "JetBrains Mono", 800, false, 24.0)
            .expect("realized fallback face");
        assert_eq!(selected, realized);
    }

    #[test]
    fn select_font_for_char_preserves_resolved_weight_for_variable_family_reports() {
        let mut svc = make_svc();
        if !svc.font_system.db().faces().any(|face| {
            face.families
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("Noto Sans Mono"))
        }) {
            return;
        }

        let selected = svc
            .select_font_for_char('A', "Noto Sans Mono", 600, false, 24.0)
            .expect("selected font for variable family");
        assert_eq!(selected.weight, FontWeight(600));
    }

    #[test]
    fn select_font_for_char_preserves_resolved_family_for_fallback_reports() {
        let mut svc = make_svc();
        let resolved = svc.resolve_font_for_char('好', "Noto Sans Mono", 400, false);
        let selected = svc
            .select_font_for_char('好', "Noto Sans Mono", 400, false, 24.0)
            .expect("selected font for fallback char");
        assert_eq!(selected.family, resolved.family);
    }

    #[test]
    fn select_font_for_char_resolves_generic_ascii_family() {
        let mut svc = make_svc();
        let expected = crate::fontconfig::resolve_family("Monospace").to_string();
        let selected = svc
            .select_font_for_char('A', "Monospace", 400, false, 24.0)
            .expect("selected font for ascii char");
        assert_eq!(selected.family, expected);
    }

    #[test]
    fn two_fontsystems_identical_for_missing_font() {
        let mut svc = make_svc();
        let mut render_fs = FontSystem::new();
        let mut render_font_file_cache = FontFileCache::new();

        // Fonts that definitely don't exist on the system
        let fake_families = [
            "NonExistentFont-XYZ-12345",
            "Comic Sans MS", // unlikely on NixOS
            "Papyrus",       // unlikely on NixOS
            "ThisFontDoesNotExist",
            "", // empty string
        ];

        for family_str in fake_families {
            for cp in 32u32..127 {
                let ch = char::from_u32(cp).unwrap();
                let layout_w = svc.char_width(ch, family_str, 400, false, 14.0);
                let render_w = measure_with_raw_fontsystem(
                    &mut render_fs,
                    &mut render_font_file_cache,
                    ch,
                    family_str,
                    Weight(400),
                    false,
                    14.0,
                );
                assert_eq!(
                    layout_w, render_w,
                    "MISSING FONT MISMATCH: '{}' (U+{:04X}) in '{}': layout={} render={}",
                    ch, cp, family_str, layout_w, render_w
                );
            }
            // Also check that fallback produces positive widths (not zero/garbage)
            let w = svc.char_width('A', family_str, 400, false, 14.0);
            assert!(
                w > 0.0,
                "missing font '{}' should still produce positive width, got {}",
                family_str,
                w
            );
        }
    }

    #[test]
    fn two_fontsystems_identical_across_weights() {
        let mut svc = make_svc();
        let mut render_fs = FontSystem::new();
        let mut render_font_file_cache = FontFileCache::new();

        // CSS font weights: 100=Thin, 200=ExtraLight, 300=Light,
        // 400=Normal, 500=Medium, 600=SemiBold, 700=Bold, 800=ExtraBold, 900=Black
        let weights: &[u16] = &[100, 200, 300, 400, 500, 600, 700, 800, 900];
        let families = ["JetBrains Mono", "DejaVu Sans", "monospace"];

        for family in families {
            for &weight in weights {
                for cp in 32u32..127 {
                    let ch = char::from_u32(cp).unwrap();
                    let layout_w = svc.char_width(ch, family, weight, false, 14.0);
                    let render_w = measure_with_raw_fontsystem(
                        &mut render_fs,
                        &mut render_font_file_cache,
                        ch,
                        family,
                        Weight(weight),
                        false,
                        14.0,
                    );
                    assert_eq!(
                        layout_w, render_w,
                        "WEIGHT MISMATCH: '{}' in {} w{}: layout={} render={}",
                        ch, family, weight, layout_w, render_w
                    );
                }
            }
        }
    }

    #[test]
    fn two_fontsystems_identical_across_styles() {
        let mut svc = make_svc();
        let mut render_fs = FontSystem::new();
        let mut render_font_file_cache = FontFileCache::new();

        let families = [
            "JetBrains Mono",
            "DejaVu Sans Mono",
            "DejaVu Sans",
            "monospace",
        ];
        let styles: &[(bool, &str)] = &[(false, "normal"), (true, "italic")];
        let weights: &[u16] = &[400, 700];

        for family in families {
            for &weight in weights {
                for &(italic, style_name) in styles {
                    for cp in 32u32..127 {
                        let ch = char::from_u32(cp).unwrap();
                        let layout_w = svc.char_width(ch, family, weight, italic, 14.0);
                        let render_w = measure_with_raw_fontsystem(
                            &mut render_fs,
                            &mut render_font_file_cache,
                            ch,
                            family,
                            Weight(weight),
                            italic,
                            14.0,
                        );
                        assert_eq!(
                            layout_w, render_w,
                            "STYLE MISMATCH: '{}' in {} w{} {}: layout={} render={}",
                            ch, family, weight, style_name, layout_w, render_w
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn two_fontsystems_identical_at_multiple_sizes() {
        let mut svc = make_svc();
        let mut render_fs = FontSystem::new();
        let mut render_font_file_cache = FontFileCache::new();

        for font_size in [10.0, 14.0, 18.0, 24.0, 36.0] {
            for cp in 32u32..127 {
                let ch = char::from_u32(cp).unwrap();
                let layout_w = svc.char_width(ch, "JetBrains Mono", 400, false, font_size);
                let render_w = measure_with_raw_fontsystem(
                    &mut render_fs,
                    &mut render_font_file_cache,
                    ch,
                    "JetBrains Mono",
                    Weight(400),
                    false,
                    font_size,
                );
                assert_eq!(
                    layout_w, render_w,
                    "SIZE MISMATCH: '{}' @ {}px: layout={} render={}",
                    ch, font_size, layout_w, render_w
                );
            }
        }
    }

    // ---------------------------------------------------------------
    // Buffer size parameter: FontMetricsService uses font_size*4.0
    // but rasterize_text() uses font_size*8.0.  Verify this doesn't
    // affect glyph.w values.
    // ---------------------------------------------------------------

    #[test]
    fn buffer_size_does_not_affect_width() {
        let mut fs = FontSystem::new();
        let font_size = 14.0;
        let line_height = font_size * 1.3;
        let metrics = safe_metrics(font_size, line_height);
        let attrs = Attrs::new().family(Family::Monospace).weight(Weight(400));

        for cp in 32u32..127 {
            let ch = char::from_u32(cp).unwrap();
            let text = String::from(ch);

            // Small buffer (font_size * 4.0) — as in FontMetricsService
            let mut buf_small = Buffer::new(&mut fs, metrics);
            buf_small.set_size(&mut fs, Some(font_size * 4.0), Some(font_size * 2.0));
            buf_small.set_text(&mut fs, &text, &attrs, cosmic_text::Shaping::Advanced, None);
            buf_small.shape_until_scroll(&mut fs, false);
            let w_small = buf_small
                .layout_runs()
                .flat_map(|r| r.glyphs.iter())
                .next()
                .map(|g| g.w)
                .unwrap_or(0.0);

            // Large buffer (font_size * 8.0) — as in rasterize_text()
            let mut buf_large = Buffer::new(&mut fs, metrics);
            buf_large.set_size(&mut fs, Some(font_size * 8.0), Some(font_size * 3.0));
            buf_large.set_text(&mut fs, &text, &attrs, cosmic_text::Shaping::Advanced, None);
            buf_large.shape_until_scroll(&mut fs, false);
            let w_large = buf_large
                .layout_runs()
                .flat_map(|r| r.glyphs.iter())
                .next()
                .map(|g| g.w)
                .unwrap_or(0.0);

            assert_eq!(
                w_small, w_large,
                "BUFFER SIZE AFFECTS WIDTH: '{}' (U+{:04X}): small_buf={} large_buf={}",
                ch, cp, w_small, w_large
            );
        }
    }

    // ---------------------------------------------------------------
    // Extreme font sizes
    // ---------------------------------------------------------------

    #[test]
    fn two_fontsystems_identical_at_extreme_sizes() {
        let mut svc = make_svc();
        let mut render_fs = FontSystem::new();
        let mut render_font_file_cache = FontFileCache::new();

        for font_size in [1.0, 4.0, 6.0, 72.0, 144.0] {
            for &ch in &['A', 'M', 'i', '.', ' '] {
                let layout_w = svc.char_width(ch, "monospace", 400, false, font_size);
                let render_w = measure_with_raw_fontsystem(
                    &mut render_fs,
                    &mut render_font_file_cache,
                    ch,
                    "monospace",
                    Weight(400),
                    false,
                    font_size,
                );
                assert_eq!(
                    layout_w, render_w,
                    "EXTREME SIZE MISMATCH: '{}' @ {}px: layout={} render={}",
                    ch, font_size, layout_w, render_w
                );
                assert!(
                    layout_w > 0.0,
                    "'{}' @ {}px should have positive width, got {}",
                    ch,
                    font_size,
                    layout_w
                );
            }
        }
    }

    // ---------------------------------------------------------------
    // Emoji
    // ---------------------------------------------------------------

    #[test]
    fn two_fontsystems_identical_for_emoji() {
        let mut svc = make_svc();
        let mut render_fs = FontSystem::new();
        let mut render_font_file_cache = FontFileCache::new();

        let emoji = ['😀', '🎉', '❤', '👍', '🔥', '⭐', '✅', '🚀'];
        for &ch in &emoji {
            let layout_w = svc.char_width(ch, "monospace", 400, false, 14.0);
            let render_w = measure_with_raw_fontsystem(
                &mut render_fs,
                &mut render_font_file_cache,
                ch,
                "monospace",
                Weight(400),
                false,
                14.0,
            );
            assert_eq!(
                layout_w, render_w,
                "EMOJI MISMATCH: '{}' (U+{:04X}): layout={} render={}",
                ch, ch as u32, layout_w, render_w
            );
            assert!(
                layout_w > 0.0,
                "emoji '{}' should have positive width, got {}",
                ch,
                layout_w
            );
        }
    }

    // ---------------------------------------------------------------
    // Zero-width and special Unicode characters
    // ---------------------------------------------------------------

    #[test]
    fn two_fontsystems_identical_for_zero_width_chars() {
        let mut svc = make_svc();
        let mut render_fs = FontSystem::new();
        let mut render_font_file_cache = FontFileCache::new();

        let special: &[(char, &str)] = &[
            ('\u{200B}', "zero-width space"),
            ('\u{200C}', "ZWNJ"),
            ('\u{200D}', "ZWJ"),
            ('\u{FEFF}', "BOM/ZWNBSP"),
            ('\u{00AD}', "soft hyphen"),
        ];

        for &(ch, name) in special {
            let layout_w = svc.char_width(ch, "monospace", 400, false, 14.0);
            let render_w = measure_with_raw_fontsystem(
                &mut render_fs,
                &mut render_font_file_cache,
                ch,
                "monospace",
                Weight(400),
                false,
                14.0,
            );
            assert_eq!(
                layout_w, render_w,
                "SPECIAL CHAR MISMATCH: {} (U+{:04X}): layout={} render={}",
                name, ch as u32, layout_w, render_w
            );
        }
    }

    // ---------------------------------------------------------------
    // RTL characters (Arabic, Hebrew)
    // ---------------------------------------------------------------

    #[test]
    fn two_fontsystems_identical_for_rtl() {
        let mut svc = make_svc();
        let mut render_fs = FontSystem::new();
        let mut render_font_file_cache = FontFileCache::new();

        let rtl: &[(char, &str)] = &[
            ('א', "Hebrew Alef"),
            ('ב', "Hebrew Bet"),
            ('ع', "Arabic Ain"),
            ('م', "Arabic Meem"),
            ('ش', "Arabic Sheen"),
        ];

        for &(ch, name) in rtl {
            let layout_w = svc.char_width(ch, "monospace", 400, false, 14.0);
            let render_w = measure_with_raw_fontsystem(
                &mut render_fs,
                &mut render_font_file_cache,
                ch,
                "monospace",
                Weight(400),
                false,
                14.0,
            );
            assert_eq!(
                layout_w, render_w,
                "RTL MISMATCH: {} '{}' (U+{:04X}): layout={} render={}",
                name, ch, ch as u32, layout_w, render_w
            );
            assert!(
                layout_w > 0.0,
                "RTL char {} should have positive width, got {}",
                name,
                layout_w
            );
        }
    }

    // ---------------------------------------------------------------
    // Combining marks / diacritics
    // ---------------------------------------------------------------

    #[test]
    fn two_fontsystems_identical_for_combining_marks() {
        let mut svc = make_svc();
        let mut render_fs = FontSystem::new();
        let mut render_font_file_cache = FontFileCache::new();

        // Standalone combining marks — these may have zero advance (expected),
        // but both systems must agree
        let combining: &[(char, &str)] = &[
            ('\u{0300}', "combining grave"),
            ('\u{0301}', "combining acute"),
            ('\u{0302}', "combining circumflex"),
            ('\u{0308}', "combining diaeresis"),
            ('\u{0327}', "combining cedilla"),
            ('\u{0303}', "combining tilde"),
        ];

        for &(ch, name) in combining {
            let layout_w = svc.char_width(ch, "monospace", 400, false, 14.0);
            let render_w = measure_with_raw_fontsystem(
                &mut render_fs,
                &mut render_font_file_cache,
                ch,
                "monospace",
                Weight(400),
                false,
                14.0,
            );
            assert_eq!(
                layout_w, render_w,
                "COMBINING MISMATCH: {} (U+{:04X}): layout={} render={}",
                name, ch as u32, layout_w, render_w
            );
        }
    }

    // ---------------------------------------------------------------
    // Mixed :height faces within a single line
    //
    // Simulates a line like:  normal(14px) LARGE(28px) small(10px) bold(14px)
    // Each character has a different face with different font_size/weight.
    // The layout engine calls char_advance() per-character, switching
    // face_data between calls. Verify that rapid switching between
    // sizes/weights/families produces identical results in both systems.
    // ---------------------------------------------------------------

    /// Simulate a line with mixed face attributes, as the layout engine
    /// would call char_width() while iterating through characters.
    #[test]
    fn two_fontsystems_identical_mixed_heights_in_line() {
        let mut svc = make_svc();
        let mut render_fs = FontSystem::new();
        let mut render_font_file_cache = FontFileCache::new();

        // Each tuple: (char, family, weight, italic, font_size)
        // Simulates a real line: "Hello WORLD tiny Bold"
        // where each word has a different :height face
        let line: &[(char, &str, u16, bool, f32)] = &[
            // "Hello" — normal face, 14px
            ('H', "JetBrains Mono", 400, false, 14.0),
            ('e', "JetBrains Mono", 400, false, 14.0),
            ('l', "JetBrains Mono", 400, false, 14.0),
            ('l', "JetBrains Mono", 400, false, 14.0),
            ('o', "JetBrains Mono", 400, false, 14.0),
            (' ', "JetBrains Mono", 400, false, 14.0),
            // "WORLD" — heading face, :height 2.0 → 28px
            ('W', "JetBrains Mono", 700, false, 28.0),
            ('O', "JetBrains Mono", 700, false, 28.0),
            ('R', "JetBrains Mono", 700, false, 28.0),
            ('L', "JetBrains Mono", 700, false, 28.0),
            ('D', "JetBrains Mono", 700, false, 28.0),
            (' ', "JetBrains Mono", 700, false, 28.0),
            // "tiny" — small face, :height 0.8 → 11.2px
            ('t', "JetBrains Mono", 400, false, 11.2),
            ('i', "JetBrains Mono", 400, false, 11.2),
            ('n', "JetBrains Mono", 400, false, 11.2),
            ('y', "JetBrains Mono", 400, false, 11.2),
            (' ', "JetBrains Mono", 400, false, 14.0),
            // "Bold" — bold italic, same size
            ('B', "JetBrains Mono", 700, true, 14.0),
            ('o', "JetBrains Mono", 700, true, 14.0),
            ('l', "JetBrains Mono", 700, true, 14.0),
            ('d', "JetBrains Mono", 700, true, 14.0),
            // Switch to proportional mid-line
            (' ', "DejaVu Sans", 400, false, 14.0),
            ('v', "DejaVu Sans", 400, false, 14.0),
            ('a', "DejaVu Sans", 400, false, 14.0),
            ('r', "DejaVu Sans", 400, false, 14.0),
            // Back to monospace, different size
            (' ', "JetBrains Mono", 400, false, 18.0),
            ('e', "JetBrains Mono", 400, false, 18.0),
            ('n', "JetBrains Mono", 400, false, 18.0),
            ('d', "JetBrains Mono", 400, false, 18.0),
        ];

        let mut layout_total = 0.0f32;
        let mut render_total = 0.0f32;

        for (i, &(ch, family, weight, italic, font_size)) in line.iter().enumerate() {
            let layout_w = svc.char_width(ch, family, weight, italic, font_size);
            let render_w = measure_with_raw_fontsystem(
                &mut render_fs,
                &mut render_font_file_cache,
                ch,
                family,
                Weight(weight),
                italic,
                font_size,
            );

            assert_eq!(
                layout_w,
                render_w,
                "MIXED LINE MISMATCH at pos {}: '{}' ({} w{} {} {}px): layout={} render={}",
                i,
                ch,
                family,
                weight,
                if italic { "italic" } else { "normal" },
                font_size,
                layout_w,
                render_w
            );

            layout_total += layout_w;
            render_total += render_w;
        }

        // Total line width must also match exactly
        assert_eq!(
            layout_total, render_total,
            "MIXED LINE TOTAL WIDTH MISMATCH: layout={} render={}",
            layout_total, render_total
        );
    }

    /// Same test but with org-mode-like headings: *, **, *** at :height 3.0, 2.0, 1.5
    #[test]
    fn two_fontsystems_identical_org_heading_sizes() {
        let mut svc = make_svc();
        let mut render_fs = FontSystem::new();
        let mut render_font_file_cache = FontFileCache::new();

        // Simulates org-mode: "* H1  ** H2  *** H3  body"
        // with decreasing :height per heading level
        let segments: &[(&str, &str, u16, f32)] = &[
            ("* ", "JetBrains Mono", 700, 42.0), // :height 3.0 → 42px
            ("H1 ", "JetBrains Mono", 700, 42.0),
            ("** ", "JetBrains Mono", 700, 28.0), // :height 2.0 → 28px
            ("H2 ", "JetBrains Mono", 700, 28.0),
            ("*** ", "JetBrains Mono", 700, 21.0), // :height 1.5 → 21px
            ("H3 ", "JetBrains Mono", 700, 21.0),
            ("body ", "JetBrains Mono", 400, 14.0),  // normal
            ("code", "DejaVu Sans Mono", 400, 14.0), // inline code, different font
        ];

        for (seg_text, family, weight, font_size) in segments {
            for ch in seg_text.chars() {
                let layout_w = svc.char_width(ch, family, *weight, false, *font_size);
                let render_w = measure_with_raw_fontsystem(
                    &mut render_fs,
                    &mut render_font_file_cache,
                    ch,
                    family,
                    Weight(*weight),
                    false,
                    *font_size,
                );
                assert_eq!(
                    layout_w, render_w,
                    "ORG HEADING MISMATCH: '{}' in {} w{} {}px: layout={} render={}",
                    ch, family, weight, font_size, layout_w, render_w
                );
            }
        }
    }
}
