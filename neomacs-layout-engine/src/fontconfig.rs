//! Fontconfig-based generic font family resolution.
//!
//! GNU Emacs uses fontconfig to resolve generic family names like "Monospace"
//! to concrete font families (e.g., "Hack", "DejaVu Sans Mono"). Neomacs
//! uses cosmic-text for rendering, which has its own font matching and may
//! pick a different font for `Family::Monospace`.
//!
//! This module bridges the gap by querying fontconfig (via `fc-match`) at
//! startup to resolve generic families, then providing the concrete family
//! name so cosmic-text uses the same font as GNU Emacs.

use std::collections::HashMap;
use std::process::Command;
use std::sync::OnceLock;

/// Cached fontconfig resolution results.
static FC_CACHE: OnceLock<HashMap<String, String>> = OnceLock::new();

/// Cached Xft.dpi value from X resources.
static XFT_DPI: OnceLock<f32> = OnceLock::new();

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
                tracing::info!(
                    "fontconfig: {} -> {}",
                    generic,
                    concrete
                );
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

/// Get the effective DPI for font sizing.
///
/// Reads `Xft.dpi` from X resources (same as GNU Emacs), falling back to 96.
/// GNU Emacs uses `Xft.dpi` (set by desktop environments for HiDPI) to convert
/// point sizes to pixels: `pixels = points * dpi / 72`.
pub fn xft_dpi() -> f32 {
    *XFT_DPI.get_or_init(|| {
        let dpi = query_xft_dpi().unwrap_or(96.0);
        tracing::info!("Xft.dpi: {}", dpi);
        dpi
    })
}

/// Query Xft.dpi from X resources via `xrdb -query`.
fn query_xft_dpi() -> Option<f32> {
    let output = Command::new("xrdb")
        .arg("-query")
        .output()
        .ok()?;

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

    // fc-match may return comma-separated families (e.g., "Hack,Hack Nerd Font")
    // Take the first one.
    let family = family.split(',').next()?.trim().to_string();

    if family.is_empty() {
        None
    } else {
        Some(family)
    }
}
