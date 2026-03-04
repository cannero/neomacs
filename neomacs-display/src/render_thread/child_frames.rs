//! Child frame management for the render thread.
//!
//! Manages child frames (posframe, which-key-posframe, etc.) as floating
//! overlays composited on top of the parent frame within a single winit window.

use std::collections::HashMap;

use crate::core::frame_glyphs::FrameGlyphBuffer;

/// State for one child frame.
pub(crate) struct ChildFrameEntry {
    pub frame_id: u64,
    pub frame: FrameGlyphBuffer,
    /// Computed absolute position on screen (from parent_x/parent_y)
    pub abs_x: f32,
    pub abs_y: f32,
    /// Frame counter when this entry was last updated
    pub last_updated: u64,
}

/// Manages all child frames for the render thread.
pub(crate) struct ChildFrameManager {
    pub frames: HashMap<u64, ChildFrameEntry>,
    /// Frame IDs sorted by z_order for rendering (lowest first = back-most)
    render_order: Vec<u64>,
    /// Monotonic counter incremented each poll_frame cycle
    frame_counter: u64,
}

impl ChildFrameManager {
    pub fn new() -> Self {
        Self {
            frames: HashMap::new(),
            render_order: Vec::new(),
            frame_counter: 0,
        }
    }

    /// Increment the frame counter. Call once per poll_frame cycle.
    pub fn tick(&mut self) {
        self.frame_counter += 1;
    }

    /// Insert or update a child frame, recompute absolute position, rebuild render order.
    pub fn update_frame(&mut self, buf: FrameGlyphBuffer) {
        let frame_id = buf.frame_id;
        let abs_x = buf.parent_x;
        let abs_y = buf.parent_y;

        tracing::debug!(
            frame_id,
            abs_x,
            abs_y,
            width = buf.width,
            height = buf.height,
            z_order = buf.z_order,
            "child_frame_manager: update_frame"
        );

        self.frames.insert(
            frame_id,
            ChildFrameEntry {
                frame_id,
                frame: buf,
                abs_x,
                abs_y,
                last_updated: self.frame_counter,
            },
        );

        self.rebuild_render_order();
    }

    /// Remove a child frame by ID.
    pub fn remove_frame(&mut self, frame_id: u64) {
        if self.frames.remove(&frame_id).is_some() {
            self.rebuild_render_order();
        }
    }

    /// Remove child frames not updated in the last `max_age` poll cycles.
    pub fn prune_stale(&mut self, max_age: u64) {
        let threshold = self.frame_counter.saturating_sub(max_age);
        let before = self.frames.len();
        self.frames
            .retain(|_, entry| entry.last_updated >= threshold);
        if self.frames.len() != before {
            self.rebuild_render_order();
        }
    }

    /// Get the z_order-sorted list of frame IDs for rendering.
    pub fn sorted_for_rendering(&self) -> &[u64] {
        &self.render_order
    }

    /// Hit test: find the topmost child frame at the given point.
    /// Returns (frame_id, local_x, local_y) if hit, None otherwise.
    /// Iterates in reverse render order (topmost first).
    pub fn hit_test(&self, x: f32, y: f32) -> Option<(u64, f32, f32)> {
        for &frame_id in self.render_order.iter().rev() {
            if let Some(entry) = self.frames.get(&frame_id) {
                let local_x = x - entry.abs_x;
                let local_y = y - entry.abs_y;
                if local_x >= 0.0
                    && local_y >= 0.0
                    && local_x < entry.frame.width
                    && local_y < entry.frame.height
                {
                    return Some((frame_id, local_x, local_y));
                }
            }
        }
        None
    }

