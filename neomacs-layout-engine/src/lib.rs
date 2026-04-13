//! Rust Display Layout Engine.
//!
//! Replaces the C display engine (xdisp.c) for computing glyph layout.
//! Reads window/buffer state from neovm-core and publishes immutable
//! `FrameDisplayState` snapshots that renderers materialize downstream.

#![allow(unsafe_op_in_unsafe_fn)] // FFI-heavy layout code; migrate to explicit blocks incrementally.

pub mod bidi;
pub mod bidi_layout;
pub mod display_backend;
pub mod display_iterator;
pub mod display_pixel_calc;
pub mod display_status_line;
pub mod emacs_types;
pub mod engine;
pub mod font_loader;
pub mod font_match;
pub mod font_metrics;
pub mod fontconfig;
pub mod hit_test;
pub mod matrix_builder;
pub mod neovm_bridge;
pub mod tty_menu_bar;
pub mod types;
pub mod unicode;
pub mod window_output;

pub use engine::*;
pub use hit_test::{hit_test_charpos_at_pixel, hit_test_window_charpos};
pub use types::*;
