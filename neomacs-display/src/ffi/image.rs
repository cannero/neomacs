//! Image Management FFI functions
//!
//! Video/image glyph stubs and image loading functions.

use super::*;

// ============================================================================
// Centralized Image Loading (replaces C-side neomacs_get_or_load_image)
// ============================================================================

/// Image loading info extracted from C `struct image` fields.
/// C fills this via `neomacs_extract_image_load_info()`, Rust decides how to load.
#[repr(C)]
pub struct NeomacsImageLoadInfo {
    /// Existing GPU ID from img->neomacs_gpu_id (0 if not yet loaded)
    pub existing_gpu_id: u32,

    // === Pixmap source (Emacs Cairo decoded) ===
    pub pixmap_data: *const u8,    // NULL if no pixmap
    pub pixmap_width: c_int,
    pub pixmap_height: c_int,
    pub pixmap_stride: c_int,
    pub pixmap_bpp: c_int,         // 24 or 32
    pub pixmap_has_mask: c_int,    // 1 if img->mask != 0

    // === File source ===
    pub file_path: *const c_char,  // NULL if not file-based

    // === Encoded data source ===
    pub encoded_data: *const u8,   // NULL if not data-based
    pub encoded_data_len: isize,

    // === Pre-loaded ID from :neomacs-id ===
    pub neomacs_id: u32,           // 0 if not pre-loaded

    // === Dimension constraints from spec ===
    pub max_width: c_int,
    pub max_height: c_int,
    pub target_width: c_int,
    pub target_height: c_int,
    pub scale: c_double,           // 1.0 = no scaling

    // === Current image dimensions ===
    pub img_width: c_int,
    pub img_height: c_int,

    // === Foreground/background colors for monochrome formats (XBM) ===
    pub fg_color: u32,  // 0xAARRGGBB, 0 = default
    pub bg_color: u32,  // 0xAARRGGBB, 0 = default
}

/// Result of image loading — GPU ID and final display dimensions.
#[repr(C)]
pub struct NeomacsImageLoadResult {
    pub gpu_id: u32,     // 0 on failure
    pub width: c_int,    // Final display width
    pub height: c_int,   // Final display height
}

/// Apply :scale, :width, :height constraints to actual image dimensions.
fn apply_dimension_constraints(
    actual_w: u32,
    actual_h: u32,
    scale: f64,
    target_w: c_int,
    target_h: c_int,
) -> (u32, u32) {
    let mut w = actual_w as f64;
    let mut h = actual_h as f64;

    if scale != 1.0 && scale > 0.0 {
        w *= scale;
        h *= scale;
    }

    if target_w > 0 && target_h > 0 {
        (target_w as u32, target_h as u32)
    } else if target_w > 0 {
        let tw = target_w as f64;
        if w > 0.0 {
            (target_w as u32, (tw * h / w) as u32)
        } else {
            (target_w as u32, target_w as u32)
        }
    } else if target_h > 0 {
        let th = target_h as f64;
        if h > 0.0 {
            ((th * w / h) as u32, target_h as u32)
        } else {
            (target_h as u32, target_h as u32)
        }
    } else {
        (w as u32, h as u32)
    }
}

