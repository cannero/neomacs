//! Thread communication infrastructure for two-thread architecture.
//!
//! Provides lock-free channels and wakeup mechanism between Emacs and render threads.

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, unbounded};
#[cfg(unix)]
use std::os::unix::io::RawFd;
#[cfg(windows)]
use std::os::windows::io::RawHandle;

/// Platform file descriptor type for the wakeup pipe.
#[cfg(unix)]
pub type WakeupFd = RawFd;
#[cfg(windows)]
pub type WakeupFd = RawHandle;

use neomacs_display_protocol::glyph_matrix::FrameDisplayState;
pub use neomacs_display_protocol::{
    EffectsConfig, MenuBarItem, PopupMenuItem, TabBarItem, ToolBarItem, TransitionPolicy,
};
use neovm_core::window::GuiFrameGeometryHints;

/// Monitor information transported from the frontend to the evaluator.
#[derive(Debug, Clone, PartialEq)]
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

/// Input event from render thread to Emacs
#[derive(Debug, Clone)]
pub enum InputEvent {
    Key {
        keysym: u32,
        modifiers: u32,
        pressed: bool,
        /// Emacs frame_id of the window that produced the key event (0 = primary)
        emacs_frame_id: u64,
    },
    MouseButton {
        button: u32,
        x: f32,
        y: f32,
        pressed: bool,
        modifiers: u32,
        /// Target frame for child frame hit testing (0 = parent frame)
        target_frame_id: u64,
        /// WebKit view ID hit by render-thread glyph search (0 = none)
        webkit_id: u32,
        /// Coordinates relative to the WebKit view (valid when webkit_id != 0)
        webkit_rel_x: i32,
        webkit_rel_y: i32,
    },
    MouseMove {
        x: f32,
        y: f32,
        modifiers: u32,
        /// Target frame for child frame hit testing (0 = parent frame)
        target_frame_id: u64,
    },
    MouseScroll {
        delta_x: f32,
        delta_y: f32,
        x: f32,
        y: f32,
        modifiers: u32,
        /// True if deltas are in pixels (touchpad), false if in lines (mouse wheel)
        pixel_precise: bool,
        /// Target frame for child frame hit testing (0 = parent frame)
        target_frame_id: u64,
        /// WebKit view ID hit by render-thread glyph search (0 = none)
        webkit_id: u32,
        /// Coordinates relative to the WebKit view (valid when webkit_id != 0)
        webkit_rel_x: i32,
        webkit_rel_y: i32,
    },
    WindowResize {
        width: u32,
        height: u32,
        /// Emacs frame_id of the window that resized (0 = primary)
        emacs_frame_id: u64,
    },
    WindowClose {
        /// Emacs frame_id of the window being closed (0 = primary)
        emacs_frame_id: u64,
    },
    WindowFocus {
        focused: bool,
        /// Emacs frame_id of the window that gained/lost focus (0 = primary)
        emacs_frame_id: u64,
    },
    /// Monitor configuration changed on the active terminal.
    MonitorsChanged {
        monitors: Vec<MonitorInfo>,
    },
    /// WebKit view title changed
    #[cfg(feature = "wpe-webkit")]
    WebKitTitleChanged {
        id: u32,
        title: String,
    },
    /// WebKit view URL changed
    #[cfg(feature = "wpe-webkit")]
    WebKitUrlChanged {
        id: u32,
        url: String,
    },
    /// WebKit view load progress changed
    #[cfg(feature = "wpe-webkit")]
    WebKitProgressChanged {
        id: u32,
        progress: f64,
    },
    /// WebKit view finished loading
    #[cfg(feature = "wpe-webkit")]
    WebKitLoadFinished {
        id: u32,
    },
    /// Image dimensions ready (sent after async image load)
    ImageDimensionsReady {
        id: u32,
        width: u32,
        height: u32,
    },
    /// Terminal child process exited
    #[cfg(feature = "neo-term")]
    TerminalExited {
        id: u32,
    },
    /// Terminal title changed
    #[cfg(feature = "neo-term")]
    TerminalTitleChanged {
        id: u32,
        title: String,
    },
    /// Popup menu selection made (index into menu items, -1 = cancelled)
    MenuSelection {
        index: i32,
    },
    /// File(s) dropped onto the window
    FileDrop {
        paths: Vec<String>,
        x: f32,
        y: f32,
    },
    /// Toolbar button clicked (index into toolbar items)
    ToolBarClick {
        index: i32,
    },
    TabBarClick {
        index: i32,
    },
    /// Menu bar item clicked (index into menu bar items)
    MenuBarClick {
        index: i32,
    },
}

