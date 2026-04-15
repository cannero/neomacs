//! Window state management for winit windows.

use std::sync::Arc;
use winit::window::Window;

use crate::core::scene::Scene;

/// State for a single winit window.
pub struct WindowState {
    pub window: Arc<Window>,
    pub surface: wgpu::Surface<'static>,
    pub config: wgpu::SurfaceConfiguration,
    pub scene: Scene,
    pub width: u32,
    pub height: u32,
}

impl WindowState {
    pub fn new(
        window: Arc<Window>,
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            window,
            surface,
            config,
            scene: Scene::new(width as f32, height as f32),
            width,
            height,
        }
    }

    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.width = width;
            self.height = height;
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(device, &self.config);
            // Update scene dimensions
            self.scene.width = width as f32;
            self.scene.height = height as f32;
            self.scene.mark_dirty();
        }
    }
}

#[cfg(test)]
#[path = "window_state_test.rs"]
mod tests;
