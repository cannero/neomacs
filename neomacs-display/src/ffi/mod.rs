//! C FFI layer for integration with Emacs.
//!
//! Enable logging with: RUST_LOG=neomacs_display=debug

pub mod animation;
pub mod clipboard;
pub mod eval_bridge;
pub mod glyph_rows;
pub mod image;
pub mod itree;
pub mod layout;
pub mod scene;
pub mod threaded;
pub mod webkit;
pub mod window;

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_double, c_int, c_uint, c_void};
use std::panic;
use std::ptr;
use std::sync::{Arc, Mutex};

use tracing::{debug, error, info, trace, warn};

use crate::backend::{BackendType, DisplayBackend};

// ============================================================================
// Event Callback for FFI
// ============================================================================

use crate::backend::wgpu::{
    NEOMACS_EVENT_BUTTON_PRESS, NEOMACS_EVENT_BUTTON_RELEASE, NEOMACS_EVENT_CLOSE,
    NEOMACS_EVENT_FILE_DROP, NEOMACS_EVENT_FOCUS_IN, NEOMACS_EVENT_FOCUS_OUT,
    NEOMACS_EVENT_IMAGE_DIMENSIONS_READY, NEOMACS_EVENT_KEY_PRESS, NEOMACS_EVENT_KEY_RELEASE,
    NEOMACS_EVENT_MENU_BAR_CLICK, NEOMACS_EVENT_MENU_SELECTION, NEOMACS_EVENT_MOUSE_MOVE,
    NEOMACS_EVENT_RESIZE, NEOMACS_EVENT_SCROLL, NEOMACS_EVENT_TAB_BAR_CLICK,
    NEOMACS_EVENT_TERMINAL_EXITED, NEOMACS_EVENT_TERMINAL_TITLE_CHANGED,
    NEOMACS_EVENT_TOOL_BAR_CLICK, NeomacsInputEvent, WinitBackend,
};

/// Resize callback function type for C FFI
pub(crate) type ResizeCallback = extern "C" fn(
    user_data: *mut std::ffi::c_void,
    width: std::ffi::c_int,
    height: std::ffi::c_int,
);

/// Global resize callback - set by C code to receive resize events
pub(crate) static mut RESIZE_CALLBACK: Option<ResizeCallback> = None;

/// User data pointer for resize callback
pub(crate) static mut RESIZE_CALLBACK_USER_DATA: *mut std::ffi::c_void = std::ptr::null_mut();

/// Pending dropped file paths (populated by drain_input, consumed by C)
pub(crate) static DROPPED_FILES: std::sync::Mutex<Vec<Vec<String>>> =
    std::sync::Mutex::new(Vec::new());

/// Pending terminal title changes (populated by drain_input, consumed by C)
/// Each entry is (terminal_id, new_title).
pub(crate) static TERMINAL_TITLES: std::sync::Mutex<Vec<(u32, String)>> =
    std::sync::Mutex::new(Vec::new());

use crate::backend::tty::TtyBackend;
use crate::core::animation::AnimationManager;
use crate::core::face::{BoxType, Face, FaceAttributes, UnderlineStyle};
use crate::core::frame_glyphs::{
    FrameGlyph, FrameGlyphBuffer, WindowEffectHint, WindowInfo, WindowTransitionHint,
    WindowTransitionKind,
};
use crate::core::scene::{CursorState, Scene, SceneCursorStyle, WindowScene};
use crate::core::types::{Color, Rect};

/// Opaque handle to the display engine
pub struct NeomacsDisplay {
    pub(crate) backend_type: BackendType,
    pub(crate) tty_backend: Option<TtyBackend>,
    pub(crate) winit_backend: Option<WinitBackend>,
    pub(crate) event_loop: Option<winit::event_loop::EventLoop<crate::backend::wgpu::UserEvent>>,
    pub(crate) scene: Scene, // The scene for rendering (legacy)
    pub(crate) frame_glyphs: FrameGlyphBuffer, // Hybrid approach: direct glyph buffer
    pub(crate) animations: AnimationManager,
    pub(crate) current_window_id: i32, // ID of current window being updated
    pub(crate) current_window_x: f32,  // Current window's left X position
    pub(crate) current_window_width: f32, // Current window's width
    pub(crate) in_frame: bool,         // Whether we're currently in a frame update
    pub(crate) frame_counter: u64,     // Frame counter for tracking row updates
    pub(crate) current_render_window_id: u32, // Winit window ID being rendered to (0 = legacy rendering)
    pub(crate) faces: HashMap<u32, Face>,
    pub(crate) transition_prev_window_infos: HashMap<i64, WindowInfo>,
    pub(crate) transition_curr_window_infos: HashMap<i64, WindowInfo>,
    pub(crate) prev_selected_window_id: i64,
    pub(crate) prev_background: Option<(f32, f32, f32, f32)>,
}

