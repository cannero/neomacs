//! Render thread implementation.
//!
//! Owns winit event loop, wgpu, GLib/WebKit. Runs at native VSync.

mod app_handler;
mod bootstrap;
pub(crate) mod child_frames;
mod command_processing;
mod cursor;
mod cursor_runtime;
mod frame_state;
mod input;
mod lifecycle;
mod media;
pub(crate) mod multi_window;
mod render_pass;
mod surface_readback;
#[cfg(test)]
mod tests;
mod transitions;
mod window_events;

pub(crate) use bootstrap::run_render_loop;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

#[cfg(target_os = "linux")]
use winit::platform::wayland::EventLoopBuilderExtWayland;
#[cfg(target_os = "linux")]
use winit::platform::x11::EventLoopBuilderExtX11;

use crate::backend::wgpu::{
    NEOMACS_CTRL_MASK, NEOMACS_META_MASK, NEOMACS_SHIFT_MASK, NEOMACS_SUPER_MASK,
};
use crate::core::face::Face;
use crate::core::frame_glyphs::{FrameGlyph, FrameGlyphBuffer, GlyphRowRole};
use crate::core::types::{
    AnimatedCursor, Color, CursorAnimStyle, Rect, ease_in_out_cubic, ease_linear, ease_out_cubic,
    ease_out_expo, ease_out_quad,
};
use crate::thread_comm::{
    InputEvent, MenuBarItem, PopupMenuItem, RenderCommand, RenderComms, ToolBarItem,
};
use cursor::{CornerSpring, CursorState, CursorTarget};
use neomacs_display_protocol::EffectsConfig;
pub(crate) use neomacs_renderer_wgpu::{MenuPanel, PopupMenuState, TooltipState};
use neomacs_renderer_wgpu::{WgpuGlyphAtlas, WgpuRenderer};
use transitions::{CrossfadeTransition, ScrollTransition, TransitionState};

#[cfg(all(feature = "wpe-webkit", wpe_platform_available))]
use crate::backend::wpe::sys::platform as plat;

#[cfg(feature = "wpe-webkit")]
use crate::backend::wpe::{WpeBackend, WpeWebView};

// All GPU caches (image, video, webkit) are managed by WgpuRenderer

/// Shared storage for image dimensions accessible from both threads
pub type SharedImageDimensions = Arc<Mutex<HashMap<u32, (u32, u32)>>>;

/// Monitor information collected from winit
#[derive(Debug, Clone)]
pub struct MonitorInfo {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub scale: f64,
    pub width_mm: i32,
    pub height_mm: i32,
    pub name: Option<String>,
}

/// Shared storage for monitor info accessible from both threads.
/// The Condvar is notified once monitors have been populated.
pub type SharedMonitorInfo = Arc<(Mutex<Vec<MonitorInfo>>, std::sync::Condvar)>;

/// Render thread state
pub struct RenderThread {
    handle: Option<JoinHandle<()>>,
}

impl RenderThread {
    /// Spawn the render thread
    pub fn spawn(
        comms: RenderComms,
        width: u32,
        height: u32,
        title: String,
        image_dimensions: SharedImageDimensions,
        shared_monitors: SharedMonitorInfo,
        #[cfg(feature = "neo-term")] shared_terminals: crate::terminal::SharedTerminals,
    ) -> Self {
        let handle = thread::spawn(move || {
            run_render_loop(
                comms,
                width,
                height,
                title,
                image_dimensions,
                shared_monitors,
                #[cfg(feature = "neo-term")]
                shared_terminals,
            );
        });

        Self {
            handle: Some(handle),
        }
    }

