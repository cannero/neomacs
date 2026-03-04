//! VA-API DMA-BUF export for zero-copy video frames.
//!
//! This module extracts DMA-BUF file descriptors from GStreamer VA memory
//! using libva's vaExportSurfaceHandle() via GStreamer VA library bindings.

#[cfg(all(target_os = "linux", feature = "video"))]
use std::os::unix::io::RawFd;

/// DMA-BUF export parameters from VA surface.
///
/// Owns the exported file descriptors — Drop closes any that are still >= 0.
/// NOT Clone: each fd must have exactly one owner.
#[cfg(all(target_os = "linux", feature = "video"))]
#[derive(Debug)]
pub struct VaDmaBufExport {
    /// DMA-BUF file descriptors (up to 4 objects, -1 = unused/taken)
    pub fds: [RawFd; 4],
    /// Number of DRM objects returned by vaExportSurfaceHandle
    pub num_objects: u32,
    /// Number of planes
    pub num_planes: u32,
    /// Byte offset per plane
    pub offsets: [u32; 4],
    /// Stride (pitch) per plane
    pub pitches: [u32; 4],
    /// DRM fourcc format
    pub fourcc: u32,
    /// DRM modifier
    pub modifier: u64,
    /// Width in pixels
    pub width: u32,
    /// Height in pixels
    pub height: u32,
}

#[cfg(all(target_os = "linux", feature = "video"))]
impl Drop for VaDmaBufExport {
    fn drop(&mut self) {
        for i in 0..self.num_objects.min(4) as usize {
            if self.fds[i] >= 0 {
                unsafe {
                    libc::close(self.fds[i]);
                }
                tracing::trace!("VaDmaBufExport::drop closed fd[{}]={}", i, self.fds[i]);
            }
        }
    }
}

/// FFI bindings to GStreamer VA plugin and libva
#[cfg(all(target_os = "linux", feature = "video"))]
mod ffi {
    use libc::{c_int, c_uint, c_void};
    use std::os::unix::io::RawFd;

    // VA-API types
    pub type VADisplay = *mut c_void;
    pub type VASurfaceID = c_uint;
    pub type VAStatus = c_int;

    pub const VA_STATUS_SUCCESS: VAStatus = 0;
    pub const VA_INVALID_SURFACE: VASurfaceID = 0xFFFFFFFF;

    // VA_EXPORT_SURFACE flags
    pub const VA_EXPORT_SURFACE_READ_ONLY: c_uint = 0x0001;
    pub const VA_EXPORT_SURFACE_SEPARATE_LAYERS: c_uint = 0x0004;

    // Memory type for DRM PRIME export
    pub const VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2: c_uint = 0x40000000;

