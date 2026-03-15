//! WGPU renderer primitives shared by display backends.

pub mod external_buffer;
pub mod glyph_atlas;
pub mod image_cache;
pub mod overlay_state;
pub mod renderer;
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
pub use overlay_state::{MenuPanel, PopupMenuState, TooltipState};
pub use renderer::WgpuRenderer;
pub use vertex::{GlyphVertex, RectVertex, RoundedRectVertex, TextureVertex, Uniforms};
#[cfg(feature = "video")]
pub use video_cache::{CachedVideo, DecodedFrame, VideoCache, VideoState};
#[cfg(all(feature = "wpe-webkit", target_os = "linux"))]
pub use webkit_cache::{CachedWebKitView, WgpuWebKitCache};

/// Re-exported effect configuration module for renderer internals and callers.
pub mod effect_config {
    pub use neomacs_display_protocol::effect_config::*;
}

/// Read GPU power preference from `NEOMACS_GPU`.
pub fn gpu_power_preference() -> wgpu::PowerPreference {
    match std::env::var("NEOMACS_GPU").as_deref() {
        Ok("low") | Ok("integrated") => wgpu::PowerPreference::LowPower,
        Ok("high") | Ok("discrete") => wgpu::PowerPreference::HighPerformance,
        _ => wgpu::PowerPreference::HighPerformance,
    }
}
