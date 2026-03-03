//! WGPU renderer primitives shared by display backends.

#[cfg(all(feature = "core-backend-emacs-c", feature = "core-backend-rust"))]
compile_error!("features `core-backend-emacs-c` and `core-backend-rust` are mutually exclusive");

pub mod glyph_atlas;
pub mod vertex;

pub use glyph_atlas::{CachedGlyph, ComposedGlyphKey, GlyphKey, RasterizeResult, WgpuGlyphAtlas};
pub use vertex::{GlyphVertex, RectVertex, RoundedRectVertex, TextureVertex, Uniforms};