    /// VA surface descriptor for DRM PRIME export
    #[repr(C)]
    #[derive(Default)]
    pub struct VADRMPRIMESurfaceDescriptor {
        pub fourcc: u32,
        pub width: u32,
        pub height: u32,
        pub num_objects: u32,
        pub objects: [VADRMObject; 4],
        pub num_layers: u32,
        pub layers: [VADRMLayer; 4],
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct VADRMObject {
        pub fd: RawFd,
        pub size: u32,
        pub drm_format_modifier: u64,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct VADRMLayer {
        pub drm_format: u32,
        pub num_planes: u32,
        pub object_index: [u32; 4],
        pub offset: [u32; 4],
        pub pitch: [u32; 4],
    }

    // GStreamer types (opaque)
    pub enum GstVaDisplay {}
    pub enum GstAllocator {}

    #[link(name = "va")]
    unsafe extern "C" {
        pub fn vaExportSurfaceHandle(
            dpy: VADisplay,
            surface_id: VASurfaceID,
            mem_type: c_uint,
            flags: c_uint,
            descriptor: *mut VADRMPRIMESurfaceDescriptor,
        ) -> VAStatus;
    }

    #[link(name = "gstva-1.0")]
    unsafe extern "C" {
        // Get VASurfaceID from GstBuffer
        pub fn gst_va_buffer_get_surface(buffer: *mut gstreamer::ffi::GstBuffer) -> VASurfaceID;

        // Get GstVaDisplay from allocator
        pub fn gst_va_allocator_peek_display(allocator: *mut GstAllocator) -> *mut GstVaDisplay;

        // Get VADisplay from GstVaDisplay
        pub fn gst_va_display_get_va_dpy(display: *mut GstVaDisplay) -> VADisplay;
    }
}

/// Try to export a GStreamer buffer's VA surface as DMA-BUF
#[cfg(all(target_os = "linux", feature = "video"))]
pub fn try_export_va_dmabuf(
    buffer: &gstreamer::BufferRef,
    va_display_ptr: *mut std::ffi::c_void,
) -> Option<VaDmaBufExport> {
    use ffi::*;

    if va_display_ptr.is_null() {
        tracing::debug!("VA display is null, cannot export DMA-BUF");
        return None;
    }

    // Get VASurfaceID from buffer
    let surface_id = unsafe { gst_va_buffer_get_surface(buffer.as_mut_ptr()) };

    if surface_id == VA_INVALID_SURFACE {
        tracing::debug!("Invalid VASurfaceID from buffer");
        return None;
    }

    tracing::debug!("Got VASurfaceID: {}", surface_id);

    // Export surface as DRM PRIME (DMA-BUF)
    let mut descriptor = VADRMPRIMESurfaceDescriptor::default();

    let status = unsafe {
        vaExportSurfaceHandle(
            va_display_ptr as VADisplay,
            surface_id,
            VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
            VA_EXPORT_SURFACE_READ_ONLY | VA_EXPORT_SURFACE_SEPARATE_LAYERS,
            &mut descriptor,
        )
    };

    if status != VA_STATUS_SUCCESS {
        tracing::warn!("vaExportSurfaceHandle failed with status: {}", status);
        return None;
    }

    tracing::info!(
        "VA DMA-BUF export success: {}x{}, fourcc={:#x}, {} objects, {} layers",
        descriptor.width,
        descriptor.height,
        descriptor.fourcc,
        descriptor.num_objects,
        descriptor.num_layers
    );

    // Extract DMA-BUF info from descriptor
    let mut export = VaDmaBufExport {
        fds: [-1; 4],
        num_objects: descriptor.num_objects,
        num_planes: 0,
        offsets: [0; 4],
        pitches: [0; 4],
        fourcc: descriptor.fourcc,
        modifier: 0,
        width: descriptor.width,
        height: descriptor.height,
    };

    // Copy object fds (DMA-BUF handles)
    for i in 0..descriptor.num_objects.min(4) as usize {
        export.fds[i] = descriptor.objects[i].fd;
        if i == 0 {
            export.modifier = descriptor.objects[i].drm_format_modifier;
        }
    }

    // Get plane info from first layer
    if descriptor.num_layers > 0 {
        let layer = &descriptor.layers[0];
        export.num_planes = layer.num_planes;
        export.fourcc = layer.drm_format;
        for i in 0..layer.num_planes.min(4) as usize {
            export.offsets[i] = layer.offset[i];
            export.pitches[i] = layer.pitch[i];
        }
    }

    Some(export)
}

/// Get VA display pointer from GStreamer allocator
#[cfg(all(target_os = "linux", feature = "video"))]
pub fn get_va_display_from_allocator(
    allocator: &gstreamer::Allocator,
) -> Option<*mut std::ffi::c_void> {
    use ffi::*;
    use gstreamer::prelude::*;

    unsafe {
        // Get GstVaDisplay from allocator
        let gst_va_display = gst_va_allocator_peek_display(allocator.as_ptr() as *mut GstAllocator);

        if gst_va_display.is_null() {
            tracing::debug!("Could not get GstVaDisplay from allocator");
            return None;
        }

        // Get VADisplay from GstVaDisplay
        let va_dpy = gst_va_display_get_va_dpy(gst_va_display);

        if va_dpy.is_null() {
            tracing::debug!("Could not get VADisplay from GstVaDisplay");
            return None;
        }

        Some(va_dpy as *mut std::ffi::c_void)
    }
}

/// Get VA display from GStreamer memory
#[cfg(all(target_os = "linux", feature = "video"))]
pub fn get_va_display_from_memory(memory: &gstreamer::Memory) -> Option<*mut std::ffi::c_void> {
    let allocator = memory.allocator()?;
    get_va_display_from_allocator(&allocator)
}