/// Centralized image loading function called from C.
/// Replaces the ~170-line `neomacs_get_or_load_image()` in neomacsterm.c.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_rust_load_image(
    info: *const NeomacsImageLoadInfo,
) -> NeomacsImageLoadResult {
    let info = &*info;
    let mut result = NeomacsImageLoadResult {
        gpu_id: 0,
        width: info.img_width,
        height: info.img_height,
    };

    // Path 0: Already loaded — just query dimensions if missing
    if info.existing_gpu_id != 0 {
        result.gpu_id = info.existing_gpu_id;
        if result.width == 0 || result.height == 0 {
            if let Some(ref state) = THREADED_STATE {
                if let Ok(dims) = state.image_dimensions.lock() {
                    if let Some(&(w, h)) = dims.get(&result.gpu_id) {
                        result.width = w as c_int;
                        result.height = h as c_int;
                    }
                }
            }
        }
        return result;
    }

    let state = match (*std::ptr::addr_of!(THREADED_STATE)).as_ref() {
        Some(s) => s,
        None => return result,
    };

    let mut gpu_id: u32 = 0;

    // Path 1: Pre-loaded neomacs-id
    if info.neomacs_id != 0 {
        gpu_id = info.neomacs_id;
    }
    // Path 2: Pixmap (Emacs Cairo decoded)
    else if !info.pixmap_data.is_null() && info.pixmap_width > 0 && info.pixmap_height > 0 {
        let data_len = match (info.pixmap_stride as usize).checked_mul(info.pixmap_height as usize)
        {
            Some(len) => len,
            None => return result,
        };
        let data = std::slice::from_raw_parts(info.pixmap_data, data_len);
        let id = IMAGE_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let cmd = if info.pixmap_has_mask != 0 || info.pixmap_bpp == 32 {
            RenderCommand::ImageLoadArgb32 {
                id,
                data: data.to_vec(),
                width: info.pixmap_width as u32,
                height: info.pixmap_height as u32,
                stride: info.pixmap_stride as u32,
            }
        } else {
            RenderCommand::ImageLoadRgb24 {
                id,
                data: data.to_vec(),
                width: info.pixmap_width as u32,
                height: info.pixmap_height as u32,
                stride: info.pixmap_stride as u32,
            }
        };
        let _ = state.emacs_comms.cmd_tx.try_send(cmd);
        gpu_id = id;
    }
    // Path 3: File
    else if !info.file_path.is_null() {
        let path = std::ffi::CStr::from_ptr(info.file_path).to_string_lossy();
        let id = IMAGE_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let cmd = RenderCommand::ImageLoadFile {
            id,
            path: path.into_owned(),
            max_width: info.max_width.max(0) as u32,
            max_height: info.max_height.max(0) as u32,
            fg_color: info.fg_color,
            bg_color: info.bg_color,
        };
        let _ = state.emacs_comms.cmd_tx.try_send(cmd);
        gpu_id = id;
    }
    // Path 4: Encoded data
    else if !info.encoded_data.is_null() && info.encoded_data_len > 0 {
        let data =
            std::slice::from_raw_parts(info.encoded_data, info.encoded_data_len as usize);
        let id = IMAGE_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let cmd = RenderCommand::ImageLoadData {
            id,
            data: data.to_vec(),
            max_width: info.max_width.max(0) as u32,
            max_height: info.max_height.max(0) as u32,
            fg_color: info.fg_color,
            bg_color: info.bg_color,
        };
        let _ = state.emacs_comms.cmd_tx.try_send(cmd);
        gpu_id = id;
    }

    if gpu_id == 0 {
        return result;
    }

    result.gpu_id = gpu_id;

    // Try to get actual dimensions from shared map (for file/data paths
    // where the render thread may have already loaded the image)
    if let Ok(dims) = state.image_dimensions.lock() {
        if let Some(&(actual_w, actual_h)) = dims.get(&gpu_id) {
            let (aw, ah) = apply_dimension_constraints(
                actual_w,
                actual_h,
                info.scale,
                info.target_width,
                info.target_height,
            );
            result.width = aw as c_int;
            result.height = ah as c_int;
        }
    }

    result
}

// ============================================================================
// Image Management (stubs - no GTK4 backend)
// ============================================================================

/// Add a video glyph to the current row
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_add_video_glyph(
    handle: *mut NeomacsDisplay,
    video_id: u32,
    pixel_width: c_int,
    pixel_height: c_int,
) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;
    let current_y = display.current_row_y;  // Frame-absolute Y
    let current_x = display.current_row_x;

    display.frame_glyphs.add_video(
        video_id,
        current_x as f32,
        current_y as f32,
        pixel_width as f32,
        pixel_height as f32,
        0,
        false,
    );
    display.current_row_x += pixel_width;
}

