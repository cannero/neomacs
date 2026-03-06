//! Backend trait and module exports.

use crate::core::error::DisplayResult;
use crate::core::scene::Scene;

pub mod tty;

pub mod wgpu;

#[cfg(feature = "wpe-webkit")]
pub mod wpe;

#[cfg(feature = "wpe-webkit")]
pub mod webkit;

/// Display backend trait
pub trait DisplayBackend {
    fn init(&mut self) -> DisplayResult<()>;
    fn shutdown(&mut self);
    fn render(&mut self, scene: &Scene) -> DisplayResult<()>;
    fn present(&mut self) -> DisplayResult<()>;
    fn name(&self) -> &'static str;
    fn is_initialized(&self) -> bool;
    fn resize(&mut self, width: u32, height: u32);
    fn set_vsync(&mut self, enabled: bool);
}

/// Backend type selection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub enum BackendType {
    /// Terminal/TTY backend
    Tty = 0,

    /// Winit/wgpu GPU-accelerated backend
    Wgpu = 1,
}

impl Default for BackendType {
    fn default() -> Self {
        return Self::Wgpu;
    }
}