    /// Wait for render thread to finish
    pub fn join(mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(feature = "wpe-webkit")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebKitImportPolicy {
    /// Prefer raw pixel upload first, fallback to DMA-BUF.
    PixelsFirst,
    /// Prefer DMA-BUF import first, fallback to raw pixels.
    DmaBufFirst,
    /// Default compatibility mode (currently PixelsFirst).
    Auto,
}

#[cfg(feature = "wpe-webkit")]
impl WebKitImportPolicy {
    fn from_env() -> Self {
        match std::env::var("NEOMACS_WEBKIT_IMPORT").ok().as_deref() {
            Some("dmabuf-first") | Some("dmabuf") | Some("dma-buf-first") => {
                tracing::info!("NEOMACS_WEBKIT_IMPORT=dmabuf-first");
                Self::DmaBufFirst
            }
            Some("pixels-first") | Some("pixels") => {
                tracing::info!("NEOMACS_WEBKIT_IMPORT=pixels-first");
                Self::PixelsFirst
            }
            Some("auto") => {
                tracing::info!("NEOMACS_WEBKIT_IMPORT=auto (effective: pixels-first)");
                Self::Auto
            }
            Some(val) => {
                tracing::warn!(
                    "NEOMACS_WEBKIT_IMPORT={}: unrecognized value, defaulting to auto (effective: pixels-first)",
                    val
                );
                Self::Auto
            }
            None => {
                tracing::info!("NEOMACS_WEBKIT_IMPORT not set (effective: pixels-first)");
                Self::Auto
            }
        }
    }

    fn effective(self) -> Self {
        match self {
            Self::Auto => Self::PixelsFirst,
            other => other,
        }
    }
}

/// FPS counter and frame time tracking state.
struct FpsCounter {
    enabled: bool,
    last_instant: std::time::Instant,
    frame_count: u32,
    display_value: f32,
    frame_time_ms: f32,
    render_start: std::time::Instant,
}

impl Default for FpsCounter {
    fn default() -> Self {
        Self {
            enabled: false,
            last_instant: std::time::Instant::now(),
            frame_count: 0,
            display_value: 0.0,
            frame_time_ms: 0.0,
            render_start: std::time::Instant::now(),
        }
    }
}

/// Borderless window chrome state (title bar, resize edges, decorations).
struct WindowChrome {
    decorations_enabled: bool,
    resize_edge: Option<winit::window::ResizeDirection>,
    title: String,
    titlebar_height: f32,
    titlebar_hover: u32,
    last_titlebar_click: std::time::Instant,
    is_fullscreen: bool,
    corner_radius: f32,
}

impl Default for WindowChrome {
    fn default() -> Self {
        Self {
            decorations_enabled: true,
            resize_edge: None,
            title: String::from("neomacs"),
            titlebar_height: 30.0,
            titlebar_hover: 0,
            last_titlebar_click: std::time::Instant::now(),
            is_fullscreen: false,
            corner_radius: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImeCursorArea {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

struct RenderApp {
    comms: RenderComms,
    window: Option<Arc<Window>>,
    current_frame: Option<FrameGlyphBuffer>,
    width: u32,
    height: u32,
    title: String,

    // wgpu state
    renderer: Option<WgpuRenderer>,
    surface: Option<wgpu::Surface<'static>>,
    surface_config: Option<wgpu::SurfaceConfiguration>,
    device: Option<Arc<wgpu::Device>>,
    queue: Option<Arc<wgpu::Queue>>,
    glyph_atlas: Option<WgpuGlyphAtlas>,

    // Face cache built from frame data
    faces: HashMap<u32, Face>,

    // Display scale factor (physical pixels / logical pixels)
    scale_factor: f64,

    // Current modifier state (NEOMACS_*_MASK flags)
    modifiers: u32,

    // Last known cursor position
    mouse_pos: (f32, f32),
    /// Whether the mouse cursor is hidden during keyboard input
    mouse_hidden_for_typing: bool,

    // Shared image dimensions (written here, read from main thread)
    image_dimensions: SharedImageDimensions,

    // Frame dirty flag: set when new frame data arrives, cleared after render
    frame_dirty: bool,

    // Cursor state (blink, animation, size transition)
    cursor: CursorState,

    // All visual effect configurations
    effects: EffectsConfig,

    // Window transition state (crossfade, scroll)
    transitions: TransitionState,

    // WebKit state (video cache is managed by renderer)
    #[cfg(feature = "wpe-webkit")]
    wpe_backend: Option<WpeBackend>,

    #[cfg(feature = "wpe-webkit")]
    webkit_views: HashMap<u32, WpeWebView>,

    #[cfg(feature = "wpe-webkit")]
    webkit_import_policy: WebKitImportPolicy,

    // Floating WebKit overlays (position/size from C side, rendered on render thread)
    #[cfg(feature = "wpe-webkit")]
    floating_webkits: Vec<crate::core::scene::FloatingWebKit>,

    // Terminal manager (neo-term)
    #[cfg(feature = "neo-term")]
    terminal_manager: crate::terminal::TerminalManager,
    #[cfg(feature = "neo-term")]
    shared_terminals: crate::terminal::SharedTerminals,

    // Multi-window manager (secondary OS windows for top-level frames)
    multi_windows: multi_window::MultiWindowManager,
    // wgpu adapter (needed for creating surfaces on new windows)
    adapter: Option<wgpu::Adapter>,

    // Child frames (posframe, which-key-posframe, etc.)
    child_frames: child_frames::ChildFrameManager,
    // Child frame visual style
    child_frame_corner_radius: f32,
    child_frame_shadow_enabled: bool,
    child_frame_shadow_layers: u32,
    child_frame_shadow_offset: f32,
    child_frame_shadow_opacity: f32,

    // Active popup menu (shown by x-popup-menu)
    popup_menu: Option<PopupMenuState>,

    // Active tooltip overlay
    tooltip: Option<TooltipState>,

    // Menu bar state
    menu_bar_items: Vec<MenuBarItem>,
    menu_bar_height: f32,
    menu_bar_fg: (f32, f32, f32),
    menu_bar_bg: (f32, f32, f32),
    menu_bar_hovered: Option<u32>,
    menu_bar_active: Option<u32>,

    // Toolbar state
    toolbar_items: Vec<ToolBarItem>,
    toolbar_height: f32,
    toolbar_fg: (f32, f32, f32),
    toolbar_bg: (f32, f32, f32),
    toolbar_icon_textures: HashMap<String, u32>, // icon_name → image_id in image_cache
    toolbar_hovered: Option<u32>,
    toolbar_pressed: Option<u32>,
    toolbar_icon_size: u32,
    toolbar_padding: u32,

    // Visual bell state (flash overlay)
    visual_bell_start: Option<std::time::Instant>,

    // IME state
    ime_enabled: bool,
    ime_preedit_active: bool,
    ime_preedit_text: String,
    last_ime_cursor_area: Option<ImeCursorArea>,

    // UI overlay state
    scroll_indicators_enabled: bool,

    // Window chrome (borderless title bar, resize, decorations)
    chrome: WindowChrome,
    // FPS counter state
    fps: FpsCounter,
    /// Extra line spacing in pixels (added between rows)
    extra_line_spacing: f32,
    /// Extra letter spacing in pixels (added between characters)
    extra_letter_spacing: f32,
    last_activity_time: std::time::Instant,
    idle_dim_current_alpha: f32, // current dimming alpha 0.0 (none) to opacity (full)
    idle_dim_active: bool,       // true when dimmed or fading
    /// Key press timestamps for WPM calculation
    key_press_times: Vec<std::time::Instant>,
    /// Smoothed WPM value for display
    displayed_wpm: f32,

    /// Shared monitor info (populated in resumed(), read from FFI thread)
    shared_monitors: Option<SharedMonitorInfo>,
    monitors_populated: bool,
    debug_first_frame_readback_pending: bool,
}

impl RenderApp {
    fn new(
        comms: RenderComms,
        width: u32,
        height: u32,
        title: String,
        image_dimensions: SharedImageDimensions,
        shared_monitors: SharedMonitorInfo,
        #[cfg(feature = "neo-term")] shared_terminals: crate::terminal::SharedTerminals,
    ) -> Self {
        #[cfg(feature = "wpe-webkit")]
        let webkit_import_policy = WebKitImportPolicy::from_env();

        Self {
            comms,
            window: None,
            current_frame: None,
            width,
            height,
            title,
            scale_factor: 1.0,
            renderer: None,
            surface: None,
            surface_config: None,
            device: None,
            queue: None,
            glyph_atlas: None,
            faces: HashMap::new(),
            modifiers: 0,
            mouse_pos: (0.0, 0.0),
            mouse_hidden_for_typing: false,
            image_dimensions,
            frame_dirty: false,
            cursor: CursorState::default(),
            effects: EffectsConfig::default(),
            transitions: TransitionState::default(),
            #[cfg(feature = "wpe-webkit")]
            wpe_backend: None,
            #[cfg(feature = "wpe-webkit")]
            webkit_views: HashMap::new(),
            #[cfg(feature = "wpe-webkit")]
            webkit_import_policy,
            #[cfg(feature = "wpe-webkit")]
            floating_webkits: Vec::new(),
            #[cfg(feature = "neo-term")]
            terminal_manager: crate::terminal::TerminalManager::new(),
            #[cfg(feature = "neo-term")]
            shared_terminals,
            multi_windows: multi_window::MultiWindowManager::new(),
            adapter: None,
            child_frames: child_frames::ChildFrameManager::new(),
            child_frame_corner_radius: 8.0,
            child_frame_shadow_enabled: true,
            child_frame_shadow_layers: 4,
            child_frame_shadow_offset: 2.0,
            child_frame_shadow_opacity: 0.3,
            popup_menu: None,
            tooltip: None,
            menu_bar_items: Vec::new(),
            menu_bar_height: 0.0,
            menu_bar_fg: (0.8, 0.8, 0.8),
            menu_bar_bg: (0.15, 0.15, 0.15),
            menu_bar_hovered: None,
            menu_bar_active: None,
            toolbar_items: Vec::new(),
            toolbar_height: 0.0,
            toolbar_fg: (0.8, 0.8, 0.8),
            toolbar_bg: (0.15, 0.15, 0.15),
            toolbar_icon_textures: HashMap::new(),
            toolbar_hovered: None,
            toolbar_pressed: None,
            toolbar_icon_size: 20,
            toolbar_padding: 6,
            visual_bell_start: None,
            ime_enabled: false,
            ime_preedit_active: false,
            ime_preedit_text: String::new(),
            last_ime_cursor_area: None,
            scroll_indicators_enabled: false,
            chrome: WindowChrome::default(),
            fps: FpsCounter::default(),
            extra_line_spacing: 0.0,
            extra_letter_spacing: 0.0,
            key_press_times: Vec::new(),
            displayed_wpm: 0.0,
            last_activity_time: std::time::Instant::now(),
            idle_dim_current_alpha: 0.0,
            idle_dim_active: false,

            shared_monitors: Some(shared_monitors),
            monitors_populated: false,
            debug_first_frame_readback_pending: std::env::var_os(
                "NEOMACS_DEBUG_FIRST_FRAME_READBACK",
            )
            .is_some(),
        }
    }
}
