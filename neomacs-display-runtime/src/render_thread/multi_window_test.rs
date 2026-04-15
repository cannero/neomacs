use super::*;
use crate::core::frame_glyphs::FrameGlyphBuffer;

// =======================================================================
// Helper: create a FrameGlyphBuffer with specified identity fields
// =======================================================================

fn default_geometry_hints() -> GuiFrameGeometryHints {
    GuiFrameGeometryHints {
        base_width: 24,
        base_height: 16,
        min_width: 24,
        min_height: 16,
        width_inc: 8,
        height_inc: 16,
    }
}

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
    mgr.request_create(
        1,
        800,
        600,
        "Test Window".to_string(),
        default_geometry_hints(),
    );

    assert_eq!(mgr.pending_creates.len(), 1);
    assert_eq!(mgr.pending_creates[0].emacs_frame_id, 1);
    assert_eq!(mgr.pending_creates[0].width, 800);
    assert_eq!(mgr.pending_creates[0].height, 600);
    assert_eq!(mgr.pending_creates[0].title, "Test Window");
}

#[test]
fn request_create_multiple_preserves_order() {
    let mut mgr = MultiWindowManager::new();
    mgr.request_create(
        1,
        800,
        600,
        "Window 1".to_string(),
        default_geometry_hints(),
    );
    mgr.request_create(
        2,
        1024,
        768,
        "Window 2".to_string(),
        default_geometry_hints(),
    );
    mgr.request_create(
        3,
        1920,
        1080,
        "Window 3".to_string(),
        default_geometry_hints(),
    );

    assert_eq!(mgr.pending_creates.len(), 3);
    assert_eq!(mgr.pending_creates[0].emacs_frame_id, 1);
    assert_eq!(mgr.pending_creates[1].emacs_frame_id, 2);
    assert_eq!(mgr.pending_creates[2].emacs_frame_id, 3);
}

#[test]
fn request_create_does_not_modify_windows_map() {
    let mut mgr = MultiWindowManager::new();
    mgr.request_create(1, 800, 600, "Test".to_string(), default_geometry_hints());

    // The window should NOT be in the windows map yet —
    // only in the pending queue until process_creates runs
    assert!(mgr.windows.is_empty());
    assert_eq!(mgr.count(), 0);
}

#[test]
fn request_create_allows_duplicate_frame_ids() {
    let mut mgr = MultiWindowManager::new();
    mgr.request_create(1, 800, 600, "First".to_string(), default_geometry_hints());
    mgr.request_create(
        1,
        1024,
        768,
        "Duplicate".to_string(),
        default_geometry_hints(),
    );

    // Both are queued (process_creates will skip duplicates)
    assert_eq!(mgr.pending_creates.len(), 2);
}

#[test]
fn request_create_zero_dimensions() {
    let mut mgr = MultiWindowManager::new();
    mgr.request_create(1, 0, 0, "Zero".to_string(), default_geometry_hints());

    assert_eq!(mgr.pending_creates.len(), 1);
    assert_eq!(mgr.pending_creates[0].width, 0);
    assert_eq!(mgr.pending_creates[0].height, 0);
}

#[test]
fn request_create_empty_title() {
    let mut mgr = MultiWindowManager::new();
    mgr.request_create(1, 800, 600, String::new(), default_geometry_hints());

    assert_eq!(mgr.pending_creates[0].title, "");
}

#[test]
fn request_create_large_frame_id() {
    let mut mgr = MultiWindowManager::new();
    let large_id = u64::MAX;
    mgr.request_create(
        large_id,
        800,
        600,
        "Max ID".to_string(),
        default_geometry_hints(),
    );

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
        geometry_hints: default_geometry_hints(),
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
        geometry_hints: default_geometry_hints(),
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

    mgr.request_create(1, 800, 600, "Win1".to_string(), default_geometry_hints());
    mgr.request_create(2, 1024, 768, "Win2".to_string(), default_geometry_hints());
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
        mgr.request_create(
            i,
            800,
            600,
            format!("Window {}", i),
            default_geometry_hints(),
        );
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