/// Wrapper for effect update closures that implements Debug.
pub struct EffectUpdater(pub Box<dyn FnOnce(&mut EffectsConfig) + Send>);

impl std::fmt::Debug for EffectUpdater {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EffectUpdater(...)")
    }
}

/// Command from Emacs to render thread
#[derive(Debug)]
pub enum RenderCommand {
    /// Shutdown the render thread
    Shutdown,
    /// Suspend the active TTY frontend.
    SuspendTty,
    /// Resume the active TTY frontend.
    ResumeTty,
    /// Scroll blit pixels within pixel buffer
    ScrollBlit {
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        from_y: i32,
        to_y: i32,
        bg_r: f32,
        bg_g: f32,
        bg_b: f32,
    },
    /// Load image from file (async, ID pre-allocated)
    ImageLoadFile {
        id: u32,
        path: String,
        max_width: u32,
        max_height: u32,
        /// Foreground color as 0xAARRGGBB for monochrome formats (XBM). 0 = default.
        fg_color: u32,
        /// Background color as 0xAARRGGBB for monochrome formats (XBM). 0 = default.
        bg_color: u32,
    },
    /// Load image from encoded data bytes (PNG, JPEG, SVG, etc.)
    ImageLoadData {
        id: u32,
        data: Vec<u8>,
        max_width: u32,
        max_height: u32,
        /// Foreground color as 0xAARRGGBB for monochrome formats (XBM). 0 = default.
        fg_color: u32,
        /// Background color as 0xAARRGGBB for monochrome formats (XBM). 0 = default.
        bg_color: u32,
    },
    /// Load image from raw ARGB32 pixel data
    ImageLoadArgb32 {
        id: u32,
        data: Vec<u8>,
        width: u32,
        height: u32,
        stride: u32,
    },
    /// Load image from raw RGB24 pixel data
    ImageLoadRgb24 {
        id: u32,
        data: Vec<u8>,
        width: u32,
        height: u32,
        stride: u32,
    },
    /// Free an image from cache
    ImageFree {
        id: u32,
    },
    /// Create a WebKit view
    WebKitCreate {
        id: u32,
        width: u32,
        height: u32,
    },
    /// Load URL in WebKit view
    WebKitLoadUri {
        id: u32,
        url: String,
    },
    /// Resize WebKit view
    WebKitResize {
        id: u32,
        width: u32,
        height: u32,
    },
    /// Destroy WebKit view
    WebKitDestroy {
        id: u32,
    },
    /// Click in WebKit view
    WebKitClick {
        id: u32,
        x: i32,
        y: i32,
        button: u32,
    },
    /// Pointer event in WebKit view (raw API)
    WebKitPointerEvent {
        id: u32,
        event_type: u32,
        x: i32,
        y: i32,
        button: u32,
        state: u32,
        modifiers: u32,
    },
    /// Scroll in WebKit view
    WebKitScroll {
        id: u32,
        x: i32,
        y: i32,
        delta_x: i32,
        delta_y: i32,
    },
    /// Keyboard event in WebKit view
    WebKitKeyEvent {
        id: u32,
        keyval: u32,
        keycode: u32,
        pressed: bool,
        modifiers: u32,
    },
    /// Navigate back in WebKit view
    WebKitGoBack {
        id: u32,
    },
    /// Navigate forward in WebKit view
    WebKitGoForward {
        id: u32,
    },
    /// Reload WebKit view
    WebKitReload {
        id: u32,
    },
    /// Execute JavaScript in WebKit view
    WebKitExecuteJavaScript {
        id: u32,
        script: String,
    },
    /// Set floating WebKit overlay position and size
    WebKitSetFloating {
        id: u32,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
    /// Remove floating WebKit overlay
    WebKitRemoveFloating {
        id: u32,
    },
    /// Create video player
    VideoCreate {
        id: u32,
        path: String,
    },
    /// Control video playback
    VideoPlay {
        id: u32,
    },
    VideoPause {
        id: u32,
    },
    VideoDestroy {
        id: u32,
    },
    /// Change the mouse pointer cursor shape (arrow, hand, ibeam, etc.)
    SetMouseCursor {
        cursor_type: i32,
    },
    /// Warp (move) the mouse pointer to given pixel position
    WarpMouse {
        x: i32,
        y: i32,
    },
    /// Set the window title
    SetWindowTitle {
        title: String,
    },
    /// Set the title for a specific GUI frame window. `emacs_frame_id == 0`
    /// targets the adopted primary window.
    SetFrameWindowTitle {
        emacs_frame_id: u64,
        title: String,
    },
    /// Set fullscreen mode (0=none, 1=fullscreen, 4=maximized)
    SetWindowFullscreen {
        mode: u32,
    },
    /// Minimize/iconify the window
    SetWindowMinimized {
        minimized: bool,
    },
    /// Set window position
    SetWindowPosition {
        x: i32,
        y: i32,
    },
    /// Request window inner size change
    SetWindowSize {
        width: u32,
        height: u32,
    },
    /// Request resizing a specific GUI frame window. `emacs_frame_id == 0`
    /// targets the adopted primary window.
    ResizeWindow {
        emacs_frame_id: u64,
        width: u32,
        height: u32,
        geometry_hints: GuiFrameGeometryHints,
    },
    /// Update geometry hints for a specific GUI frame window. `emacs_frame_id == 0`
    /// targets the adopted primary window.
    SetFrameGeometryHints {
        emacs_frame_id: u64,
        geometry_hints: GuiFrameGeometryHints,
    },
    /// Set window decorations (title bar, borders)
    SetWindowDecorated {
        decorated: bool,
    },
    /// Configure cursor blinking
    SetCursorBlink {
        enabled: bool,
        interval_ms: u32,
    },
    /// Configure cursor animation (smooth motion)
    SetCursorAnimation {
        enabled: bool,
        speed: f32,
    },
    /// Configure all animations
    SetAnimationConfig {
        cursor_enabled: bool,
        cursor_speed: f32,
        cursor_style: crate::core::types::CursorAnimStyle,
        cursor_duration_ms: u32,
        transition_policy: TransitionPolicy,
        trail_size: f32,
    },
    /// Create a terminal
    #[cfg(feature = "neo-term")]
    TerminalCreate {
        id: u32,
        cols: u16,
        rows: u16,
        mode: u8, // 0=Window, 1=Inline, 2=Floating
        shell: Option<String>,
    },
    /// Write input to a terminal
    #[cfg(feature = "neo-term")]
    TerminalWrite {
        id: u32,
        data: Vec<u8>,
    },
    /// Resize a terminal
    #[cfg(feature = "neo-term")]
    TerminalResize {
        id: u32,
        cols: u16,
        rows: u16,
    },
    /// Destroy a terminal
    #[cfg(feature = "neo-term")]
    TerminalDestroy {
        id: u32,
    },
    /// Set floating terminal position and opacity
    #[cfg(feature = "neo-term")]
    TerminalSetFloat {
        id: u32,
        x: f32,
        y: f32,
        opacity: f32,
    },
    /// Show a popup menu at position (x, y)
    ShowPopupMenu {
        x: f32,
        y: f32,
        items: Vec<PopupMenuItem>,
        title: Option<String>,
        /// Menu face colors (sRGB 0.0-1.0). None = use defaults.
        fg: Option<(f32, f32, f32)>,
        bg: Option<(f32, f32, f32)>,
    },
    /// Hide the active popup menu
    HidePopupMenu,
    /// Show a tooltip at position (x, y)
    ShowTooltip {
        x: f32,
        y: f32,
        text: String,
        fg_r: f32,
        fg_g: f32,
        fg_b: f32,
        bg_r: f32,
        bg_g: f32,
        bg_b: f32,
    },
    /// Hide the active tooltip
    HideTooltip,
    /// Trigger visual bell flash
    VisualBell,
    /// Request window attention (urgency hint / taskbar flash)
    RequestAttention {
        urgent: bool,
    },
    /// Update visual effect configuration.
    /// The closure modifies the shared EffectsConfig in-place.
    UpdateEffect(EffectUpdater),
    /// Toggle scroll indicators and focus ring
    SetScrollIndicators {
        enabled: bool,
    },
    /// Set custom title bar height (0 = hidden, >0 = show with given height)
    SetTitlebarHeight {
        height: f32,
    },
    /// Toggle FPS counter overlay
    SetShowFps {
        enabled: bool,
    },
    /// Set window corner radius for borderless mode (0 = no rounding)
    SetCornerRadius {
        radius: f32,
    },
    /// Set extra spacing (line spacing in pixels, letter spacing in pixels)
    SetExtraSpacing {
        line_spacing: f32,
        letter_spacing: f32,
    },
    /// Configure rainbow indent guide colors (up to 6 cycling colors by depth)
    SetIndentGuideRainbow {
        enabled: bool,
        /// Colors as sRGB 0.0-1.0 tuples with opacity
        colors: Vec<(f32, f32, f32, f32)>,
    },
    /// Configure smooth cursor size transition on text-scale-adjust
    SetCursorSizeTransition {
        enabled: bool,
        /// Transition duration in milliseconds
        duration_ms: u32,
    },
    /// Enable or disable font ligatures
    SetLigaturesEnabled {
        enabled: bool,
    },
    /// Remove a child frame (sent when frame is deleted or unparented)
    RemoveChildFrame {
        frame_id: u64,
    },
    /// Create a new OS window for a top-level Emacs frame
    CreateWindow {
        emacs_frame_id: u64,
        width: u32,
        height: u32,
        title: String,
        geometry_hints: GuiFrameGeometryHints,
    },
    /// Destroy an OS window for a top-level Emacs frame
    DestroyWindow {
        emacs_frame_id: u64,
    },
    /// Configure child frame visual style (drop shadow, rounded corners)
    SetChildFrameStyle {
        corner_radius: f32,
        shadow_enabled: bool,
        shadow_layers: u32,
        shadow_offset: f32,
        shadow_opacity: f32,
    },
    /// Set toolbar items (sent each frame when items change)
    SetToolBar {
        items: Vec<ToolBarItem>,
        height: f32,
        fg_r: f32,
        fg_g: f32,
        fg_b: f32,
        bg_r: f32,
        bg_g: f32,
        bg_b: f32,
    },
    /// Configure toolbar appearance
    SetToolBarConfig {
        icon_size: u32,
        padding: u32,
    },
    /// Set menu bar items (sent each frame when items change)
    SetMenuBar {
        items: Vec<MenuBarItem>,
        height: f32,
        fg_r: f32,
        fg_g: f32,
        fg_b: f32,
        bg_r: f32,
        bg_g: f32,
        bg_b: f32,
    },
}

/// Wakeup pipe for signaling Emacs from render thread
#[cfg(unix)]
pub struct WakeupPipe {
    read_fd: RawFd,
    write_fd: RawFd,
}

#[cfg(unix)]
impl WakeupPipe {
    /// Create a new wakeup pipe
    pub fn new() -> std::io::Result<Self> {
        let (read, write) = os_pipe::pipe()?;
        use std::os::unix::io::IntoRawFd;
        Ok(Self {
            read_fd: read.into_raw_fd(),
            write_fd: write.into_raw_fd(),
        })
    }

