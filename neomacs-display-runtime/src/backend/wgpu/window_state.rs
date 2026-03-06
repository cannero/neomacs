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
mod tests {
    use crate::core::scene::Scene;

    // WindowState::new and WindowState::resize require real wgpu::Device,
    // wgpu::Surface, and winit::Window instances that cannot be constructed
    // without a GPU and display server. These tests verify the Scene-level
    // logic that WindowState delegates to, and the resize guard invariants.

    // ---- Scene creation (mirrors WindowState::new logic) ----

    #[test]
    fn scene_created_with_correct_dimensions() {
        let scene = Scene::new(1920.0, 1080.0);
        assert_eq!(scene.width, 1920.0);
        assert_eq!(scene.height, 1080.0);
    }

    #[test]
    fn scene_created_with_zero_dimensions() {
        let scene = Scene::new(0.0, 0.0);
        assert_eq!(scene.width, 0.0);
        assert_eq!(scene.height, 0.0);
    }

    #[test]
    fn scene_initial_state_is_clean() {
        let scene = Scene::new(800.0, 600.0);
        assert!(scene.dirty.is_none());
        assert!(scene.windows.is_empty());
        assert!(scene.root.is_none());
        assert!(scene.faces.is_empty());
        assert!(scene.borders.is_empty());
    }

    // ---- Resize dimension tracking (mirrors WindowState::resize scene update) ----

    #[test]
    fn scene_dimensions_update_on_resize() {
        let mut scene = Scene::new(800.0, 600.0);
        // Simulate what WindowState::resize does to the scene
        scene.width = 1024.0;
        scene.height = 768.0;
        scene.mark_dirty();

        assert_eq!(scene.width, 1024.0);
        assert_eq!(scene.height, 768.0);
        assert!(scene.dirty.is_some());
    }

    #[test]
    fn scene_mark_dirty_covers_full_area() {
        let mut scene = Scene::new(1920.0, 1080.0);
        scene.mark_dirty();

        let dirty = scene.dirty.unwrap();
        assert_eq!(dirty.x, 0.0);
        assert_eq!(dirty.y, 0.0);
        assert_eq!(dirty.width, 1920.0);
        assert_eq!(dirty.height, 1080.0);
    }

    #[test]
    fn scene_mark_dirty_after_dimension_change() {
        let mut scene = Scene::new(800.0, 600.0);
        // Simulate resize: update dimensions then mark dirty
        scene.width = 1600.0;
        scene.height = 900.0;
        scene.mark_dirty();

        let dirty = scene.dirty.unwrap();
        // Dirty rect should reflect NEW dimensions
        assert_eq!(dirty.width, 1600.0);
        assert_eq!(dirty.height, 900.0);
    }

    // ---- Resize guard logic (width > 0 && height > 0) ----

    #[test]
    fn resize_guard_rejects_zero_width() {
        // WindowState::resize guards with `if width > 0 && height > 0`.
        // With width=0, dimensions should NOT change.
        let mut scene = Scene::new(800.0, 600.0);
        let (new_w, new_h): (u32, u32) = (0, 600);

        // Replicate the guard
        if new_w > 0 && new_h > 0 {
            scene.width = new_w as f32;
            scene.height = new_h as f32;
            scene.mark_dirty();
        }

        // Scene should remain unchanged
        assert_eq!(scene.width, 800.0);
        assert_eq!(scene.height, 600.0);
        assert!(scene.dirty.is_none());
    }

    #[test]
    fn resize_guard_rejects_zero_height() {
        let mut scene = Scene::new(800.0, 600.0);
        let (new_w, new_h): (u32, u32) = (1024, 0);

        if new_w > 0 && new_h > 0 {
            scene.width = new_w as f32;
            scene.height = new_h as f32;
            scene.mark_dirty();
        }

        assert_eq!(scene.width, 800.0);
        assert_eq!(scene.height, 600.0);
        assert!(scene.dirty.is_none());
    }

    #[test]
    fn resize_guard_rejects_both_zero() {
        let mut scene = Scene::new(800.0, 600.0);
        let (new_w, new_h): (u32, u32) = (0, 0);

        if new_w > 0 && new_h > 0 {
            scene.width = new_w as f32;
            scene.height = new_h as f32;
            scene.mark_dirty();
        }

        assert_eq!(scene.width, 800.0);
        assert_eq!(scene.height, 600.0);
        assert!(scene.dirty.is_none());
    }

    #[test]
    fn resize_guard_accepts_valid_dimensions() {
        let mut scene = Scene::new(800.0, 600.0);
        let (new_w, new_h): (u32, u32) = (1920, 1080);

        if new_w > 0 && new_h > 0 {
            scene.width = new_w as f32;
            scene.height = new_h as f32;
            scene.mark_dirty();
        }

        assert_eq!(scene.width, 1920.0);
        assert_eq!(scene.height, 1080.0);
        assert!(scene.dirty.is_some());
    }

    // ---- Multiple resizes ----

    #[test]
    fn multiple_resizes_track_latest_dimensions() {
        let mut scene = Scene::new(800.0, 600.0);

        // First resize
        scene.width = 1024.0;
        scene.height = 768.0;
        scene.mark_dirty();

        // Second resize
        scene.width = 1920.0;
        scene.height = 1080.0;
        scene.mark_dirty();

        assert_eq!(scene.width, 1920.0);
        assert_eq!(scene.height, 1080.0);
        let dirty = scene.dirty.unwrap();
        assert_eq!(dirty.width, 1920.0);
        assert_eq!(dirty.height, 1080.0);
    }

    // ---- Dimension type conversion (u32 -> f32 as done in WindowState) ----

    #[test]
    fn u32_to_f32_dimension_conversion() {
        // WindowState stores width/height as u32, Scene as f32.
        // Verify the conversion preserves values for typical display sizes.
        let pairs: &[(u32, f32)] = &[(1, 1.0), (1920, 1920.0), (3840, 3840.0), (7680, 7680.0)];
        for &(u, f) in pairs {
            assert_eq!(u as f32, f, "u32 {} did not convert to f32 {}", u, f);
        }
    }
}
