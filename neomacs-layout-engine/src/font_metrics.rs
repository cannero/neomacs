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
    /// Cache: Unicode script range → resolved font key for that script.
    /// Avoids per-character fontconfig queries for same-script characters.
    script_cache: HashMap<unicode_script::Script, ResolvedCharFont>,
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
            script_cache: HashMap::new(),
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

        // Non-ASCII: resolve the actual covering font for this character's
        // script first (cached per script range), then measure with that font.
        let script = unicode_script::Script::from(ch);
        let resolved = if let Some(r) = self.script_cache.get(&script) {
            r.clone()
        } else {
            let r = self.resolve_font_for_char(ch, family, weight, italic);
            self.script_cache.insert(script, r.clone());
            r
        };
        let resolved_italic = resolved.slant.is_italic();
        let resolved_key = MetricsCacheKey::new(&resolved.family, resolved.weight, resolved_italic, font_size);

        let char_key = (resolved_key, ch);
        if let Some(&w) = self.char_cache.get(&char_key) {
            return w;
        }

        let w = self.measure_char(ch, &resolved.family, resolved.weight, resolved_italic, font_size);
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
#[path = "font_metrics_test.rs"]
mod tests;