    /// Get the read fd for Emacs to select() on
    pub fn read_fd(&self) -> WakeupFd {
        self.read_fd
    }

    /// Signal Emacs to wake up (called from render thread)
    pub fn wake(&self) {
        unsafe {
            libc::write(self.write_fd, [1u8].as_ptr() as *const _, 1);
        }
    }

    /// Clear the wakeup signal (called from Emacs thread)
    pub fn clear(&self) {
        let mut buf = [0u8; 64];
        unsafe {
            // Non-blocking read to drain the pipe
            let flags = libc::fcntl(self.read_fd, libc::F_GETFL);
            libc::fcntl(self.read_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            while libc::read(self.read_fd, buf.as_mut_ptr() as *mut _, buf.len()) > 0 {}
            libc::fcntl(self.read_fd, libc::F_SETFL, flags);
        }
    }
}

#[cfg(unix)]
impl Drop for WakeupPipe {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.read_fd);
            libc::close(self.write_fd);
        }
    }
}

/// Wakeup pipe for signaling Emacs from render thread (Windows)
#[cfg(windows)]
pub struct WakeupPipe {
    read_handle: RawHandle,
    write_handle: RawHandle,
}

#[cfg(windows)]
impl WakeupPipe {
    /// Create a new wakeup pipe
    pub fn new() -> std::io::Result<Self> {
        let (read, write) = os_pipe::pipe()?;
        use std::os::windows::io::IntoRawHandle;
        Ok(Self {
            read_handle: read.into_raw_handle(),
            write_handle: write.into_raw_handle(),
        })
    }

