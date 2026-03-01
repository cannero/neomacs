//! C FFI layer for integration with Emacs.
//!
//! Enable logging with: RUST_LOG=neomacs_display=debug

pub mod scene;
pub mod glyph_rows;
pub mod image;
pub mod animation;
pub mod webkit;
pub mod window;
pub mod layout;
pub mod threaded;
pub mod clipboard;
pub mod itree;
pub mod eval_bridge;

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_uint, c_double, c_void, CStr, CString};
use std::panic;
use std::ptr;
use std::sync::{Arc, Mutex};

use tracing::{debug, trace, warn, info, error};

use crate::backend::{BackendType, DisplayBackend};

// ============================================================================
// Event Callback for FFI
// ============================================================================

use crate::backend::wgpu::{
    NeomacsInputEvent, WinitBackend,
    NEOMACS_EVENT_KEY_PRESS, NEOMACS_EVENT_KEY_RELEASE,
    NEOMACS_EVENT_BUTTON_PRESS, NEOMACS_EVENT_BUTTON_RELEASE,
    NEOMACS_EVENT_MOUSE_MOVE, NEOMACS_EVENT_SCROLL,
    NEOMACS_EVENT_RESIZE, NEOMACS_EVENT_CLOSE,
    NEOMACS_EVENT_FOCUS_IN, NEOMACS_EVENT_FOCUS_OUT,
    NEOMACS_EVENT_IMAGE_DIMENSIONS_READY,
    NEOMACS_EVENT_TERMINAL_EXITED,
    NEOMACS_EVENT_MENU_SELECTION,
    NEOMACS_EVENT_FILE_DROP,
    NEOMACS_EVENT_TERMINAL_TITLE_CHANGED,
    NEOMACS_EVENT_TOOL_BAR_CLICK,
    NEOMACS_EVENT_MENU_BAR_CLICK,
};

/// Resize callback function type for C FFI
pub(crate) type ResizeCallback = extern "C" fn(user_data: *mut std::ffi::c_void, width: std::ffi::c_int, height: std::ffi::c_int);

/// Global resize callback - set by C code to receive resize events
pub(crate) static mut RESIZE_CALLBACK: Option<ResizeCallback> = None;

/// User data pointer for resize callback
pub(crate) static mut RESIZE_CALLBACK_USER_DATA: *mut std::ffi::c_void = std::ptr::null_mut();

/// Pending dropped file paths (populated by drain_input, consumed by C)
pub(crate) static DROPPED_FILES: std::sync::Mutex<Vec<Vec<String>>> = std::sync::Mutex::new(Vec::new());

/// Pending terminal title changes (populated by drain_input, consumed by C)
/// Each entry is (terminal_id, new_title).
pub(crate) static TERMINAL_TITLES: std::sync::Mutex<Vec<(u32, String)>> = std::sync::Mutex::new(Vec::new());

use crate::backend::tty::TtyBackend;
use crate::core::types::{Color, Rect};
use crate::core::scene::{Scene, WindowScene, CursorState, SceneCursorStyle};
use crate::core::animation::AnimationManager;
use crate::core::frame_glyphs::{FrameGlyphBuffer, FrameGlyph};
use crate::core::face::{Face, FaceAttributes, UnderlineStyle, BoxType};

/// Opaque handle to the display engine
pub struct NeomacsDisplay {
    pub(crate) backend_type: BackendType,
    pub(crate) tty_backend: Option<TtyBackend>,
    pub(crate) winit_backend: Option<WinitBackend>,
    pub(crate) event_loop: Option<winit::event_loop::EventLoop<crate::backend::wgpu::UserEvent>>,
    pub(crate) scene: Scene,           // The scene for rendering (legacy)
    pub(crate) frame_glyphs: FrameGlyphBuffer,  // Hybrid approach: direct glyph buffer
    pub(crate) use_hybrid: bool,       // Whether to use hybrid rendering (default: true)
    pub(crate) animations: AnimationManager,
    pub(crate) current_row_y: i32,     // Y position of current row being built
    pub(crate) current_row_x: i32,     // X position for next glyph in current row
    pub(crate) current_row_height: i32, // Height of current row
    pub(crate) current_row_ascent: i32, // Ascent of current row
    pub(crate) current_row_is_overlay: bool, // True if current row is mode-line/echo area
    pub(crate) current_window_id: i32, // ID of current window being updated
    pub(crate) current_window_x: f32,  // Current window's left X position
    pub(crate) current_window_width: f32, // Current window's width
    pub(crate) in_frame: bool,         // Whether we're currently in a frame update
    pub(crate) frame_counter: u64,     // Frame counter for tracking row updates
    pub(crate) current_render_window_id: u32, // Winit window ID being rendered to (0 = legacy rendering)
    pub(crate) faces: HashMap<u32, Face>,
}