/// Load a video from file path (async - uses GStreamer)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_load_video(
    handle: *mut NeomacsDisplay,
    path: *const c_char,
) -> u32 {
    let display = match handle.as_mut() {
        Some(d) => d,
        None => return 0,
    };

    let path_str = match std::ffi::CStr::from_ptr(path).to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };

    tracing::info!("load_video: path={}", path_str);

    // Threaded path: send command to render thread
    #[cfg(feature = "video")]
    if let Some(ref state) = THREADED_STATE {
        let id = VIDEO_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let cmd = RenderCommand::VideoCreate {
            id,
            path: path_str.to_string(),
        };
        let _ = state.emacs_comms.cmd_tx.try_send(cmd);
        tracing::info!("load_video: threaded path, id={}", id);
        return id;
    }

    #[cfg(feature = "video")]
    if let Some(ref mut backend) = display.winit_backend {
        if let Some(renderer) = backend.renderer_mut() {
            let id = renderer.load_video_file(path_str);
            tracing::info!("load_video: returned id={}", id);
            return id;
        }
    }

    0
}

/// Play a loaded video
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_video_play(
    handle: *mut NeomacsDisplay,
    video_id: u32,
) -> c_int {
    // Threaded path
    #[cfg(feature = "video")]
    if let Some(ref state) = THREADED_STATE {
        let cmd = RenderCommand::VideoPlay { id: video_id };
        let _ = state.emacs_comms.cmd_tx.try_send(cmd);
        return 0;
    }

    let display = match handle.as_mut() {
        Some(d) => d,
        None => return -1,
    };

    #[cfg(feature = "video")]
    if let Some(ref mut backend) = display.winit_backend {
        if let Some(renderer) = backend.renderer_mut() {
            renderer.video_play(video_id);
            return 0;
        }
    }

    -1
}

/// Pause a video
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_video_pause(
    handle: *mut NeomacsDisplay,
    video_id: u32,
) -> c_int {
    // Threaded path
    #[cfg(feature = "video")]
    if let Some(ref state) = THREADED_STATE {
        let cmd = RenderCommand::VideoPause { id: video_id };
        let _ = state.emacs_comms.cmd_tx.try_send(cmd);
        return 0;
    }

    let display = match handle.as_mut() {
        Some(d) => d,
        None => return -1,
    };

    #[cfg(feature = "video")]
    if let Some(ref mut backend) = display.winit_backend {
        if let Some(renderer) = backend.renderer_mut() {
            renderer.video_pause(video_id);
            return 0;
        }
    }

    -1
}

/// Stop a video
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_video_stop(
    handle: *mut NeomacsDisplay,
    video_id: u32,
) -> c_int {
    // Threaded path: stop maps to destroy
    #[cfg(feature = "video")]
    if let Some(ref state) = THREADED_STATE {
        let cmd = RenderCommand::VideoDestroy { id: video_id };
        let _ = state.emacs_comms.cmd_tx.try_send(cmd);
        return 0;
    }

    let display = match handle.as_mut() {
        Some(d) => d,
        None => return -1,
    };

    #[cfg(feature = "video")]
    if let Some(ref mut backend) = display.winit_backend {
        if let Some(renderer) = backend.renderer_mut() {
            renderer.video_stop(video_id);
            return 0;
        }
    }

    -1
}

/// Set video loop mode (-1 for infinite)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_video_set_loop(
    handle: *mut NeomacsDisplay,
    video_id: u32,
    loop_count: c_int,
) -> c_int {
    let display = match handle.as_mut() {
        Some(d) => d,
        None => return -1,
    };

    #[cfg(feature = "video")]
    if let Some(ref mut backend) = display.winit_backend {
        if let Some(renderer) = backend.renderer_mut() {
            renderer.video_set_loop(video_id, loop_count);
            return 0;
        }
    }

    -1
}

/// Process pending video frames (call each frame)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_video_update(
    handle: *mut NeomacsDisplay,
    _video_id: u32,
) -> c_int {
    let display = match handle.as_mut() {
        Some(d) => d,
        None => return -1,
    };

    #[cfg(feature = "video")]
    if let Some(ref mut backend) = display.winit_backend {
        if let Some(renderer) = backend.renderer_mut() {
            renderer.process_pending_videos();
            return 0;
        }
    }

    -1
}