    /// Get the read handle for wakeup signaling
    pub fn read_fd(&self) -> WakeupFd {
        self.read_handle
    }

    /// Signal Emacs to wake up (called from render thread)
    pub fn wake(&self) {
        use windows_sys::Win32::Storage::FileSystem::WriteFile;
        unsafe {
            WriteFile(
                self.write_handle as _,
                [1u8].as_ptr() as _,
                1,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
        }
    }

    /// Clear the wakeup signal (called from Emacs thread)
    pub fn clear(&self) {
        use windows_sys::Win32::Storage::FileSystem::ReadFile;
        use windows_sys::Win32::System::Pipes::PeekNamedPipe;
        let mut buf = [0u8; 64];
        loop {
            let mut avail: u32 = 0;
            unsafe {
                PeekNamedPipe(
                    self.read_handle as _,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut avail,
                    std::ptr::null_mut(),
                );
            }
            if avail == 0 {
                break;
            }
            let mut read_bytes: u32 = 0;
            unsafe {
                ReadFile(
                    self.read_handle as _,
                    buf.as_mut_ptr() as _,
                    buf.len() as u32,
                    &mut read_bytes,
                    std::ptr::null_mut(),
                );
            }
            if read_bytes == 0 {
                break;
            }
        }
    }
}

#[cfg(windows)]
impl Drop for WakeupPipe {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        unsafe {
            CloseHandle(self.read_handle as _);
            CloseHandle(self.write_handle as _);
        }
    }
}

