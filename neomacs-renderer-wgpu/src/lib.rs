//! WGPU renderer primitives shared by display backends.

#[cfg(all(feature = "core-backend-emacs-c", feature = "core-backend-rust"))]
compile_error!("features `core-backend-emacs-c` and `core-backend-rust` are mutually exclusive");

pub mod external_buffer;
pub mod glyph_atlas;
pub mod image_cache;
pub mod vertex;
pub mod xbm;
pub mod xpm;

#[cfg(feature = "video")]
pub mod video_cache;

#[cfg(all(feature = "wpe-webkit", target_os = "linux"))]
pub mod webkit_cache;

#[cfg(all(feature = "video", target_os = "linux"))]
pub mod va_dmabuf_export;

#[cfg(all(feature = "video", target_os = "linux"))]
pub mod vulkan_dmabuf;

#[cfg(target_os = "linux")]
pub use external_buffer::DmaBufBuffer;
pub use external_buffer::{BufferFormat, ExternalBuffer, PlatformBuffer, SharedMemoryBuffer};
pub use glyph_atlas::{CachedGlyph, ComposedGlyphKey, GlyphKey, RasterizeResult, WgpuGlyphAtlas};
pub use image_cache::{CachedImage, ImageCache, ImageDimensions, ImageState};
pub use vertex::{GlyphVertex, RectVertex, RoundedRectVertex, TextureVertex, Uniforms};
#[cfg(feature = "video")]
pub use video_cache::{CachedVideo, DecodedFrame, VideoCache, VideoState};
#[cfg(all(feature = "wpe-webkit", target_os = "linux"))]
pub use webkit_cache::{CachedWebKitView, WgpuWebKitCache};
