//! Neomacs Display Engine
//!
//! A GPU-accelerated display engine for Neomacs using WPE WebKit and wgpu.
//!
//! # Architecture
//!
//! ```text
//! Emacs Core (C) ──► FFI ──► Scene Graph ──► wgpu ──► GPU
//! ```

#![allow(unused)] // TODO: Remove once implementation is complete
#![allow(unsafe_op_in_unsafe_fn)] // TODO: migrate FFI-heavy code to explicit unsafe blocks

#[cfg(all(feature = "core-backend-emacs-c", feature = "core-backend-rust"))]
compile_error!("features `core-backend-emacs-c` and `core-backend-rust` are mutually exclusive");

pub mod backend;
pub mod core;
pub mod effect_config;
pub mod ffi;
pub mod font_match;
pub mod layout;
pub mod text;
pub mod thread_comm;

pub mod render_thread;

#[cfg(feature = "neo-term")]
pub mod terminal;

pub use crate::backend::DisplayBackend;
pub use crate::core::*;
pub use crate::text::TextEngine;
use std::sync::Once;

/// Library version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// NeoVM core backend selected at compile time.
#[cfg(feature = "core-backend-rust")]
pub const CORE_BACKEND: &str = "rust";

/// NeoVM core backend selected at compile time.
#[cfg(not(feature = "core-backend-rust"))]
pub const CORE_BACKEND: &str = "emacs-c";

static LOGGING_INIT: Once = Once::new();

/// Initialize tracing + log bridge once per process.
///
/// This forwards crates using the `log` facade (e.g. cosmic-text) into the
/// tracing subscriber configured via `RUST_LOG`.
pub fn init_logging() {
    LOGGING_INIT.call_once(|| {
        let _ = tracing_log::LogTracer::init();
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .try_init();
    });
}

/// Read GPU power preference from `NEOMACS_GPU` environment variable.
///
/// - `"low"` or `"integrated"` → `LowPower` (prefer integrated GPU, e.g. Intel)
/// - `"high"` or `"discrete"` → `HighPerformance` (prefer discrete GPU, e.g. NVIDIA)
/// - unset or anything else → `HighPerformance` (default)
pub fn gpu_power_preference() -> wgpu::PowerPreference {
    match std::env::var("NEOMACS_GPU").as_deref() {
        Ok("low") | Ok("integrated") => {
            tracing::info!(
                "NEOMACS_GPU={}: using LowPower (integrated GPU)",
                std::env::var("NEOMACS_GPU").unwrap()
            );
            wgpu::PowerPreference::LowPower
        }
        Ok("high") | Ok("discrete") => {
            tracing::info!("NEOMACS_GPU=high: using HighPerformance (discrete GPU)");
            wgpu::PowerPreference::HighPerformance
        }
        Ok(val) => {
            tracing::warn!(
                "NEOMACS_GPU={}: unrecognized value, defaulting to HighPerformance",
                val
            );
            wgpu::PowerPreference::HighPerformance
        }
        Err(_) => wgpu::PowerPreference::HighPerformance,
    }
}

/// Initialize the display engine
pub fn init() -> Result<(), DisplayError> {
    init_logging();
    tracing::info!(
        "Neomacs display engine v{} initializing (wgpu backend)",
        VERSION
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version() {
        assert!(!VERSION.is_empty());
    }
}