// SAFETY: WakeupPipe handles are OS pipe endpoints; each end is used by
// exactly one thread (write on render, read on emacs). The raw handles
// are safe to transfer across threads.
#[cfg(windows)]
unsafe impl Send for WakeupPipe {}
#[cfg(windows)]
unsafe impl Sync for WakeupPipe {}

/// Channel capacities
// Frame channel: unbounded so try_send never drops frames.
// The render thread drains all queued frames and keeps only the latest
// (see poll_frame()), so memory stays bounded in practice.
const INPUT_CHANNEL_CAPACITY: usize = 256;
const COMMAND_CHANNEL_CAPACITY: usize = 64;

/// Communication channels between threads
pub struct ThreadComms {
    /// Frame display state: Emacs → Render
    pub frame_tx: Sender<FrameDisplayState>,
    pub frame_rx: Receiver<FrameDisplayState>,

    /// Commands: Emacs → Render
    pub cmd_tx: Sender<RenderCommand>,
    pub cmd_rx: Receiver<RenderCommand>,

    /// Input events: Render → Emacs
    pub input_tx: Sender<InputEvent>,
    pub input_rx: Receiver<InputEvent>,

    /// Wakeup pipe: Render → Emacs
    pub wakeup: WakeupPipe,
}

impl ThreadComms {
    /// Create new thread communication channels
    pub fn new() -> std::io::Result<Self> {
        let (frame_tx, frame_rx) = unbounded();
        let (cmd_tx, cmd_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
        let (input_tx, input_rx) = bounded(INPUT_CHANNEL_CAPACITY);
        let wakeup = WakeupPipe::new()?;

        Ok(Self {
            frame_tx,
            frame_rx,
            cmd_tx,
            cmd_rx,
            input_tx,
            input_rx,
            wakeup,
        })
    }

    /// Split into Emacs-side and Render-side handles
    pub fn split(self) -> (EmacsComms, RenderComms) {
        let emacs = EmacsComms {
            frame_tx: self.frame_tx,
            cmd_tx: self.cmd_tx,
            input_rx: self.input_rx,
            wakeup_read_fd: self.wakeup.read_fd(),
            wakeup_clear: WakeupClear {
                fd: self.wakeup.read_fd(),
            },
        };

        let render = RenderComms {
            frame_rx: self.frame_rx,
            cmd_rx: self.cmd_rx,
            input_tx: self.input_tx,
            wakeup: self.wakeup,
        };

        (emacs, render)
    }
}

/// Emacs thread communication handle
pub struct EmacsComms {
    pub frame_tx: Sender<FrameDisplayState>,
    pub cmd_tx: Sender<RenderCommand>,
    pub input_rx: Receiver<InputEvent>,
    pub wakeup_read_fd: WakeupFd,
    pub wakeup_clear: WakeupClear,
}

// SAFETY: EmacsComms is used exclusively on the Emacs thread.
// The WakeupFd (RawHandle) it holds is a valid OS handle.
#[cfg(windows)]
unsafe impl Send for EmacsComms {}
#[cfg(windows)]
unsafe impl Sync for EmacsComms {}

/// Handle for clearing wakeup pipe (Unix)
#[cfg(unix)]
pub struct WakeupClear {
    fd: WakeupFd,
}

#[cfg(unix)]
impl WakeupClear {
    pub fn clear(&self) {
        let mut buf = [0u8; 64];
        unsafe {
            let flags = libc::fcntl(self.fd, libc::F_GETFL);
            libc::fcntl(self.fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            while libc::read(self.fd, buf.as_mut_ptr() as *mut _, buf.len()) > 0 {}
            libc::fcntl(self.fd, libc::F_SETFL, flags);
        }
    }
}

/// Handle for clearing wakeup pipe (Windows)
#[cfg(windows)]
pub struct WakeupClear {
    fd: WakeupFd,
}

#[cfg(windows)]
impl WakeupClear {
    pub fn clear(&self) {
        use windows_sys::Win32::Storage::FileSystem::ReadFile;
        use windows_sys::Win32::System::Pipes::PeekNamedPipe;
        let mut buf = [0u8; 64];
        loop {
            let mut avail: u32 = 0;
            unsafe {
                PeekNamedPipe(
                    self.fd as _,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut avail,
                    std::ptr::null_mut(),
                );
            }
            if avail == 0 {
                break;
            }
            let mut read_bytes: u32 = 0;
            unsafe {
                ReadFile(
                    self.fd as _,
                    buf.as_mut_ptr() as _,
                    buf.len() as u32,
                    &mut read_bytes,
                    std::ptr::null_mut(),
                );
            }
            if read_bytes == 0 {
                break;
            }
        }
    }
}

// SAFETY: WakeupClear holds a read-end handle used only on the Emacs thread.
#[cfg(windows)]
unsafe impl Send for WakeupClear {}
#[cfg(windows)]
unsafe impl Sync for WakeupClear {}

/// Render thread communication handle
pub struct RenderComms {
    pub frame_rx: Receiver<FrameDisplayState>,
    pub cmd_rx: Receiver<RenderCommand>,
    pub input_tx: Sender<InputEvent>,
    pub wakeup: WakeupPipe,
}

impl RenderComms {
    fn should_log_delivery(event: &InputEvent) -> bool {
        matches!(
            event,
            InputEvent::WindowResize { .. }
                | InputEvent::WindowClose { .. }
                | InputEvent::WindowFocus { .. }
                | InputEvent::MonitorsChanged { .. }
        )
    }

