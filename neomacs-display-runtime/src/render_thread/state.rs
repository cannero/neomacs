use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use winit::dpi::{LogicalSize, PhysicalSize, Size};
use winit::window::Window;

use crate::core::face::Face;
use crate::core::frame_glyphs::FrameGlyphBuffer;
pub use crate::thread_comm::MonitorInfo;
use crate::thread_comm::{MenuBarItem, RenderComms, TabBarItem, ToolBarItem};
use neomacs_display_protocol::EffectsConfig;
use neomacs_renderer_wgpu::{PopupMenuState, TooltipState, WgpuGlyphAtlas, WgpuRenderer};

use super::child_frames::ChildFrameManager;
use super::cursor::CursorState;
use super::multi_window::MultiWindowManager;
use super::transitions::TransitionState;

#[cfg(feature = "wpe-webkit")]
use crate::backend::wpe::{WpeBackend, WpeWebView};

/// Shared storage for image dimensions accessible from both threads.
pub type SharedImageDimensions = Arc<(Mutex<HashMap<u32, (u32, u32)>>, Condvar)>;

/// Shared storage for monitor info accessible from both threads.
/// The Condvar is notified once monitors have been populated.
pub type SharedMonitorInfo = Arc<(Mutex<Vec<MonitorInfo>>, std::sync::Condvar)>;

pub(super) fn backend_uses_winit_logical_pixels() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("WAYLAND_DISPLAY").is_some()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

pub(super) fn effective_window_scale_factor(raw_scale_factor: f64) -> f64 {
    if backend_uses_winit_logical_pixels() {
        raw_scale_factor
    } else {
        1.0
    }
}

pub(super) fn window_size_from_emacs_pixels(width: u32, height: u32) -> Size {
    if backend_uses_winit_logical_pixels() {
        Size::Logical(LogicalSize::new(width as f64, height as f64))
    } else {
        Size::Physical(PhysicalSize::new(width, height))
    }
}

pub(super) fn emacs_pixels_from_window_size(
    width: u32,
    height: u32,
    scale_factor: f64,
) -> (u32, u32) {
    if backend_uses_winit_logical_pixels() {
        (
            (width as f64 / scale_factor) as u32,
            (height as f64 / scale_factor) as u32,
        )
    } else {
        (width, height)
    }
}

#[cfg(feature = "wpe-webkit")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WebKitImportPolicy {
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

    pub(super) fn effective(self) -> Self {
        match self {
            Self::Auto => Self::PixelsFirst,
            other => other,
        }
    }
}

/// FPS counter and frame time tracking state.
pub(super) struct FpsCounter {
    pub(super) enabled: bool,
    pub(super) last_instant: Instant,
    pub(super) frame_count: u32,
    pub(super) display_value: f32,
    pub(super) frame_time_ms: f32,
    pub(super) render_start: Instant,
}

impl Default for FpsCounter {
    fn default() -> Self {
        Self {
            enabled: false,
            last_instant: Instant::now(),
            frame_count: 0,
            display_value: 0.0,
            frame_time_ms: 0.0,
            render_start: Instant::now(),
        }
    }
}

/// Borderless window chrome state (title bar, resize edges, decorations).
pub(super) struct WindowChrome {
    pub(super) decorations_enabled: bool,
    pub(super) resize_edge: Option<winit::window::ResizeDirection>,
    pub(super) title: String,
    pub(super) titlebar_height: f32,
    pub(super) titlebar_hover: u32,
    pub(super) last_titlebar_click: Instant,
    pub(super) is_fullscreen: bool,
    pub(super) corner_radius: f32,
}

