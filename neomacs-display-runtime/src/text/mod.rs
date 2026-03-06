//! Pure Rust text rendering using cosmic-text
//!
//! This module provides text shaping and rasterization using:
//! - cosmic-text for text layout and glyph caching
//! - wgpu textures for GPU upload

mod engine;

pub use engine::TextEngine;