/// Get video dimensions (works for pending and loaded videos)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_get_video_size(
    handle: *mut NeomacsDisplay,
    video_id: u32,
    width: *mut c_int,
    height: *mut c_int,
) -> c_int {
    if handle.is_null() || width.is_null() || height.is_null() {
        return -1;
    }
    let display = &mut *handle;

    #[cfg(feature = "video")]
    if let Some(ref backend) = display.winit_backend {
        if let Some(renderer) = backend.renderer() {
            if let Some((w, h)) = renderer.get_video_size(video_id) {
                *width = w as c_int;
                *height = h as c_int;
                return 0;
            }
        }
    }

    -1
}

// ============================================================================
// Image Functions (stubs - no GTK4 backend)
// ============================================================================

/// Load an image from a file path (delegates to load_image_file)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_load_image(
    handle: *mut NeomacsDisplay,
    path: *const c_char,
) -> u32 {
    neomacs_display_load_image_file(handle, path)
}

/// Load an image from raw bytes (encoded image format)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_load_image_data(
    handle: *mut NeomacsDisplay,
    data: *const u8,
    len: usize,
) -> u32 {
    if data.is_null() || len == 0 {
        return 0;
    }

    let data_slice = std::slice::from_raw_parts(data, len);

    // Threaded path: send encoded data to render thread
    if let Some(ref state) = THREADED_STATE {
        let id = IMAGE_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        tracing::info!("load_image_data: threaded path, id={}, len={} bytes", id, len);
        let cmd = RenderCommand::ImageLoadData {
            id,
            data: data_slice.to_vec(),
            max_width: 0,
            max_height: 0,
            fg_color: 0,
            bg_color: 0,
        };
        let _ = state.emacs_comms.cmd_tx.try_send(cmd);
        return id;
    }

    // Non-threaded path: direct renderer access
    if handle.is_null() {
        return 0;
    }
    let display = &mut *handle;

    if let Some(ref mut backend) = display.winit_backend {
        if let Some(renderer) = backend.renderer_mut() {
            return renderer.load_image_data(data_slice, 0, 0, 0, 0);
        }
    }
    0
}

/// Load an image from raw bytes with optional scaling
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_load_image_data_scaled(
    handle: *mut NeomacsDisplay,
    data: *const u8,
    len: usize,
    max_width: c_int,
    max_height: c_int,
) -> u32 {
    if data.is_null() || len == 0 {
        return 0;
    }

    let data_slice = std::slice::from_raw_parts(data, len);

    // Threaded path: send encoded data to render thread
    if let Some(ref state) = THREADED_STATE {
        let id = IMAGE_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let cmd = RenderCommand::ImageLoadData {
            id,
            data: data_slice.to_vec(),
            max_width: max_width.max(0) as u32,
            max_height: max_height.max(0) as u32,
            fg_color: 0,
            bg_color: 0,
        };
        let _ = state.emacs_comms.cmd_tx.try_send(cmd);
        return id;
    }

    // Non-threaded path: direct renderer access
    if handle.is_null() {
        return 0;
    }
    let display = &mut *handle;

    if let Some(ref mut backend) = display.winit_backend {
        if let Some(renderer) = backend.renderer_mut() {
            return renderer.load_image_data(
                data_slice,
                max_width.max(0) as u32,
                max_height.max(0) as u32,
                0,
                0,
            );
        }
    }
    0
}