impl Default for WindowChrome {
    fn default() -> Self {
        Self {
            decorations_enabled: true,
            resize_edge: None,
            title: String::from("neomacs"),
            titlebar_height: 30.0,
            titlebar_hover: 0,
            last_titlebar_click: Instant::now(),
            is_fullscreen: false,
            corner_radius: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ImeCursorArea {
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) width: u32,
    pub(super) height: u32,
}

pub(super) struct RenderApp {
    pub(super) comms: RenderComms,
    pub(super) window: Option<Arc<Window>>,
    pub(super) current_frame: Option<FrameGlyphBuffer>,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) title: String,

    // wgpu state
    pub(super) renderer: Option<WgpuRenderer>,
    pub(super) surface: Option<wgpu::Surface<'static>>,
    pub(super) surface_config: Option<wgpu::SurfaceConfiguration>,
    pub(super) device: Option<Arc<wgpu::Device>>,
    pub(super) queue: Option<Arc<wgpu::Queue>>,
    pub(super) glyph_atlas: Option<WgpuGlyphAtlas>,

    // Face cache built from frame data
    pub(super) faces: HashMap<u32, Face>,

    // Display scale factor (physical pixels / logical pixels)
    pub(super) scale_factor: f64,

    // Current modifier state (NEOMACS_*_MASK flags)
    pub(super) modifiers: u32,

    // Last known cursor position
    pub(super) mouse_pos: (f32, f32),
    /// Whether the mouse cursor is hidden during keyboard input
    pub(super) mouse_hidden_for_typing: bool,

    // Shared image dimensions (written here, read from main thread)
    pub(super) image_dimensions: SharedImageDimensions,

    // Frame dirty flag: set when new frame data arrives, cleared after render
    pub(super) frame_dirty: bool,

    // Cursor state (blink, animation, size transition)
    pub(super) cursor: CursorState,

    // All visual effect configurations
    pub(super) effects: EffectsConfig,

    // Window transition state (crossfade, scroll)
    pub(super) transitions: TransitionState,

    // WebKit state (video cache is managed by renderer)
    #[cfg(feature = "wpe-webkit")]
    pub(super) wpe_backend: Option<WpeBackend>,

    #[cfg(feature = "wpe-webkit")]
    pub(super) webkit_views: HashMap<u32, WpeWebView>,

    #[cfg(feature = "wpe-webkit")]
    pub(super) webkit_import_policy: WebKitImportPolicy,

    // Floating WebKit overlays (position/size from C side, rendered on render thread)
    #[cfg(feature = "wpe-webkit")]
    pub(super) floating_webkits: Vec<crate::core::scene::FloatingWebKit>,

    // Terminal manager (neo-term)
    #[cfg(feature = "neo-term")]
    pub(super) terminal_manager: crate::terminal::TerminalManager,
    #[cfg(feature = "neo-term")]
    pub(super) shared_terminals: crate::terminal::SharedTerminals,

    // Multi-window manager (secondary OS windows for top-level frames)
    pub(super) multi_windows: MultiWindowManager,
    // wgpu adapter (needed for creating surfaces on new windows)
    pub(super) adapter: Option<wgpu::Adapter>,

    // Child frames (posframe, which-key-posframe, etc.)
    pub(super) child_frames: ChildFrameManager,
    // Child frame visual style
    pub(super) child_frame_corner_radius: f32,
    pub(super) child_frame_shadow_enabled: bool,
    pub(super) child_frame_shadow_layers: u32,
    pub(super) child_frame_shadow_offset: f32,
    pub(super) child_frame_shadow_opacity: f32,

    // Active popup menu (shown by x-popup-menu)
    pub(super) popup_menu: Option<PopupMenuState>,

    // Active tooltip overlay
    pub(super) tooltip: Option<TooltipState>,

    // Menu bar state
    pub(super) menu_bar_items: Vec<MenuBarItem>,
    pub(super) menu_bar_height: f32,
    pub(super) menu_bar_fg: (f32, f32, f32),
    pub(super) menu_bar_bg: (f32, f32, f32),
    pub(super) menu_bar_hovered: Option<u32>,
    pub(super) menu_bar_active: Option<u32>,

    // Tab bar state (items + height kept for click hit-testing)
    pub(super) tab_bar_items: Vec<TabBarItem>,
    pub(super) tab_bar_height: f32,
    pub(super) tab_bar_hovered: Option<u32>,
    pub(super) tab_bar_pressed: Option<u32>,

    // Toolbar state
    pub(super) toolbar_items: Vec<ToolBarItem>,
    pub(super) toolbar_height: f32,
    pub(super) toolbar_fg: (f32, f32, f32),
    pub(super) toolbar_bg: (f32, f32, f32),
    pub(super) toolbar_icon_textures: HashMap<String, u32>,
    pub(super) toolbar_hovered: Option<u32>,
    pub(super) toolbar_pressed: Option<u32>,
    pub(super) toolbar_icon_size: u32,
    pub(super) toolbar_padding: u32,

    // Visual bell state (flash overlay)
    pub(super) visual_bell_start: Option<Instant>,

    // IME state
    pub(super) ime_enabled: bool,
    pub(super) ime_preedit_active: bool,
    pub(super) ime_preedit_text: String,
    pub(super) last_ime_cursor_area: Option<ImeCursorArea>,

    // UI overlay state
    pub(super) scroll_indicators_enabled: bool,

    // Window chrome (borderless title bar, resize, decorations)
    pub(super) chrome: WindowChrome,
    // FPS counter state
    pub(super) fps: FpsCounter,
    /// Extra line spacing in pixels (added between rows)
    pub(super) extra_line_spacing: f32,
    /// Extra letter spacing in pixels (added between characters)
    pub(super) extra_letter_spacing: f32,
    pub(super) last_activity_time: Instant,
    pub(super) idle_dim_current_alpha: f32,
    pub(super) idle_dim_active: bool,
    /// Key press timestamps for WPM calculation
    pub(super) key_press_times: Vec<Instant>,
    /// Smoothed WPM value for display
    pub(super) displayed_wpm: f32,

    /// Shared monitor info (populated in resumed(), read from FFI thread)
    pub(super) shared_monitors: Option<SharedMonitorInfo>,
    pub(super) monitors_populated: bool,
    pub(super) last_monitor_snapshot: Vec<MonitorInfo>,
    pub(super) debug_first_frame_readback_pending: bool,
    pub(super) debug_surface_readback_frames_remaining: u32,
    pub(super) resumed_seen: bool,
    pub(super) about_to_wait_seen: bool,
}

impl RenderApp {
    pub(super) fn new(
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
            multi_windows: MultiWindowManager::new(),
            adapter: None,
            child_frames: ChildFrameManager::new(),
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
            tab_bar_items: Vec::new(),
            tab_bar_height: 0.0,
            tab_bar_hovered: None,
            tab_bar_pressed: None,
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
            last_activity_time: Instant::now(),
            idle_dim_current_alpha: 0.0,
            idle_dim_active: false,
            shared_monitors: Some(shared_monitors),
            monitors_populated: false,
            last_monitor_snapshot: Vec::new(),
            debug_first_frame_readback_pending: std::env::var_os(
                "NEOMACS_DEBUG_FIRST_FRAME_READBACK",
            )
            .is_some(),
            debug_surface_readback_frames_remaining: std::env::var(
                "NEOMACS_DEBUG_SURFACE_READBACK",
            )
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|count| *count > 0)
            .unwrap_or_else(|| {
                if std::env::var_os("NEOMACS_DEBUG_SURFACE_READBACK").is_some() {
                    32
                } else {
                    0
                }
            }),
            resumed_seen: false,
            about_to_wait_seen: false,
        }
    }
}
