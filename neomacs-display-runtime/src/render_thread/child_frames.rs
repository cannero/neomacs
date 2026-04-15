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
#[path = "child_frames_test.rs"]
mod tests;
