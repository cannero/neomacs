//! Font file pre-loading cache.
//!
//! Ensures the Rust renderer uses the exact same font file that Emacs/Fontconfig
//! resolved, by pre-loading it into cosmic-text's fontdb and returning the
//! fontdb-registered family name for use in `Family::Name(...)`.

use allsorts::binary::read::ReadScope;
use allsorts::font_data::FontData;
use allsorts::tables::{FontTableProvider, SfntVersion};
use cosmic_text::FontSystem;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const HEAD_TAG: u32 = u32::from_be_bytes(*b"head");

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

    pub fn prime_file(&mut self, font_system: &mut FontSystem, file_path: &str) -> bool {
        if !self.path_to_family.contains_key(file_path) {
            let family = Self::load_and_resolve(font_system, file_path);
            self.path_to_family.insert(file_path.to_string(), family);
        }
        self.path_to_family
            .get(file_path)
            .is_some_and(|family| family.is_some())
    }

    fn load_and_resolve(font_system: &mut FontSystem, file_path: &str) -> Option<String> {
        let db = font_system.db_mut();
        let ids: Vec<fontdb::ID> = if Self::is_web_font_path(file_path) {
            // Fontconfig may resolve to WOFF/WOFF2. fontdb/ttf-parser doesn't
            // parse those containers directly, so decode to SFNT first.
            Self::load_web_font_source(db, file_path)
                .or_else(|| Self::load_sibling_sfnt_source(db, file_path))
                .unwrap_or_default()
        } else {
            db.load_font_source(fontdb::Source::File(file_path.into()))
                .into_iter()
                .collect()
        };

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

    fn is_web_font_path(file_path: &str) -> bool {
        let Some(ext) = Path::new(file_path).extension().and_then(|e| e.to_str()) else {
            return false;
        };
        ext.eq_ignore_ascii_case("woff2") || ext.eq_ignore_ascii_case("woff")
    }

    fn load_web_font_source(db: &mut fontdb::Database, file_path: &str) -> Option<Vec<fontdb::ID>> {
        let sfnt = Self::decode_web_font_to_sfnt(file_path)?;
        let ids = db.load_font_source(fontdb::Source::Binary(Arc::new(sfnt)));
        if ids.is_empty() {
            tracing::warn!(
                "FontFileCache: decoded webfont but fontdb still rejected it: {}",
                file_path
            );
            return None;
        }
        Some(ids.into_iter().collect())
    }

    fn decode_web_font_to_sfnt(file_path: &str) -> Option<Vec<u8>> {
        let bytes = match std::fs::read(file_path) {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::warn!(
                    "FontFileCache: failed reading webfont {}: {}",
                    file_path,
                    err
                );
                return None;
            }
        };

        let ctxt = ReadScope::new(&bytes);
        let font_data = match ctxt.read::<FontData<'_>>() {
            Ok(font) => font,
            Err(err) => {
                tracing::warn!(
                    "FontFileCache: allsorts failed parsing {}: {:?}",
                    file_path,
                    err
                );
                return None;
            }
        };

        let provider = match font_data.table_provider(0) {
            Ok(provider) => provider,
            Err(err) => {
                tracing::warn!(
                    "FontFileCache: allsorts failed opening provider for {}: {:?}",
                    file_path,
                    err
                );
                return None;
            }
        };

        let mut tags = provider.table_tags().unwrap_or_default();
        if tags.is_empty() {
            tracing::warn!(
                "FontFileCache: allsorts returned no tables for {}",
                file_path
            );
            return None;
        }
        tags.sort_unstable();
        tags.dedup();

        let mut tables = Vec::with_capacity(tags.len());
        for tag in tags {
            let mut data = match provider.table_data(tag) {
                Ok(Some(data)) => data.into_owned(),
                Ok(None) => continue,
                Err(err) => {
                    tracing::warn!(
                        "FontFileCache: failed reading table {:#010x} from {}: {:?}",
                        tag,
                        file_path,
                        err
                    );
                    return None;
                }
            };

            // OpenType requires this field zeroed while checksums are computed.
            if tag == HEAD_TAG && data.len() >= 12 {
                data[8..12].fill(0);
            }
            tables.push((tag, data));
        }

        if tables.is_empty() {
            tracing::warn!(
                "FontFileCache: webfont {} had no usable tables after decode",
                file_path
            );
            return None;
        }

        Some(Self::serialize_sfnt(provider.sfnt_version(), tables))
    }

    fn load_sibling_sfnt_source(
        db: &mut fontdb::Database,
        file_path: &str,
    ) -> Option<Vec<fontdb::ID>> {
        let sibling = Self::find_sibling_sfnt(file_path)?;
        tracing::info!(
            "FontFileCache: using sibling SFNT fallback for {} -> {}",
            file_path,
            sibling.display()
        );
        let ids = db.load_font_source(fontdb::Source::File(sibling));
        if ids.is_empty() {
            return None;
        }
        Some(ids.into_iter().collect())
    }

    fn find_sibling_sfnt(file_path: &str) -> Option<PathBuf> {
        let path = Path::new(file_path);
        let stem = path.file_stem()?.to_str()?;
        let parent = path.parent()?;

        let mut dirs = vec![parent.to_path_buf()];
        if let Some(root) = parent.parent() {
            for dir in ["truetype", "opentype", "ttf", "otf", "TTF", "OTF"] {
                dirs.push(root.join(dir));
            }
        }

        for dir in dirs {
            for ext in ["ttf", "otf", "ttc", "otc", "TTF", "OTF", "TTC", "OTC"] {
                let candidate = dir.join(format!("{stem}.{ext}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }

        None
    }

    fn serialize_sfnt(sfnt_version: u32, tables: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
        #[derive(Clone, Copy)]
        struct Record {
            tag: u32,
            checksum: u32,
            offset: u32,
            length: u32,
        }

        let num_tables = tables.len() as u16;
        let table_dir_len = 12usize + tables.len() * 16;
        let table_data_start = Self::align4(table_dir_len);
        let mut out = vec![0u8; table_data_start];
        let mut records = Vec::with_capacity(tables.len());

        for (tag, table) in tables {
            let length = table.len() as u32;
            let checksum = Self::checksum(&table);
            let offset = out.len() as u32;
            out.extend_from_slice(&table);

            let pad = (4 - (table.len() % 4)) % 4;
            if pad > 0 {
                out.resize(out.len() + pad, 0);
            }

            records.push(Record {
                tag,
                checksum,
                offset,
                length,
            });
        }

        let (search_range, entry_selector, range_shift) = Self::sfnt_search_params(num_tables);
        Self::write_u32_be(&mut out, 0, sfnt_version);
        Self::write_u16_be(&mut out, 4, num_tables);
        Self::write_u16_be(&mut out, 6, search_range);
        Self::write_u16_be(&mut out, 8, entry_selector);
        Self::write_u16_be(&mut out, 10, range_shift);

        for (i, rec) in records.iter().enumerate() {
            let base = 12 + i * 16;
            Self::write_u32_be(&mut out, base, rec.tag);
            Self::write_u32_be(&mut out, base + 4, rec.checksum);
            Self::write_u32_be(&mut out, base + 8, rec.offset);
            Self::write_u32_be(&mut out, base + 12, rec.length);
        }

        if let Some(head_rec) = records.iter().find(|rec| rec.tag == HEAD_TAG) {
            let head_off = head_rec.offset as usize;
            if head_off + 12 <= out.len() {
                let whole_sum = Self::checksum(&out);
                let check_sum_adjustment = 0xB1B0_AFBAu32.wrapping_sub(whole_sum);
                Self::write_u32_be(&mut out, head_off + 8, check_sum_adjustment);
            }
        }

        out
    }

    fn sfnt_search_params(num_tables: u16) -> (u16, u16, u16) {
        if num_tables == 0 {
            return (0, 0, 0);
        }

        let mut max_pow2 = 1u16;
        let mut entry_selector = 0u16;
        while max_pow2.saturating_mul(2) <= num_tables {
            max_pow2 *= 2;
            entry_selector += 1;
        }
        let search_range = max_pow2 * 16;
        let range_shift = num_tables * 16 - search_range;
        (search_range, entry_selector, range_shift)
    }

    fn checksum(bytes: &[u8]) -> u32 {
        let mut sum = 0u32;
        for chunk in bytes.chunks(4) {
            let mut word = [0u8; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            sum = sum.wrapping_add(u32::from_be_bytes(word));
        }
        sum
    }

    fn write_u16_be(out: &mut [u8], offset: usize, value: u16) {
        out[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
    }

    fn write_u32_be(out: &mut [u8], offset: usize, value: u32) {
        out[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn align4(value: usize) -> usize {
        (value + 3) & !3
    }
}