/// Load an image from raw ARGB32 pixel data
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_load_image_argb32(
    handle: *mut NeomacsDisplay,
    data: *const u8,
    width: c_int,
    height: c_int,
    stride: c_int,
) -> u32 {
    if handle.is_null() || data.is_null() || width <= 0 || height <= 0 || stride <= 0 {
        return 0;
    }

    // Use checked multiplication to prevent overflow
    let data_len = match (stride as usize).checked_mul(height as usize) {
        Some(len) => len,
        None => return 0,
    };
    let data_slice = std::slice::from_raw_parts(data, data_len);

    // Threaded path: send pixel data to render thread
    if let Some(ref state) = THREADED_STATE {
        let id = IMAGE_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let cmd = RenderCommand::ImageLoadArgb32 {
            id,
            data: data_slice.to_vec(),
            width: width as u32,
            height: height as u32,
            stride: stride as u32,
        };
        let _ = state.emacs_comms.cmd_tx.try_send(cmd);
        return id;
    }

    // Non-threaded path: direct renderer access
    let display = &mut *handle;
    if let Some(ref mut backend) = display.winit_backend {
        if let Some(renderer) = backend.renderer_mut() {
            return renderer.load_image_argb32(
                data_slice,
                width as u32,
                height as u32,
                stride as u32,
            );
        }
    }
    0
}

/// Load an image from raw RGB24 pixel data
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_load_image_rgb24(
    handle: *mut NeomacsDisplay,
    data: *const u8,
    width: c_int,
    height: c_int,
    stride: c_int,
) -> u32 {
    if handle.is_null() || data.is_null() || width <= 0 || height <= 0 || stride <= 0 {
        return 0;
    }

    // Use checked multiplication to prevent overflow
    let data_len = match (stride as usize).checked_mul(height as usize) {
        Some(len) => len,
        None => return 0,
    };
    let data_slice = std::slice::from_raw_parts(data, data_len);

    // Threaded path: send pixel data to render thread
    if let Some(ref state) = THREADED_STATE {
        let id = IMAGE_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let cmd = RenderCommand::ImageLoadRgb24 {
            id,
            data: data_slice.to_vec(),
            width: width as u32,
            height: height as u32,
            stride: stride as u32,
        };
        let _ = state.emacs_comms.cmd_tx.try_send(cmd);
        return id;
    }

    // Non-threaded path: direct renderer access
    let display = &mut *handle;
    if let Some(ref mut backend) = display.winit_backend {
        if let Some(renderer) = backend.renderer_mut() {
            return renderer.load_image_rgb24(
                data_slice,
                width as u32,
                height as u32,
                stride as u32,
            );
        }
    }
    0
}

/// Load an image from a file path (async - returns ID immediately)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_load_image_file(
    handle: *mut NeomacsDisplay,
    path: *const c_char,
) -> u32 {
    neomacs_display_load_image_file_scaled(handle, path, 0, 0)
}

/// Load an image from a file path with scaling (async)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_load_image_file_scaled(
    handle: *mut NeomacsDisplay,
    path: *const c_char,
    max_width: c_int,
    max_height: c_int,
) -> u32 {
    if handle.is_null() || path.is_null() {
        return 0;
    }
    let path_str = match std::ffi::CStr::from_ptr(path).to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };

    tracing::info!("load_image_file_scaled: path={}, max={}x{}", path_str, max_width, max_height);

    // Threaded path: send command to render thread
    if let Some(ref state) = THREADED_STATE {
        let id = IMAGE_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let cmd = RenderCommand::ImageLoadFile {
            id,
            path: path_str.to_string(),
            max_width: max_width.max(0) as u32,
            max_height: max_height.max(0) as u32,
            fg_color: 0,
            bg_color: 0,
        };
        let _ = state.emacs_comms.cmd_tx.try_send(cmd);
        tracing::info!("load_image_file_scaled: threaded path, id={}", id);
        return id;
    }

    // Non-threaded path: direct renderer access
    let display = &mut *handle;
    if let Some(ref mut backend) = display.winit_backend {
        if let Some(renderer) = backend.renderer_mut() {
            let id = renderer.load_image_file(
                path_str,
                max_width.max(0) as u32,
                max_height.max(0) as u32,
                0,
                0,
            );
            tracing::info!("load_image_file_scaled: returned id={}", id);
            return id;
        }
    }
    0
}

/// Load an image directly as texture (same as load_image_file)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_load_image_file_direct(
    handle: *mut NeomacsDisplay,
    path: *const c_char,
) -> u32 {
    neomacs_display_load_image_file(handle, path)
}