impl NeomacsDisplay {
    pub(crate) fn get_backend(&mut self) -> Option<&mut dyn DisplayBackend> {
        match self.backend_type {
            BackendType::Tty => self
                .tty_backend
                .as_mut()
                .map(|b| b as &mut dyn DisplayBackend),
            BackendType::Wgpu => self
                .winit_backend
                .as_mut()
                .map(|b| b as &mut dyn DisplayBackend),
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

    pub(crate) fn begin_transition_hint_frame(&mut self) {
        std::mem::swap(
            &mut self.transition_prev_window_infos,
            &mut self.transition_curr_window_infos,
        );
        self.transition_curr_window_infos.clear();
    }

    pub(crate) fn record_transition_hint_window(&mut self, info: &WindowInfo) {
        if let Some(prev) = self.transition_prev_window_infos.get(&info.window_id) {
            if let Some(hint) = FrameGlyphBuffer::derive_transition_hint(prev, info) {
                self.frame_glyphs.add_transition_hint(hint);
            }
        }
        self.record_effect_hints_window(info);
        self.transition_curr_window_infos
            .insert(info.window_id, info.clone());
    }

    pub(crate) fn record_effect_hints_window(&mut self, info: &WindowInfo) {
        if info.is_minibuffer {
            return;
        }

        let Some(prev) = self.transition_prev_window_infos.get(&info.window_id) else {
            return;
        };

        if prev.buffer_id == 0 || info.buffer_id == 0 {
            return;
        }

        if prev.buffer_id != info.buffer_id {
            self.frame_glyphs
                .add_effect_hint(WindowEffectHint::TextFadeIn {
                    window_id: info.window_id,
                    bounds: info.bounds,
                });
            return;
        }

        if prev.window_start != info.window_start {
            let direction = if info.window_start > prev.window_start {
                1
            } else {
                -1
            };
            let delta = (info.window_start - prev.window_start).unsigned_abs() as f32;
            self.frame_glyphs
                .add_effect_hint(WindowEffectHint::TextFadeIn {
                    window_id: info.window_id,
                    bounds: info.bounds,
                });
            self.frame_glyphs
                .add_effect_hint(WindowEffectHint::ScrollLineSpacing {
                    window_id: info.window_id,
                    bounds: info.bounds,
                    direction,
                });
            self.frame_glyphs
                .add_effect_hint(WindowEffectHint::ScrollMomentum {
                    window_id: info.window_id,
                    bounds: info.bounds,
                    direction,
                });
            self.frame_glyphs
                .add_effect_hint(WindowEffectHint::ScrollVelocityFade {
                    window_id: info.window_id,
                    bounds: info.bounds,
                    delta,
                });
        }
    }

    fn find_window_cursor_y(&self, info: &WindowInfo) -> Option<f32> {
        for glyph in &self.frame_glyphs.glyphs {
            if let FrameGlyph::Cursor { x, y, style, .. } = glyph {
                if *x >= info.bounds.x
                    && *x < info.bounds.x + info.bounds.width
                    && *y >= info.bounds.y
                    && *y < info.bounds.y + info.bounds.height
                    && !style.is_hollow()
                {
                    return Some(*y);
                }
            }
        }
        None
    }

    pub(crate) fn finalize_effect_hints(&mut self) {
        if self.transition_curr_window_infos.is_empty() {
            return;
        }

        for (window_id, info) in &self.transition_curr_window_infos {
            if info.is_minibuffer {
                continue;
            }
            let Some(prev) = self.transition_prev_window_infos.get(window_id) else {
                continue;
            };
            if prev.buffer_id == 0 || info.buffer_id == 0 {
                continue;
            }
            if prev.buffer_id == info.buffer_id
                && prev.window_start == info.window_start
                && prev.buffer_size != info.buffer_size
            {
                if let Some(edit_y) = self.find_window_cursor_y(info) {
                    let offset = if info.buffer_size > prev.buffer_size {
                        -info.char_height
                    } else {
                        info.char_height
                    };
                    self.frame_glyphs
                        .add_effect_hint(WindowEffectHint::LineAnimation {
                            window_id: info.window_id,
                            bounds: info.bounds,
                            edit_y: edit_y + info.char_height,
                            offset,
                        });
                }
            }
        }

        let new_selected = self
            .frame_glyphs
            .window_infos
            .iter()
            .find(|info| info.selected && !info.is_minibuffer)
            .map(|info| (info.window_id, info.bounds));
        if let Some((window_id, bounds)) = new_selected {
            if self.prev_selected_window_id != 0 && self.prev_selected_window_id != window_id {
                self.frame_glyphs
                    .add_effect_hint(WindowEffectHint::WindowSwitchFade { window_id, bounds });
            }
            self.prev_selected_window_id = window_id;
        }

        let bg = &self.frame_glyphs.background;
        let new_bg = (bg.r, bg.g, bg.b, bg.a);
        if let Some(old_bg) = self.prev_background {
            let dr = (new_bg.0 - old_bg.0).abs();
            let dg = (new_bg.1 - old_bg.1).abs();
            let db = (new_bg.2 - old_bg.2).abs();
            if dr > 0.02 || dg > 0.02 || db > 0.02 {
                let full_h = self
                    .frame_glyphs
                    .window_infos
                    .iter()
                    .find(|w| w.is_minibuffer)
                    .map_or(self.frame_glyphs.height, |w| w.bounds.y);
                self.frame_glyphs
                    .add_effect_hint(WindowEffectHint::ThemeTransition {
                        bounds: Rect::new(0.0, 0.0, self.frame_glyphs.width, full_h),
                    });
            }
        }
        self.prev_background = Some(new_bg);
    }

    pub(crate) fn finalize_transition_hints(&mut self) {
        if self.transition_prev_window_infos.is_empty() {
            return;
        }

        let prev_non_mini: std::collections::HashSet<i64> = self
            .transition_prev_window_infos
            .iter()
            .filter(|(_, info)| !info.is_minibuffer)
            .map(|(window_id, _)| *window_id)
            .collect();
        let curr_non_mini: std::collections::HashSet<i64> = self
            .transition_curr_window_infos
            .iter()
            .filter(|(_, info)| !info.is_minibuffer)
            .map(|(window_id, _)| *window_id)
            .collect();

        if prev_non_mini.is_empty() || curr_non_mini.is_empty() || prev_non_mini == curr_non_mini {
            return;
        }

        if self
            .frame_glyphs
            .transition_hints
            .iter()
            .any(|hint| hint.window_id == 0 && matches!(hint.kind, WindowTransitionKind::Crossfade))
        {
            return;
        }

        // Window topology changed (split/delete): request a full-frame crossfade,
        // excluding the minibuffer area to avoid echo-area overlap artifacts.
        let full_h = self
            .frame_glyphs
            .window_infos
            .iter()
            .find(|w| w.is_minibuffer)
            .map_or(self.frame_glyphs.height, |w| w.bounds.y);

        self.frame_glyphs.add_transition_hint(WindowTransitionHint {
            window_id: 0,
            bounds: Rect::new(0.0, 0.0, self.frame_glyphs.width, full_h),
            kind: WindowTransitionKind::Crossfade,
            effect: None,
            easing: None,
        });
    }

    pub(crate) fn finalize_frame_hints(&mut self) {
        self.finalize_transition_hints();
        self.finalize_effect_hints();
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
pub unsafe extern "C" fn neomacs_display_backend_name(
    handle: *mut NeomacsDisplay,
) -> *const c_char {
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
pub(crate) static IMAGE_ID_COUNTER: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(1);

/// Atomic counter for generating WebKit view IDs in threaded mode
#[cfg(feature = "wpe-webkit")]
pub(crate) static WEBKIT_VIEW_ID_COUNTER: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(1);

/// Atomic counter for generating video IDs in threaded mode
#[cfg(feature = "video")]
pub(crate) static VIDEO_ID_COUNTER: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(1);

/// Atomic counter for generating terminal IDs in threaded mode
#[cfg(feature = "neo-term")]
pub(crate) static TERMINAL_ID_COUNTER: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(1);

// ============================================================================
// Threaded State
// ============================================================================

use crate::render_thread::{RenderThread, SharedImageDimensions, SharedMonitorInfo};
use crate::thread_comm::{
    EffectUpdater, EmacsComms, InputEvent, MenuBarItem, PopupMenuItem, RenderCommand, TabBarItem,
    ThreadComms, ToolBarItem,
};

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