    /// Whether there are any child frames.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Rebuild the render order from z_order values.
    fn rebuild_render_order(&mut self) {
        self.render_order.clear();
        self.render_order.extend(self.frames.keys());
        // Sort by z_order ascending (lowest z = rendered first = behind)
        self.render_order.sort_by(|a, b| {
            let za = self.frames.get(a).map(|e| e.frame.z_order).unwrap_or(0);
            let zb = self.frames.get(b).map(|e| e.frame.z_order).unwrap_or(0);
            za.cmp(&zb)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a FrameGlyphBuffer with specified frame_id, position, size, and z_order.
    fn make_child_buf(
        frame_id: u64,
        parent_x: f32,
        parent_y: f32,
        width: f32,
        height: f32,
        z_order: i32,
    ) -> FrameGlyphBuffer {
        let mut buf = FrameGlyphBuffer::with_size(width, height);
        buf.frame_id = frame_id;
        buf.parent_x = parent_x;
        buf.parent_y = parent_y;
        buf.z_order = z_order;
        buf
    }

    // ===================================================================
    // Default / empty state
    // ===================================================================

    #[test]
    fn new_manager_is_empty() {
        let mgr = ChildFrameManager::new();
        assert!(mgr.is_empty());
        assert!(mgr.sorted_for_rendering().is_empty());
        assert!(mgr.frames.is_empty());
    }

    #[test]
    fn new_manager_frame_counter_starts_at_zero() {
        let mgr = ChildFrameManager::new();
        assert_eq!(mgr.frame_counter, 0);
    }

    #[test]
    fn empty_manager_hit_test_returns_none() {
        let mgr = ChildFrameManager::new();
        assert_eq!(mgr.hit_test(100.0, 100.0), None);
    }

    // ===================================================================
    // tick()
    // ===================================================================

    #[test]
    fn tick_increments_frame_counter() {
        let mut mgr = ChildFrameManager::new();
        assert_eq!(mgr.frame_counter, 0);
        mgr.tick();
        assert_eq!(mgr.frame_counter, 1);
        mgr.tick();
        assert_eq!(mgr.frame_counter, 2);
    }

    // ===================================================================
    // update_frame()
    // ===================================================================

    #[test]
    fn update_frame_inserts_single_frame() {
        let mut mgr = ChildFrameManager::new();
        let buf = make_child_buf(100, 50.0, 75.0, 200.0, 100.0, 0);
        mgr.update_frame(buf);

        assert!(!mgr.is_empty());
        assert_eq!(mgr.frames.len(), 1);
        assert!(mgr.frames.contains_key(&100));

        let entry = mgr.frames.get(&100).unwrap();
        assert_eq!(entry.frame_id, 100);
        assert_eq!(entry.abs_x, 50.0);
        assert_eq!(entry.abs_y, 75.0);
        assert_eq!(entry.frame.width, 200.0);
        assert_eq!(entry.frame.height, 100.0);
    }

    #[test]
    fn update_frame_sets_last_updated_to_current_counter() {
        let mut mgr = ChildFrameManager::new();
        mgr.tick(); // counter = 1
        mgr.tick(); // counter = 2

        let buf = make_child_buf(10, 0.0, 0.0, 100.0, 100.0, 0);
        mgr.update_frame(buf);

        let entry = mgr.frames.get(&10).unwrap();
        assert_eq!(entry.last_updated, 2);
    }

    #[test]
    fn update_frame_replaces_existing_frame() {
        let mut mgr = ChildFrameManager::new();

        // Insert initial
        let buf1 = make_child_buf(42, 10.0, 20.0, 100.0, 50.0, 1);
        mgr.update_frame(buf1);

        // Replace with new position and size
        let buf2 = make_child_buf(42, 30.0, 40.0, 200.0, 80.0, 2);
        mgr.update_frame(buf2);

        assert_eq!(mgr.frames.len(), 1);
        let entry = mgr.frames.get(&42).unwrap();
        assert_eq!(entry.abs_x, 30.0);
        assert_eq!(entry.abs_y, 40.0);
        assert_eq!(entry.frame.width, 200.0);
        assert_eq!(entry.frame.height, 80.0);
        assert_eq!(entry.frame.z_order, 2);
    }

    #[test]
    fn update_frame_abs_position_from_parent_xy() {
        let mut mgr = ChildFrameManager::new();
        let buf = make_child_buf(1, 123.5, 456.7, 300.0, 200.0, 0);
        mgr.update_frame(buf);

        let entry = mgr.frames.get(&1).unwrap();
        assert_eq!(entry.abs_x, 123.5);
        assert_eq!(entry.abs_y, 456.7);
    }

    // ===================================================================
    // Z-order sorting (render_order / sorted_for_rendering)
    // ===================================================================

    #[test]
    fn sorted_for_rendering_orders_by_z_order_ascending() {
        let mut mgr = ChildFrameManager::new();

        // Insert in non-sorted order
        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 10));
        mgr.update_frame(make_child_buf(2, 0.0, 0.0, 100.0, 100.0, 1));
        mgr.update_frame(make_child_buf(3, 0.0, 0.0, 100.0, 100.0, 5));

        let order = mgr.sorted_for_rendering();
        assert_eq!(order.len(), 3);
        assert_eq!(order[0], 2); // z_order=1, lowest
        assert_eq!(order[1], 3); // z_order=5
        assert_eq!(order[2], 1); // z_order=10, highest
    }

    #[test]
    fn sorted_for_rendering_handles_negative_z_order() {
        let mut mgr = ChildFrameManager::new();

        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 0));
        mgr.update_frame(make_child_buf(2, 0.0, 0.0, 100.0, 100.0, -5));
        mgr.update_frame(make_child_buf(3, 0.0, 0.0, 100.0, 100.0, 5));

