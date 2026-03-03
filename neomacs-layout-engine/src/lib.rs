//! Rust Display Layout Engine.
//!
//! Replaces the C display engine (xdisp.c) for computing glyph layout.
//! Reads buffer data via FFI and produces `FrameGlyphBuffer` for the renderer.

#![allow(unsafe_op_in_unsafe_fn)] // FFI-heavy layout code; migrate to explicit blocks incrementally.

#[cfg(all(feature = "core-backend-emacs-c", feature = "core-backend-rust"))]
compile_error!("features `core-backend-emacs-c` and `core-backend-rust` are mutually exclusive");

pub mod bidi;
pub mod bidi_layout;
pub mod emacs_ffi;
pub mod emacs_types;
pub mod engine;
pub mod font_loader;
pub mod font_match;
pub mod font_metrics;
pub mod hit_test;
pub mod neovm_bridge;
pub mod status_line;
pub mod types;
pub mod unicode;

pub use engine::*;
pub use hit_test::{hit_test_charpos_at_pixel, hit_test_window_charpos};
pub use types::*;