impl NeomacsDisplay {
    pub(crate) fn get_backend(&mut self) -> Option<&mut dyn DisplayBackend> {
        match self.backend_type {
            BackendType::Tty => self.tty_backend.as_mut().map(|b| b as &mut dyn DisplayBackend),
            BackendType::Wgpu => self.winit_backend.as_mut().map(|b| b as &mut dyn DisplayBackend),
        }
    }

    /// Get the scene to render to based on current_render_window_id.
    /// Returns the winit window's scene if rendering to a window,
    /// otherwise returns the legacy scene.
    pub(crate) fn get_target_scene(&mut self) -> &mut Scene {
        if self.current_render_window_id > 0 {
            if let Some(ref mut backend) = self.winit_backend {
                if let Some(scene) = backend.get_scene_mut(self.current_render_window_id) {
                    return scene;
                }
            }
        }
        &mut self.scene
    }
}

// ============================================================================
// Initialization
// ============================================================================

// Note: neomacs_display_init() has been removed - use neomacs_display_init_threaded() instead

/// Shutdown the display engine
///
/// # Safety
/// The handle must have been returned by neomacs_display_init_threaded.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_shutdown(handle: *mut NeomacsDisplay) {
    if handle.is_null() {
        return;
    }

    let mut display = Box::from_raw(handle);

    if let Some(backend) = display.get_backend() {
        backend.shutdown();
    }

    // display is dropped here
}

// ============================================================================
// Backend Info
// ============================================================================

/// Get backend name
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_backend_name(handle: *mut NeomacsDisplay) -> *const c_char {
    if handle.is_null() {
        return b"null\0".as_ptr() as *const c_char;
    }

    let display = &mut *handle;

    match display.get_backend() {
        Some(backend) => backend.name().as_ptr() as *const c_char,
        None => b"none\0".as_ptr() as *const c_char,
    }
}

/// Check if backend is initialized
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_is_initialized(handle: *mut NeomacsDisplay) -> c_int {
    if handle.is_null() {
        return 0;
    }

    let display = &mut *handle;

    match display.get_backend() {
        Some(backend) => backend.is_initialized() as c_int,
        None => 0,
    }
}

/// Type for the resize callback function pointer from C
pub type ResizeCallbackFn = extern "C" fn(user_data: *mut c_void, width: c_int, height: c_int);

/// Set the resize callback for winit windows.
///
/// The callback will be invoked when the window is resized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_set_resize_callback(
    callback: ResizeCallbackFn,
    user_data: *mut c_void,
) {
    RESIZE_CALLBACK = Some(callback);
    RESIZE_CALLBACK_USER_DATA = user_data;
    tracing::debug!("Resize callback set");
}

// ============================================================================
// Atomic Counters
// ============================================================================

/// Atomic counter for generating image IDs in threaded mode
pub(crate) static IMAGE_ID_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// Atomic counter for generating WebKit view IDs in threaded mode
#[cfg(feature = "wpe-webkit")]
pub(crate) static WEBKIT_VIEW_ID_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// Atomic counter for generating video IDs in threaded mode
#[cfg(feature = "video")]
pub(crate) static VIDEO_ID_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// Atomic counter for generating terminal IDs in threaded mode
#[cfg(feature = "neo-term")]
pub(crate) static TERMINAL_ID_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

// ============================================================================
// Threaded State
// ============================================================================

use crate::thread_comm::{EmacsComms, EffectUpdater, InputEvent, MenuBarItem, PopupMenuItem, RenderCommand, ThreadComms, ToolBarItem};
use crate::render_thread::{RenderThread, SharedImageDimensions, SharedMonitorInfo};

/// Global state for threaded mode
pub(crate) static mut THREADED_STATE: Option<ThreadedState> = None;

pub(crate) struct ThreadedState {
    pub(crate) emacs_comms: EmacsComms,
    pub(crate) render_thread: Option<RenderThread>,
    pub(crate) display_handle: *mut NeomacsDisplay,
    /// Shared storage for image dimensions (id -> (width, height))
    /// Populated synchronously when loading images, accessible from main thread
    pub(crate) image_dimensions: Arc<Mutex<HashMap<u32, (u32, u32)>>>,
    /// Shared storage for monitor info from winit
    pub(crate) shared_monitors: SharedMonitorInfo,
    /// Shared terminal handles for cross-thread text extraction
    #[cfg(feature = "neo-term")]
    pub(crate) shared_terminals: crate::terminal::SharedTerminals,
}