        let order = mgr.sorted_for_rendering();
        assert_eq!(order[0], 2); // z_order=-5
        assert_eq!(order[1], 1); // z_order=0
        assert_eq!(order[2], 3); // z_order=5
    }

    #[test]
    fn sorted_for_rendering_same_z_order_is_stable_count() {
        let mut mgr = ChildFrameManager::new();

        // All same z_order -- we just verify all 3 are present
        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 0));
        mgr.update_frame(make_child_buf(2, 0.0, 0.0, 100.0, 100.0, 0));
        mgr.update_frame(make_child_buf(3, 0.0, 0.0, 100.0, 100.0, 0));

        let order = mgr.sorted_for_rendering();
        assert_eq!(order.len(), 3);
        let mut sorted = order.to_vec();
        sorted.sort();
        assert_eq!(sorted, vec![1, 2, 3]);
    }

    #[test]
    fn render_order_updates_when_z_order_changes() {
        let mut mgr = ChildFrameManager::new();

        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 1));
        mgr.update_frame(make_child_buf(2, 0.0, 0.0, 100.0, 100.0, 2));

        // Frame 1 should be first (lower z)
        assert_eq!(mgr.sorted_for_rendering()[0], 1);

        // Now update frame 1 with higher z_order
        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 10));

        // Frame 2 should now be first
        assert_eq!(mgr.sorted_for_rendering()[0], 2);
        assert_eq!(mgr.sorted_for_rendering()[1], 1);
    }

    // ===================================================================
    // remove_frame()
    // ===================================================================

    #[test]
    fn remove_frame_removes_existing() {
        let mut mgr = ChildFrameManager::new();
        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 0));
        mgr.update_frame(make_child_buf(2, 0.0, 0.0, 100.0, 100.0, 1));

        mgr.remove_frame(1);
        assert_eq!(mgr.frames.len(), 1);
        assert!(!mgr.frames.contains_key(&1));
        assert!(mgr.frames.contains_key(&2));
        assert_eq!(mgr.sorted_for_rendering(), &[2]);
    }

    #[test]
    fn remove_frame_nonexistent_is_noop() {
        let mut mgr = ChildFrameManager::new();
        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 0));

        mgr.remove_frame(999); // does not exist
        assert_eq!(mgr.frames.len(), 1);
        assert_eq!(mgr.sorted_for_rendering().len(), 1);
    }

    #[test]
    fn remove_all_frames_leaves_empty() {
        let mut mgr = ChildFrameManager::new();
        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 0));
        mgr.update_frame(make_child_buf(2, 0.0, 0.0, 100.0, 100.0, 1));

        mgr.remove_frame(1);
        mgr.remove_frame(2);
        assert!(mgr.is_empty());
        assert!(mgr.sorted_for_rendering().is_empty());
    }

    // ===================================================================
    // hit_test()
    // ===================================================================

    #[test]
    fn hit_test_single_frame_inside() {
        let mut mgr = ChildFrameManager::new();
        // Frame at (100, 200) with size 300x150
        mgr.update_frame(make_child_buf(1, 100.0, 200.0, 300.0, 150.0, 0));

        // Click inside the frame
        let result = mgr.hit_test(150.0, 250.0);
        assert!(result.is_some());
        let (frame_id, local_x, local_y) = result.unwrap();
        assert_eq!(frame_id, 1);
        assert_eq!(local_x, 50.0); // 150 - 100
        assert_eq!(local_y, 50.0); // 250 - 200
    }

    #[test]
    fn hit_test_single_frame_outside() {
        let mut mgr = ChildFrameManager::new();
        mgr.update_frame(make_child_buf(1, 100.0, 200.0, 300.0, 150.0, 0));

        // Click to the left
        assert_eq!(mgr.hit_test(50.0, 250.0), None);
        // Click above
        assert_eq!(mgr.hit_test(150.0, 150.0), None);
        // Click to the right (400 = 100 + 300, boundary is exclusive)
        assert_eq!(mgr.hit_test(400.0, 250.0), None);
        // Click below (350 = 200 + 150, boundary is exclusive)
        assert_eq!(mgr.hit_test(150.0, 350.0), None);
    }

    #[test]
    fn hit_test_frame_boundary_top_left_corner() {
        let mut mgr = ChildFrameManager::new();
        mgr.update_frame(make_child_buf(1, 100.0, 200.0, 300.0, 150.0, 0));

        // Exact top-left corner (inclusive)
        let result = mgr.hit_test(100.0, 200.0);
        assert!(result.is_some());
        let (_, lx, ly) = result.unwrap();
        assert_eq!(lx, 0.0);
        assert_eq!(ly, 0.0);
    }

    #[test]
    fn hit_test_frame_boundary_bottom_right_exclusive() {
        let mut mgr = ChildFrameManager::new();
        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 0));

        // Right at the boundary (100.0 is exclusive)
        assert_eq!(mgr.hit_test(100.0, 50.0), None);
        assert_eq!(mgr.hit_test(50.0, 100.0), None);

        // Just inside the boundary
        let result = mgr.hit_test(99.9, 99.9);
        assert!(result.is_some());
    }

    #[test]
    fn hit_test_overlapping_frames_returns_topmost() {
        let mut mgr = ChildFrameManager::new();

        // Frame A: z_order=1, at (0,0) 200x200
        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 200.0, 200.0, 1));
        // Frame B: z_order=10, at (50,50) 200x200 -- overlaps with A, on top
        mgr.update_frame(make_child_buf(2, 50.0, 50.0, 200.0, 200.0, 10));

        // Click in overlap region (100, 100)
        let result = mgr.hit_test(100.0, 100.0);
        assert!(result.is_some());
        let (frame_id, local_x, local_y) = result.unwrap();
        assert_eq!(frame_id, 2); // Topmost (higher z_order)
        assert_eq!(local_x, 50.0); // 100 - 50
        assert_eq!(local_y, 50.0); // 100 - 50
    }

    #[test]
    fn hit_test_overlapping_frames_returns_lower_when_top_not_hit() {
        let mut mgr = ChildFrameManager::new();

        // Frame A: z_order=1, at (0,0) 200x200
        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 200.0, 200.0, 1));
        // Frame B: z_order=10, at (50,50) 200x200
        mgr.update_frame(make_child_buf(2, 50.0, 50.0, 200.0, 200.0, 10));

        // Click in area only covered by Frame A (25, 25)
        let result = mgr.hit_test(25.0, 25.0);
        assert!(result.is_some());
        let (frame_id, local_x, local_y) = result.unwrap();
        assert_eq!(frame_id, 1); // Only frame A covers this point
        assert_eq!(local_x, 25.0);
        assert_eq!(local_y, 25.0);
    }

    #[test]
    fn hit_test_no_frames_hit() {
        let mut mgr = ChildFrameManager::new();
        mgr.update_frame(make_child_buf(1, 100.0, 100.0, 50.0, 50.0, 0));
        mgr.update_frame(make_child_buf(2, 200.0, 200.0, 50.0, 50.0, 1));

        // Click between the two frames
        assert_eq!(mgr.hit_test(175.0, 175.0), None);
    }

    #[test]
    fn hit_test_three_overlapping_layers() {
        let mut mgr = ChildFrameManager::new();

        // Three frames all overlapping at (100, 100), different z orders
        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 200.0, 200.0, 1));
        mgr.update_frame(make_child_buf(2, 0.0, 0.0, 200.0, 200.0, 5));
        mgr.update_frame(make_child_buf(3, 0.0, 0.0, 200.0, 200.0, 10));

        let result = mgr.hit_test(100.0, 100.0);
        assert_eq!(result.unwrap().0, 3); // Topmost z_order=10
    }

    // ===================================================================
    // prune_stale()
    // ===================================================================

    #[test]
    fn prune_stale_removes_old_frames() {
        let mut mgr = ChildFrameManager::new();

        // Frame inserted at counter=0
        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 0));

        // Advance counter by 5
        for _ in 0..5 {
            mgr.tick();
        }

        // Frame inserted at counter=5
        mgr.update_frame(make_child_buf(2, 0.0, 0.0, 100.0, 100.0, 1));

        assert_eq!(mgr.frames.len(), 2);

        // Prune frames not updated in last 3 cycles
        // counter=5, threshold=5-3=2, so frame 1 (updated at 0) is stale
        mgr.prune_stale(3);

        assert_eq!(mgr.frames.len(), 1);
        assert!(!mgr.frames.contains_key(&1));
        assert!(mgr.frames.contains_key(&2));
    }

    #[test]
    fn prune_stale_keeps_recently_updated() {
        let mut mgr = ChildFrameManager::new();

        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 0));
        mgr.tick(); // counter=1

        // Frame updated at counter=0, max_age=5, threshold=1-5=0 (saturating)
        // So threshold=0, frame updated at 0 >= 0, keep it
        mgr.prune_stale(5);

        assert_eq!(mgr.frames.len(), 1);
    }

    #[test]
    fn prune_stale_no_frames_is_noop() {
        let mut mgr = ChildFrameManager::new();
        mgr.tick();
        mgr.prune_stale(1);
        assert!(mgr.is_empty());
    }

    #[test]
    fn prune_stale_all_stale_leaves_empty() {
        let mut mgr = ChildFrameManager::new();

        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 0));
        mgr.update_frame(make_child_buf(2, 0.0, 0.0, 100.0, 100.0, 1));

        // Advance far past both frames
        for _ in 0..100 {
            mgr.tick();
        }

        mgr.prune_stale(1);
        assert!(mgr.is_empty());
        assert!(mgr.sorted_for_rendering().is_empty());
    }

    #[test]
    fn prune_stale_updates_render_order() {
        let mut mgr = ChildFrameManager::new();

        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 1));
        mgr.tick(); // counter=1
        mgr.tick(); // counter=2
        mgr.tick(); // counter=3

        mgr.update_frame(make_child_buf(2, 0.0, 0.0, 100.0, 100.0, 2));

        // Frame 1 updated at 0, frame 2 updated at 3
        // Prune with max_age=2: threshold=3-2=1, frame 1 (0 < 1) is stale
        mgr.prune_stale(2);

        let order = mgr.sorted_for_rendering();
        assert_eq!(order.len(), 1);
        assert_eq!(order[0], 2);
    }

    #[test]
    fn prune_stale_boundary_exact_threshold() {
        let mut mgr = ChildFrameManager::new();

        // counter=0, insert frame
        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 0));

        mgr.tick(); // counter=1
        mgr.tick(); // counter=2

        // max_age=2, counter=2, threshold=2-2=0
        // Frame updated at 0 >= 0, so it should be kept (boundary case)
        mgr.prune_stale(2);
        assert_eq!(mgr.frames.len(), 1);
    }

    #[test]
    fn prune_stale_max_age_zero_removes_all_except_current() {
        let mut mgr = ChildFrameManager::new();

        // Insert at counter=0
        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 0));

        // max_age=0, counter=0, threshold=0-0=0
        // Frame updated at 0 >= 0, keep
        mgr.prune_stale(0);
        assert_eq!(mgr.frames.len(), 1);

        // Now tick and prune with 0
        mgr.tick(); // counter=1
        // threshold=1-0=1, frame updated at 0 < 1, stale
        mgr.prune_stale(0);
        assert!(mgr.is_empty());
    }

    #[test]
    fn prune_stale_saturating_sub_prevents_underflow() {
        let mut mgr = ChildFrameManager::new();
        // counter=0, max_age=1000 -- saturating_sub yields 0
        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 0));
        mgr.prune_stale(1000);
        // Frame updated at 0 >= 0, so it should be kept
        assert_eq!(mgr.frames.len(), 1);
    }

    // ===================================================================
    // render_order() after insertions and removals
    // ===================================================================

    #[test]
    fn render_order_correct_after_mixed_operations() {
        let mut mgr = ChildFrameManager::new();

        // Insert 3 frames
        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 10));
        mgr.update_frame(make_child_buf(2, 0.0, 0.0, 100.0, 100.0, 5));
        mgr.update_frame(make_child_buf(3, 0.0, 0.0, 100.0, 100.0, 15));

        // Order should be: 2(z=5), 1(z=10), 3(z=15)
        assert_eq!(mgr.sorted_for_rendering(), &[2, 1, 3]);

        // Remove middle one
        mgr.remove_frame(1);
        assert_eq!(mgr.sorted_for_rendering(), &[2, 3]);

        // Add new frame with z_order between remaining
        mgr.update_frame(make_child_buf(4, 0.0, 0.0, 100.0, 100.0, 7));
        assert_eq!(mgr.sorted_for_rendering(), &[2, 4, 3]);
    }

    #[test]
    fn render_order_after_update_preserves_all_frames() {
        let mut mgr = ChildFrameManager::new();

        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 1));
        mgr.update_frame(make_child_buf(2, 0.0, 0.0, 100.0, 100.0, 2));
        mgr.update_frame(make_child_buf(3, 0.0, 0.0, 100.0, 100.0, 3));

        // Update frame 2 with new z_order
        mgr.update_frame(make_child_buf(2, 0.0, 0.0, 100.0, 100.0, 100));

        let order = mgr.sorted_for_rendering();
        assert_eq!(order.len(), 3);
        // Order: 1(z=1), 3(z=3), 2(z=100)
        assert_eq!(order[0], 1);
        assert_eq!(order[1], 3);
        assert_eq!(order[2], 2);
    }

    #[test]
    fn render_order_single_frame() {
        let mut mgr = ChildFrameManager::new();
        mgr.update_frame(make_child_buf(42, 10.0, 20.0, 300.0, 200.0, 0));

        let order = mgr.sorted_for_rendering();
        assert_eq!(order.len(), 1);
        assert_eq!(order[0], 42);
    }

    // ===================================================================
    // is_empty()
    // ===================================================================

    #[test]
    fn is_empty_after_insert_and_remove() {
        let mut mgr = ChildFrameManager::new();
        assert!(mgr.is_empty());

        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 0));
        assert!(!mgr.is_empty());

        mgr.remove_frame(1);
        assert!(mgr.is_empty());
    }

    // ===================================================================
    // Combined: hit_test after prune_stale
    // ===================================================================

    #[test]
    fn hit_test_after_prune_stale() {
        let mut mgr = ChildFrameManager::new();

        // Insert two non-overlapping frames
        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 100.0, 100.0, 0));

        // Advance and insert second
        for _ in 0..10 {
            mgr.tick();
        }
        mgr.update_frame(make_child_buf(2, 200.0, 0.0, 100.0, 100.0, 1));

        // Prune frame 1 (stale)
        mgr.prune_stale(3);
        assert_eq!(mgr.frames.len(), 1);

        // Hit test on frame 1's position should miss
        assert_eq!(mgr.hit_test(50.0, 50.0), None);

        // Hit test on frame 2's position should hit
        let result = mgr.hit_test(250.0, 50.0);
        assert!(result.is_some());
        assert_eq!(result.unwrap().0, 2);
    }

    // ===================================================================
    // Combined: z-order and hit test interaction
    // ===================================================================

    #[test]
    fn hit_test_respects_updated_z_order() {
        let mut mgr = ChildFrameManager::new();

        // Two overlapping frames, frame 1 on top
        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 200.0, 200.0, 10));
        mgr.update_frame(make_child_buf(2, 0.0, 0.0, 200.0, 200.0, 5));

        // Frame 1 is on top
        assert_eq!(mgr.hit_test(100.0, 100.0).unwrap().0, 1);

        // Swap z-orders by updating
        mgr.update_frame(make_child_buf(1, 0.0, 0.0, 200.0, 200.0, 1));
        mgr.update_frame(make_child_buf(2, 0.0, 0.0, 200.0, 200.0, 20));

        // Now frame 2 is on top
        assert_eq!(mgr.hit_test(100.0, 100.0).unwrap().0, 2);
    }
}
