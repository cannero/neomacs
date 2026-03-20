//! Multi-window management for the render thread.
//!
//! Supports multiple OS windows, each with its own wgpu surface.
//! Shared GPU device/queue/glyph atlas across all windows.
//! Each window holds its own frame data and child frames.

use std::collections::HashMap;
use std::sync::Arc;

use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId};

use super::child_frames::ChildFrameManager;
use super::state::{effective_window_scale_factor, window_size_from_emacs_pixels};
use crate::core::frame_glyphs::FrameGlyphBuffer;

/// Per-window state. Each Emacs top-level frame gets its own OS window
/// with a separate wgpu surface.
pub(crate) struct WindowState {
    /// The winit window.
    pub window: Arc<Window>,
    /// wgpu surface for this window.
    pub surface: wgpu::Surface<'static>,
    /// Surface configuration.
    pub surface_config: wgpu::SurfaceConfiguration,
    /// Physical width in pixels.
    pub width: u32,
    /// Physical height in pixels.
    pub height: u32,
    /// Display scale factor for this window's monitor.
    pub scale_factor: f64,
    /// The Emacs frame_id that owns this window (used for routing).
    pub emacs_frame_id: u64,
    /// Current root frame glyph buffer for this window.
    pub current_frame: Option<FrameGlyphBuffer>,
    /// Child frames rendered as overlays in this window.
    pub child_frames: ChildFrameManager,
    /// Whether this window needs a redraw.
    pub frame_dirty: bool,
    /// Window title.
    pub title: String,
}

impl WindowState {
    /// Resize this window's surface.
    pub fn handle_resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.width = width;
        self.height = height;
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(device, &self.surface_config);
        self.frame_dirty = true;
    }
}

/// Manages all windows in the render thread.
///
/// Maps between Emacs frame IDs (u64) and winit WindowIds.
/// One shared GPU device services all windows.
pub(crate) struct MultiWindowManager {
    /// Emacs frame_id → WindowState
    pub windows: HashMap<u64, WindowState>,
    /// Winit WindowId → Emacs frame_id (reverse mapping for event dispatch)
    pub winit_to_emacs: HashMap<WindowId, u64>,
    /// Pending window creation requests (processed in resumed/about_to_wait)
    pub pending_creates: Vec<PendingWindow>,
    /// Pending window destruction requests
    pub pending_destroys: Vec<u64>,
}

/// A request to create a new OS window.
pub(crate) struct PendingWindow {
    pub emacs_frame_id: u64,
    pub width: u32,
    pub height: u32,
    pub title: String,
}

impl MultiWindowManager {
    pub fn new() -> Self {
        Self {
            windows: HashMap::new(),
            winit_to_emacs: HashMap::new(),
            pending_creates: Vec::new(),
            pending_destroys: Vec::new(),
        }
    }

    /// Schedule a new window to be created on the next event loop iteration.
    pub fn request_create(&mut self, emacs_frame_id: u64, width: u32, height: u32, title: String) {
        self.pending_creates.push(PendingWindow {
            emacs_frame_id,
            width,
            height,
            title,
        });
    }

    /// Schedule a window for destruction.
    pub fn request_destroy(&mut self, emacs_frame_id: u64) {
        self.pending_destroys.push(emacs_frame_id);
    }

    /// Process pending window creations. Must be called from the event loop
    /// (requires ActiveEventLoop for window creation).
    pub fn process_creates(
        &mut self,
        event_loop: &ActiveEventLoop,
        device: &wgpu::Device,
        adapter: &wgpu::Adapter,
    ) {
        let pending = std::mem::take(&mut self.pending_creates);
        for req in pending {
            if self.windows.contains_key(&req.emacs_frame_id) {
                tracing::warn!("Window for frame {} already exists", req.emacs_frame_id);
                continue;
            }

            let attrs = Window::default_attributes()
                .with_title(&req.title)
                .with_inner_size(window_size_from_emacs_pixels(req.width, req.height))
                .with_transparent(true);

            match event_loop.create_window(attrs) {
                Ok(window) => {
                    let window = Arc::new(window);
                    crate::window_icon::apply_window_icon(&window);
                    let raw_scale_factor = window.scale_factor();
                    let scale_factor = effective_window_scale_factor(raw_scale_factor);
                    let phys = window.inner_size();

                    // Create surface for this window
                    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                        backends: wgpu::Backends::all(),
                        ..Default::default()
                    });
                    let surface = match instance.create_surface(window.clone()) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::error!(
                                "Failed to create surface for frame {}: {:?}",
                                req.emacs_frame_id,
                                e
                            );
                            continue;
                        }
                    };

