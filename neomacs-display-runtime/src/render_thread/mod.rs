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
mod state;
mod surface_readback;
#[cfg(test)]
mod tests;
mod thread_handle;
mod transitions;
mod window_events;

pub(crate) use bootstrap::run_render_loop;
#[cfg(feature = "wpe-webkit")]
use state::WebKitImportPolicy;
use state::{FpsCounter, ImeCursorArea, RenderApp, WindowChrome};
pub use state::{MonitorInfo, SharedImageDimensions, SharedMonitorInfo};
pub use thread_handle::RenderThread;

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
