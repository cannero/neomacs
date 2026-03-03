//! Font file pre-loading cache.
//!
//! Ensures the Rust renderer uses the exact same font file that Emacs/Fontconfig
//! resolved, by pre-loading it into cosmic-text's fontdb and returning the
//! fontdb-registered family name for use in `Family::Name(...)`.

use cosmic_text::FontSystem;
use std::collections::HashMap;

/// Cache of font file path -> fontdb family name.
/// Avoids re-loading and re-scanning on every frame.
#[derive(Debug, Default)]
pub struct FontFileCache {
    /// Maps font file path -> resolved family name from fontdb (None if load failed)
    path_to_family: HashMap<String, Option<String>>,
}

impl FontFileCache {
    pub fn new() -> Self {
        Self {
            path_to_family: HashMap::new(),
        }
    }

    /// Pre-load a font file into the FontSystem's fontdb and return the
    /// family name that fontdb assigned to it. Returns None if the file
    /// couldn't be loaded or has no family metadata.
    ///
    /// Results are cached so subsequent calls with the same path are free.
    pub fn resolve_family<'a>(
        &'a mut self,
        font_system: &mut FontSystem,
        file_path: &str,
    ) -> Option<&'a str> {
        if !self.path_to_family.contains_key(file_path) {
            let family = Self::load_and_resolve(font_system, file_path);
            self.path_to_family.insert(file_path.to_string(), family);
        }
        self.path_to_family
            .get(file_path)
            .and_then(|f| f.as_deref())
    }

    fn load_and_resolve(font_system: &mut FontSystem, file_path: &str) -> Option<String> {
        // Load the font file into fontdb. This is idempotent if the file
        // was already loaded from fontconfig's initial scan.
        let db = font_system.db_mut();
        let ids = db.load_font_source(fontdb::Source::File(file_path.into()));

        if ids.is_empty() {
            tracing::warn!("FontFileCache: failed to load font file: {}", file_path);
            return None;
        }

        // Extract family name from the first loaded face
        let family = ids.first().and_then(|&id| {
            db.face(id)
                .and_then(|face_info| face_info.families.first().map(|(name, _)| name.clone()))
        });

        if family.is_some() {
            tracing::debug!(
                "FontFileCache: loaded {} -> family {:?}",
                file_path,
                family.as_deref().unwrap_or("?")
            );
        }

        family
    }
}