                    // Configure surface
                    let caps = surface.get_capabilities(adapter);
                    let format = caps
                        .formats
                        .iter()
                        .copied()
                        .find(|f| f.is_srgb())
                        .unwrap_or(caps.formats[0]);
                    let alpha_mode = if caps
                        .alpha_modes
                        .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
                    {
                        wgpu::CompositeAlphaMode::PreMultiplied
                    } else {
                        caps.alpha_modes[0]
                    };
                    let config = wgpu::SurfaceConfiguration {
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                        format,
                        width: phys.width,
                        height: phys.height,
                        present_mode: wgpu::PresentMode::Fifo,
                        alpha_mode,
                        view_formats: vec![],
                        desired_maximum_frame_latency: 2,
                    };
                    surface.configure(device, &config);

                    // Enable IME
                    window.set_ime_allowed(true);

                    let winit_id = window.id();
                    tracing::info!(
                        "Created window for frame {} (winit {:?}, {}x{}, raw_scale={}, effective_scale={})",
                        req.emacs_frame_id,
                        winit_id,
                        phys.width,
                        phys.height,
                        raw_scale_factor,
                        scale_factor
                    );

                    self.winit_to_emacs.insert(winit_id, req.emacs_frame_id);
                    self.windows.insert(
                        req.emacs_frame_id,
                        WindowState {
                            window,
                            surface,
                            surface_config: config,
                            width: phys.width,
                            height: phys.height,
                            scale_factor,
                            emacs_frame_id: req.emacs_frame_id,
                            current_frame: None,
                            child_frames: ChildFrameManager::new(),
                            frame_dirty: false,
                            title: req.title,
                        },
                    );
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to create window for frame {}: {:?}",
                        req.emacs_frame_id,
                        e
                    );
                }
            }
        }
    }

    /// Process pending window destructions.
    pub fn process_destroys(&mut self) {
        let pending = std::mem::take(&mut self.pending_destroys);
        for frame_id in pending {
            if let Some(state) = self.windows.remove(&frame_id) {
                self.winit_to_emacs.remove(&state.window.id());
                tracing::info!("Destroyed window for frame {}", frame_id);
                // Window and surface are dropped here
            }
        }
    }

    /// Drop all windows and their wgpu surfaces (for clean shutdown).
    pub fn destroy_all(&mut self) {
        self.pending_creates.clear();
        self.pending_destroys.clear();
        self.winit_to_emacs.clear();
        // Drop all window states (surfaces, etc.)
        self.windows.clear();
    }

    /// Look up the Emacs frame_id for a winit WindowId.
    pub fn emacs_frame_for_winit(&self, winit_id: WindowId) -> Option<u64> {
        self.winit_to_emacs.get(&winit_id).copied()
    }

    /// Get a window state by Emacs frame_id.
    pub fn get(&self, emacs_frame_id: u64) -> Option<&WindowState> {
        self.windows.get(&emacs_frame_id)
    }

    /// Get a mutable window state by Emacs frame_id.
    pub fn get_mut(&mut self, emacs_frame_id: u64) -> Option<&mut WindowState> {
        self.windows.get_mut(&emacs_frame_id)
    }

    /// Get a window state by winit WindowId.
    pub fn get_by_winit(&self, winit_id: WindowId) -> Option<&WindowState> {
        self.winit_to_emacs
            .get(&winit_id)
            .and_then(|id| self.windows.get(id))
    }

    /// Get a mutable window state by winit WindowId.
    pub fn get_by_winit_mut(&mut self, winit_id: WindowId) -> Option<&mut WindowState> {
        self.winit_to_emacs
            .get(&winit_id)
            .copied()
            .and_then(move |id| self.windows.get_mut(&id))
    }

    /// Route a FrameGlyphBuffer to the appropriate window.
    /// Returns true if the frame was routed to a secondary window.
    pub fn route_frame(&mut self, frame: FrameGlyphBuffer) -> bool {
        let frame_id = frame.frame_id;
        if frame_id != 0 {
            if frame.parent_id != 0 {
                // Child frame: route to the window that owns the parent
                // Find which window has the parent as its root frame
                for (_, ws) in self.windows.iter_mut() {
                    if ws.emacs_frame_id == frame.parent_id {
                        ws.child_frames.update_frame(frame);
                        ws.frame_dirty = true;
                        return true;
                    }
                }
            } else if let Some(ws) = self.windows.get_mut(&frame_id) {
                // Root frame for a secondary window
                ws.current_frame = Some(frame);
                ws.frame_dirty = true;
                return true;
            }
        }
        false // Not handled — belongs to primary window
    }

    /// Check if any secondary window needs redrawing.
    pub fn any_dirty(&self) -> bool {
        self.windows.values().any(|ws| ws.frame_dirty)
    }

    /// Return number of secondary windows.
    pub fn count(&self) -> usize {
        self.windows.len()
    }

    /// Iterate over all windows that need rendering.
    pub fn dirty_windows(&mut self) -> Vec<u64> {
        self.windows
            .iter()
            .filter(|(_, ws)| ws.frame_dirty)
            .map(|(&id, _)| id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::frame_glyphs::FrameGlyphBuffer;

    // =======================================================================
    // Helper: create a FrameGlyphBuffer with specified identity fields
    // =======================================================================

    fn make_frame(frame_id: u64, parent_id: u64) -> FrameGlyphBuffer {
        let mut buf = FrameGlyphBuffer::with_size(800.0, 600.0);
        buf.frame_id = frame_id;
        buf.parent_id = parent_id;
        buf
    }

    // =======================================================================
    // MultiWindowManager::new() — initial state
    // =======================================================================

    #[test]
    fn new_manager_is_empty() {
        let mgr = MultiWindowManager::new();
        assert!(mgr.windows.is_empty());
        assert!(mgr.winit_to_emacs.is_empty());
        assert!(mgr.pending_creates.is_empty());
        assert!(mgr.pending_destroys.is_empty());
    }

    #[test]
    fn new_manager_count_is_zero() {
        let mgr = MultiWindowManager::new();
        assert_eq!(mgr.count(), 0);
    }

    #[test]
    fn new_manager_any_dirty_is_false() {
        let mgr = MultiWindowManager::new();
        assert!(!mgr.any_dirty());
    }

    #[test]
    fn new_manager_dirty_windows_is_empty() {
        let mut mgr = MultiWindowManager::new();
        assert!(mgr.dirty_windows().is_empty());
    }

    // =======================================================================
    // request_create() — pending create queue
    // =======================================================================

    #[test]
    fn request_create_adds_to_pending() {
        let mut mgr = MultiWindowManager::new();
        mgr.request_create(1, 800, 600, "Test Window".to_string());

        assert_eq!(mgr.pending_creates.len(), 1);
        assert_eq!(mgr.pending_creates[0].emacs_frame_id, 1);
        assert_eq!(mgr.pending_creates[0].width, 800);
        assert_eq!(mgr.pending_creates[0].height, 600);
        assert_eq!(mgr.pending_creates[0].title, "Test Window");
    }

    #[test]
    fn request_create_multiple_preserves_order() {
        let mut mgr = MultiWindowManager::new();
        mgr.request_create(1, 800, 600, "Window 1".to_string());
        mgr.request_create(2, 1024, 768, "Window 2".to_string());
        mgr.request_create(3, 1920, 1080, "Window 3".to_string());

        assert_eq!(mgr.pending_creates.len(), 3);
        assert_eq!(mgr.pending_creates[0].emacs_frame_id, 1);
        assert_eq!(mgr.pending_creates[1].emacs_frame_id, 2);
        assert_eq!(mgr.pending_creates[2].emacs_frame_id, 3);
    }

    #[test]
    fn request_create_does_not_modify_windows_map() {
        let mut mgr = MultiWindowManager::new();
        mgr.request_create(1, 800, 600, "Test".to_string());

        // The window should NOT be in the windows map yet —
        // only in the pending queue until process_creates runs
        assert!(mgr.windows.is_empty());
        assert_eq!(mgr.count(), 0);
    }

    #[test]
    fn request_create_allows_duplicate_frame_ids() {
        let mut mgr = MultiWindowManager::new();
        mgr.request_create(1, 800, 600, "First".to_string());
        mgr.request_create(1, 1024, 768, "Duplicate".to_string());

        // Both are queued (process_creates will skip duplicates)
        assert_eq!(mgr.pending_creates.len(), 2);
    }

    #[test]
    fn request_create_zero_dimensions() {
        let mut mgr = MultiWindowManager::new();
        mgr.request_create(1, 0, 0, "Zero".to_string());

        assert_eq!(mgr.pending_creates.len(), 1);
        assert_eq!(mgr.pending_creates[0].width, 0);
        assert_eq!(mgr.pending_creates[0].height, 0);
    }

    #[test]
    fn request_create_empty_title() {
        let mut mgr = MultiWindowManager::new();
        mgr.request_create(1, 800, 600, String::new());

        assert_eq!(mgr.pending_creates[0].title, "");
    }

    #[test]
    fn request_create_large_frame_id() {
        let mut mgr = MultiWindowManager::new();
        let large_id = u64::MAX;
        mgr.request_create(large_id, 800, 600, "Max ID".to_string());

        assert_eq!(mgr.pending_creates[0].emacs_frame_id, large_id);
    }

    // =======================================================================
    // request_destroy() — pending destroy queue
    // =======================================================================

    #[test]
    fn request_destroy_adds_to_pending() {
        let mut mgr = MultiWindowManager::new();
        mgr.request_destroy(42);

        assert_eq!(mgr.pending_destroys.len(), 1);
        assert_eq!(mgr.pending_destroys[0], 42);
    }

    #[test]
    fn request_destroy_multiple_preserves_order() {
        let mut mgr = MultiWindowManager::new();
        mgr.request_destroy(1);
        mgr.request_destroy(2);
        mgr.request_destroy(3);

        assert_eq!(mgr.pending_destroys.len(), 3);
        assert_eq!(mgr.pending_destroys, vec![1, 2, 3]);
    }

    #[test]
    fn request_destroy_does_not_modify_windows_map() {
        let mut mgr = MultiWindowManager::new();
        mgr.request_destroy(99);

        // Nothing should change in the actual windows map
        assert!(mgr.windows.is_empty());
        assert_eq!(mgr.count(), 0);
    }

    #[test]
    fn request_destroy_nonexistent_frame_id_is_accepted() {
        let mut mgr = MultiWindowManager::new();
        // No windows exist, but we can still queue a destroy
        mgr.request_destroy(999);
        assert_eq!(mgr.pending_destroys.len(), 1);
    }

    #[test]
    fn request_destroy_duplicate_frame_ids_are_queued() {
        let mut mgr = MultiWindowManager::new();
        mgr.request_destroy(1);
        mgr.request_destroy(1);

        assert_eq!(mgr.pending_destroys.len(), 2);
        assert_eq!(mgr.pending_destroys[0], 1);
        assert_eq!(mgr.pending_destroys[1], 1);
    }

    // =======================================================================
    // process_destroys() — drain pending destroy queue
    // =======================================================================

    #[test]
    fn process_destroys_drains_pending_queue() {
        let mut mgr = MultiWindowManager::new();
        mgr.request_destroy(1);
        mgr.request_destroy(2);

        mgr.process_destroys();

        assert!(mgr.pending_destroys.is_empty());
    }

    #[test]
    fn process_destroys_on_empty_queue_is_noop() {
        let mut mgr = MultiWindowManager::new();
        mgr.process_destroys();
        assert!(mgr.pending_destroys.is_empty());
        assert!(mgr.windows.is_empty());
    }

    #[test]
    fn process_destroys_nonexistent_frame_ids_does_not_panic() {
        let mut mgr = MultiWindowManager::new();
        mgr.request_destroy(999);
        mgr.request_destroy(1000);

        // Should not panic even though these frame IDs don't exist in windows
        mgr.process_destroys();

        assert!(mgr.pending_destroys.is_empty());
    }

    // =======================================================================
    // get() / get_mut() — lookup by emacs frame_id (empty manager)
    // =======================================================================

    #[test]
    fn get_returns_none_for_empty_manager() {
        let mgr = MultiWindowManager::new();
        assert!(mgr.get(1).is_none());
        assert!(mgr.get(0).is_none());
        assert!(mgr.get(u64::MAX).is_none());
    }

    #[test]
    fn get_mut_returns_none_for_empty_manager() {
        let mut mgr = MultiWindowManager::new();
        assert!(mgr.get_mut(1).is_none());
    }

    // =======================================================================
    // emacs_frame_for_winit() — reverse lookup (empty manager)
    // =======================================================================

    // Note: We cannot construct winit::window::WindowId in tests
    // (it's opaque), so we can only test the empty-map case indirectly
    // by verifying the map is empty.

    #[test]
    fn winit_to_emacs_map_is_empty_initially() {
        let mgr = MultiWindowManager::new();
        assert!(mgr.winit_to_emacs.is_empty());
    }

    // =======================================================================
    // route_frame() — frame routing logic
    // =======================================================================

    #[test]
    fn route_frame_with_frame_id_zero_returns_false() {
        let mut mgr = MultiWindowManager::new();
        let frame = make_frame(0, 0);

        // frame_id == 0 means primary window, not handled by multi_window
        assert!(!mgr.route_frame(frame));
    }

    #[test]
    fn route_frame_with_nonexistent_root_frame_returns_false() {
        let mut mgr = MultiWindowManager::new();
        let frame = make_frame(42, 0);

        // frame_id=42, parent_id=0 → root frame for secondary window
        // But no window with emacs_frame_id=42 exists
        assert!(!mgr.route_frame(frame));
    }

    #[test]
    fn route_frame_child_with_no_parent_window_returns_false() {
        let mut mgr = MultiWindowManager::new();
        let frame = make_frame(100, 42);

        // frame_id=100, parent_id=42 → child frame for parent 42
        // But no window with emacs_frame_id=42 exists
        assert!(!mgr.route_frame(frame));
    }

    #[test]
    fn route_frame_does_not_create_windows() {
        let mut mgr = MultiWindowManager::new();
        let frame = make_frame(42, 0);

        mgr.route_frame(frame);

        // route_frame should not add entries to the windows map
        assert!(mgr.windows.is_empty());
        assert_eq!(mgr.count(), 0);
    }

    // =======================================================================
    // PendingWindow struct
    // =======================================================================

    #[test]
    fn pending_window_stores_all_fields() {
        let pw = PendingWindow {
            emacs_frame_id: 123,
            width: 1920,
            height: 1080,
            title: "My Emacs Frame".to_string(),
        };

        assert_eq!(pw.emacs_frame_id, 123);
        assert_eq!(pw.width, 1920);
        assert_eq!(pw.height, 1080);
        assert_eq!(pw.title, "My Emacs Frame");
    }

    #[test]
    fn pending_window_unicode_title() {
        let pw = PendingWindow {
            emacs_frame_id: 1,
            width: 800,
            height: 600,
            title: "Emacs \u{2014} \u{1F680} Neomacs".to_string(),
        };

        assert!(pw.title.contains('\u{2014}')); // em dash
        assert!(pw.title.contains('\u{1F680}')); // rocket emoji
    }

    // =======================================================================
    // Mixed create/destroy queue operations
    // =======================================================================

    #[test]
    fn create_and_destroy_queues_are_independent() {
        let mut mgr = MultiWindowManager::new();

        mgr.request_create(1, 800, 600, "Win1".to_string());
        mgr.request_create(2, 1024, 768, "Win2".to_string());
        mgr.request_destroy(3);
        mgr.request_destroy(4);

        assert_eq!(mgr.pending_creates.len(), 2);
        assert_eq!(mgr.pending_destroys.len(), 2);

        // Processing destroys should not affect creates
        mgr.process_destroys();
        assert!(mgr.pending_destroys.is_empty());
        assert_eq!(mgr.pending_creates.len(), 2);
    }

    #[test]
    fn process_destroys_called_twice_is_safe() {
        let mut mgr = MultiWindowManager::new();
        mgr.request_destroy(1);

        mgr.process_destroys();
        assert!(mgr.pending_destroys.is_empty());

        // Second call should be a no-op
        mgr.process_destroys();
        assert!(mgr.pending_destroys.is_empty());
    }

    // =======================================================================
    // route_frame() edge cases
    // =======================================================================

    #[test]
    fn route_frame_primary_window_frame_id_zero_parent_zero() {
        let mut mgr = MultiWindowManager::new();
        let frame = make_frame(0, 0);
        assert!(!mgr.route_frame(frame));
    }

    #[test]
    fn route_frame_frame_id_zero_parent_nonzero_returns_false() {
        let mut mgr = MultiWindowManager::new();
        // frame_id=0 short-circuits before checking parent_id
        let mut frame = make_frame(0, 42);
        frame.frame_id = 0;
        frame.parent_id = 42;
        assert!(!mgr.route_frame(frame));
    }

    #[test]
    fn route_frame_multiple_unmatched_calls_return_false() {
        let mut mgr = MultiWindowManager::new();

        for i in 1..=10 {
            let frame = make_frame(i, 0);
            assert!(!mgr.route_frame(frame));
        }

        // Nothing should have been added
        assert!(mgr.windows.is_empty());
    }

    // =======================================================================
    // any_dirty() / dirty_windows() / count() — empty manager
    // =======================================================================

    #[test]
    fn count_on_empty_manager() {
        let mgr = MultiWindowManager::new();
        assert_eq!(mgr.count(), 0);
    }

    #[test]
    fn any_dirty_on_empty_manager() {
        let mgr = MultiWindowManager::new();
        assert!(!mgr.any_dirty());
    }

    #[test]
    fn dirty_windows_on_empty_manager() {
        let mut mgr = MultiWindowManager::new();
        let dirty = mgr.dirty_windows();
        assert!(dirty.is_empty());
    }

    // =======================================================================
    // Queue draining semantics: request + process + request again
    // =======================================================================

    #[test]
    fn destroy_queue_refill_after_process() {
        let mut mgr = MultiWindowManager::new();

        mgr.request_destroy(1);
        mgr.request_destroy(2);
        mgr.process_destroys();
        assert!(mgr.pending_destroys.is_empty());

        // Queue new destroys after processing
        mgr.request_destroy(3);
        mgr.request_destroy(4);
        assert_eq!(mgr.pending_destroys.len(), 2);
        assert_eq!(mgr.pending_destroys[0], 3);
        assert_eq!(mgr.pending_destroys[1], 4);
    }

    // =======================================================================
    // Verify that route_frame passes frame_id correctly
    // =======================================================================

    #[test]
    fn route_frame_extracts_frame_id_and_parent_id() {
        let mut mgr = MultiWindowManager::new();

        // Create a frame with specific IDs
        let frame = make_frame(0xDEAD, 0xBEEF);
        assert_eq!(frame.frame_id, 0xDEAD);
        assert_eq!(frame.parent_id, 0xBEEF);

        // route_frame reads these fields for routing
        // No matching window, so returns false
        assert!(!mgr.route_frame(frame));
    }

    // =======================================================================
    // WindowState struct fields (verify field existence/types)
    // =======================================================================

    // Note: WindowState cannot be constructed in tests because it requires
    // Arc<Window>, wgpu::Surface, and wgpu::SurfaceConfiguration.
    // The following test just verifies the struct's field count and layout
    // by testing that the manager maps are properly typed.

    #[test]
    fn windows_map_key_is_u64() {
        let mgr = MultiWindowManager::new();
        // Verify the map accepts u64 keys
        assert!(mgr.windows.get(&0u64).is_none());
        assert!(mgr.windows.get(&u64::MAX).is_none());
    }

    // =======================================================================
    // Stress: many pending operations
    // =======================================================================

    #[test]
    fn many_pending_creates() {
        let mut mgr = MultiWindowManager::new();
        for i in 0..1000 {
            mgr.request_create(i, 800, 600, format!("Window {}", i));
        }
        assert_eq!(mgr.pending_creates.len(), 1000);
        assert_eq!(mgr.pending_creates[0].emacs_frame_id, 0);
        assert_eq!(mgr.pending_creates[999].emacs_frame_id, 999);
    }

    #[test]
    fn many_pending_destroys_processed() {
        let mut mgr = MultiWindowManager::new();
        for i in 0..1000 {
            mgr.request_destroy(i);
        }
        assert_eq!(mgr.pending_destroys.len(), 1000);

        mgr.process_destroys();
        assert!(mgr.pending_destroys.is_empty());
    }

    #[test]
    fn many_route_frame_misses() {
        let mut mgr = MultiWindowManager::new();
        for i in 1..=100 {
            let frame = make_frame(i, 0);
            assert!(!mgr.route_frame(frame));
        }
        assert!(mgr.windows.is_empty());
    }
}