    fn event_name(event: &InputEvent) -> &'static str {
        match event {
            InputEvent::Key { .. } => "key",
            InputEvent::MouseButton { .. } => "mouse-button",
            InputEvent::MouseMove { .. } => "mouse-move",
            InputEvent::MouseScroll { .. } => "mouse-scroll",
            InputEvent::WindowResize { .. } => "window-resize",
            InputEvent::WindowClose { .. } => "window-close",
            InputEvent::WindowFocus { .. } => "window-focus",
            InputEvent::MonitorsChanged { .. } => "monitors-changed",
            #[cfg(feature = "wpe-webkit")]
            InputEvent::WebKitTitleChanged { .. } => "webkit-title-changed",
            #[cfg(feature = "wpe-webkit")]
            InputEvent::WebKitUrlChanged { .. } => "webkit-url-changed",
            #[cfg(feature = "wpe-webkit")]
            InputEvent::WebKitProgressChanged { .. } => "webkit-progress-changed",
            #[cfg(feature = "wpe-webkit")]
            InputEvent::WebKitLoadFinished { .. } => "webkit-load-finished",
            InputEvent::ImageDimensionsReady { .. } => "image-dimensions-ready",
            InputEvent::MenuSelection { .. } => "menu-selection",
            InputEvent::FileDrop { .. } => "file-drop",
            InputEvent::ToolBarClick { .. } => "toolbar-click",
            InputEvent::TabBarClick { .. } => "tabbar-click",
            InputEvent::MenuBarClick { .. } => "menubar-click",
            #[cfg(feature = "neo-term")]
            InputEvent::TerminalExited { .. } => "terminal-exited",
            #[cfg(feature = "neo-term")]
            InputEvent::TerminalTitleChanged { .. } => "terminal-title-changed",
        }
    }

    /// Send input event to Emacs and wake it up
    pub fn send_input(&self, event: InputEvent) {
        let log_delivery = Self::should_log_delivery(&event);
        let event_name = Self::event_name(&event);
        match self.input_tx.try_send(event) {
            Ok(()) => {
                if log_delivery {
                    tracing::debug!("send_input: queued {}", event_name);
                }
                self.wakeup.wake();
            }
            Err(TrySendError::Full(event)) => {
                tracing::warn!(
                    "send_input: dropped {} because the input queue is full",
                    Self::event_name(&event)
                );
            }
            Err(TrySendError::Disconnected(event)) => {
                tracing::warn!(
                    "send_input: dropped {} because the input queue is disconnected",
                    Self::event_name(&event)
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "thread_comm_test.rs"]
mod tests;