/// Load an image directly as texture with scaling
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_load_image_file_direct_scaled(
    handle: *mut NeomacsDisplay,
    path: *const c_char,
    max_width: c_int,
    max_height: c_int,
) -> u32 {
    neomacs_display_load_image_file_scaled(handle, path, max_width, max_height)
}

/// Get image dimensions (works for pending and loaded images)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_get_image_size(
    handle: *mut NeomacsDisplay,
    image_id: u32,
    width: *mut c_int,
    height: *mut c_int,
) -> c_int {
    if width.is_null() || height.is_null() {
        return -1;
    }

    // Threaded path: check shared map
    if let Some(ref state) = THREADED_STATE {
        if let Ok(dims) = state.image_dimensions.lock() {
            if let Some(&(w, h)) = dims.get(&image_id) {
                *width = w as c_int;
                *height = h as c_int;
                return 0;
            }
        }
        // Not ready yet - return 0,0 so Emacs can retry on next redisplay
        *width = 0;
        *height = 0;
        return -1;
    }

    // Non-threaded path: direct renderer access
    if handle.is_null() {
        return -1;
    }
    let display = &mut *handle;

    if let Some(ref backend) = display.winit_backend {
        if let Some(renderer) = backend.renderer() {
            if let Some((w, h)) = renderer.get_image_size(image_id) {
                *width = w as c_int;
                *height = h as c_int;
                return 0;
            }
        }
    }
    -1
}

/// Query image file dimensions without loading (fast - reads header only)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_query_image_file_size(
    _handle: *mut NeomacsDisplay,
    path: *const c_char,
    width: *mut c_int,
    height: *mut c_int,
) -> c_int {
    if path.is_null() || width.is_null() || height.is_null() {
        return -1;
    }
    let path_str = match std::ffi::CStr::from_ptr(path).to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };

    {
        use crate::backend::wgpu::WgpuRenderer;
        if let Some((w, h)) = WgpuRenderer::query_image_file_size(path_str) {
            *width = w as c_int;
            *height = h as c_int;
            return 0;
        }
    }
    -1
}

/// Query image data dimensions synchronously (reads header only, no GPU loading).
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_query_image_data_size(
    _handle: *mut NeomacsDisplay,
    data: *const u8,
    len: usize,
    width: *mut c_int,
    height: *mut c_int,
) -> c_int {
    if data.is_null() || width.is_null() || height.is_null() || len == 0 {
        return -1;
    }
    let bytes = std::slice::from_raw_parts(data, len);

    {
        use crate::backend::wgpu::WgpuRenderer;
        if let Some((w, h)) = WgpuRenderer::query_image_data_size(bytes) {
            *width = w as c_int;
            *height = h as c_int;
            return 0;
        }
    }
    -1
}

/// Free an image from cache
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_free_image(
    handle: *mut NeomacsDisplay,
    image_id: u32,
) -> c_int {
    // Threaded path: send command to render thread
    if let Some(ref state) = THREADED_STATE {
        let cmd = RenderCommand::ImageFree { id: image_id };
        let _ = state.emacs_comms.cmd_tx.try_send(cmd);
        return 0;
    }

    if handle.is_null() {
        return -1;
    }
    let display = &mut *handle;

    if let Some(ref mut backend) = display.winit_backend {
        if let Some(renderer) = backend.renderer_mut() {
            renderer.free_image(image_id);
            return 0;
        }
    }
    -1
}

/// Set a floating video at a specific screen position
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_set_floating_video(
    handle: *mut NeomacsDisplay,
    video_id: u32,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;

    // Remove existing floating video for this ID
    let target_scene = display.get_target_scene();
    target_scene.remove_floating_video(video_id);

    // Add new floating video
    target_scene.add_floating_video(
        video_id,
        x as f32,
        y as f32,
        width as f32,
        height as f32,
    );
}

/// Remove a floating video
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_clear_floating_video(
    handle: *mut NeomacsDisplay,
    video_id: u32,
) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;
    display.get_target_scene().remove_floating_video(video_id);
}

/// Set a floating image at a specific screen position
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_set_floating_image(
    handle: *mut NeomacsDisplay,
    image_id: u32,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;

    // Remove existing floating image for this ID
    let target_scene = display.get_target_scene();
    target_scene.remove_floating_image(image_id);

    // Add new floating image
    target_scene.add_floating_image(
        image_id,
        x as f32,
        y as f32,
        width as f32,
        height as f32,
    );
}

/// Remove a floating image
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_clear_floating_image(
    handle: *mut NeomacsDisplay,
    image_id: u32,
) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;
    display.get_target_scene().remove_floating_image(image_id);
}

/// Clear a rectangular area of the display.
/// No-op with full-frame rebuild (buffer is rebuilt from scratch each frame).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_clear_area(
    _handle: *mut NeomacsDisplay,
    _x: c_int,
    _y: c_int,
    _width: c_int,
    _height: c_int,
) {
    // No-op: with full-frame rebuild, the buffer is cleared and rebuilt each frame.
}

/// Clear only media glyphs (Image, Video, WebKit) in a rectangular area.
/// No-op with full-frame rebuild (buffer is rebuilt from scratch each frame).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_clear_media_in_area(
    _handle: *mut NeomacsDisplay,
    _x: c_int,
    _y: c_int,
    _width: c_int,
    _height: c_int,
) {
    // No-op: with full-frame rebuild, the buffer is cleared and rebuilt each frame.
}

/// Clear all glyphs - used when frame layout changes
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_clear_all_glyphs(handle: *mut NeomacsDisplay) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;
    let n_glyphs = display.frame_glyphs.glyphs.len();
    tracing::info!("neomacs_display_clear_all_glyphs: clearing {} glyphs", n_glyphs);
    display.frame_glyphs.glyphs.clear();
    display.frame_glyphs.window_regions.clear();
    display.frame_glyphs.prev_window_regions.clear();
}

/// Clear all cursors
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_clear_all_cursors(handle: *mut NeomacsDisplay) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;
    display.frame_glyphs.glyphs.retain(|g| !matches!(g, FrameGlyph::Cursor { .. }));
}

/// Clear all borders (window dividers)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_clear_all_borders(handle: *mut NeomacsDisplay) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;
    display.frame_glyphs.glyphs.retain(|g| !matches!(g, FrameGlyph::Border { .. }));
}

/// End frame and render
/// Returns 0 on success, 1 if layout changed, -1 on error
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_end_frame(handle: *mut NeomacsDisplay) -> c_int {
    if handle.is_null() {
        return -1;
    }

    let display = &mut *handle;
    let current_frame = display.frame_counter;

    // Reset frame flag
    display.in_frame = false;

    let n_glyphs = display.frame_glyphs.len();
    let n_regions = display.frame_glyphs.window_regions.len();
    debug!("end_frame: frame={}, glyphs={}, regions={}", current_frame, n_glyphs, n_regions);

    // End frame - this handles layout change detection and stale glyph removal
    let mut layout_cleared = false;
    if display.use_hybrid {
        layout_cleared = display.frame_glyphs.end_frame();
        let n_glyphs_after = display.frame_glyphs.len();
        debug!("After end_frame: {} glyphs, cleared={}", n_glyphs_after, layout_cleared);
    }

    // Build scene if it has content (legacy scene graph path)
    if !display.scene.windows.is_empty() {
        display.scene.build();
    }

    // Update animations
    display.animations.tick();

    // Render - we need to match backend type explicitly to avoid borrow conflict
    let result = match display.backend_type {
        BackendType::Tty => {
            if let Some(backend) = display.tty_backend.as_mut() {
                backend.render(&display.scene)
                    .and_then(|_| backend.present())
            } else {
                Ok(())
            }
        }
        BackendType::Wgpu => {
            if let Some(backend) = display.winit_backend.as_mut() {
                backend.render(&display.scene)
                    .and_then(|_| backend.present())
            } else {
                Ok(())
            }
        }
    };

    if let Err(e) = result {
        error!("Render error: {}", e);
        return -1;
    }

    display.scene.clear_dirty();

    if layout_cleared { 1 } else { 0 }
}
