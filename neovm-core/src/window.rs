//! Window and frame management for the editor.
//!
//! Implements the Emacs window tree model:
//! - A **frame** contains a root window (which may be split).
//! - A **window** is either a *leaf* (displays a buffer) or an *internal*
//!   node with children (horizontal or vertical split).
//! - The **selected window** is the one receiving input.
//! - The **minibuffer window** is a special single-line window at the bottom.

use crate::buffer::BufferId;
use crate::emacs_core::value::{Value, eq_value};
use crate::face::Face as RuntimeFace;
use crate::gc::GcTrace;
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// IDs
// ---------------------------------------------------------------------------

/// Opaque window identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowId(pub u64);

/// Opaque frame identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameId(pub u64);

/// Keep frame and window numeric domains disjoint while both are represented
/// as Lisp integers.
pub(crate) const FRAME_ID_BASE: u64 = 1 << 32;
/// Synthetic window-id domain reserved for per-frame minibuffer windows.
pub(crate) const MINIBUFFER_WINDOW_ID_BASE: u64 = 1 << 48;

// ---------------------------------------------------------------------------
// Window geometry
// ---------------------------------------------------------------------------

/// Pixel-based rectangle for window placement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
}

// ---------------------------------------------------------------------------
// Split direction
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitDirection {
    Horizontal, // side by side
    Vertical,   // stacked
}

// ---------------------------------------------------------------------------
// Window
// ---------------------------------------------------------------------------

/// A window in the window tree.
#[derive(Clone, Debug)]
pub enum Window {
    /// Leaf window displaying a buffer.
    Leaf {
        id: WindowId,
        buffer_id: BufferId,
        /// Pixel bounds within the frame.
        bounds: Rect,
        /// Character position of the first visible character.
        window_start: usize,
        /// Offset of the last displayed character position from buffer `Z`.
        ///
        /// Mirrors GNU Emacs `w->window_end_pos`, so Lisp-visible
        /// `window-end` can continue to track buffer growth/shrinkage even
        /// between redisplays.
        window_end_pos: usize,
        /// Offset of the last displayed byte position from buffer `Z_BYTE`.
        ///
        /// This is the byte-position companion to `window_end_pos`.
        window_end_bytepos: usize,
        /// Visual row that produced `window_end_pos`.
        window_end_vpos: usize,
        /// Whether the last completed redisplay recorded window-end state.
        window_end_valid: bool,
        /// Cursor (point) position in this window.
        point: usize,
        /// Whether this is a dedicated window.
        dedicated: bool,
        /// Window parameters (name -> value).
        parameters: HashMap<String, Value>,
        /// Desired height in lines (for fixed windows, 0 = flexible).
        fixed_height: usize,
        /// Desired width in columns (for fixed windows, 0 = flexible).
        fixed_width: usize,
        /// Horizontal scroll offset (columns).
        hscroll: usize,
        /// Window margins (left, right) in columns.
        margins: (usize, usize),
        /// Fringes (left_width, right_width) in pixels.
        fringes: (u32, u32),
    },

    /// Internal node: contains children split in a direction.
    Internal {
        id: WindowId,
        direction: SplitDirection,
        children: Vec<Window>,
        bounds: Rect,
        /// Combination limit — prevents recombination when non-nil.
        /// Mirrors GNU Emacs `w->combination_limit`.
        combination_limit: bool,
    },
}

impl Window {
    /// Create a new leaf window.
    pub fn new_leaf(id: WindowId, buffer_id: BufferId, bounds: Rect) -> Self {
        Window::Leaf {
            id,
            buffer_id,
            bounds,
            window_start: 1,
            window_end_pos: 0,
            window_end_bytepos: 0,
            window_end_vpos: 0,
            window_end_valid: false,
            point: 1,
            dedicated: false,
            parameters: HashMap::new(),
            fixed_height: 0,
            fixed_width: 0,
            hscroll: 0,
            margins: (0, 0),
            fringes: (8, 8),
        }
    }

    /// Window ID.
    pub fn id(&self) -> WindowId {
        match self {
            Window::Leaf { id, .. } | Window::Internal { id, .. } => *id,
        }
    }

    /// Pixel bounds.
    pub fn bounds(&self) -> &Rect {
        match self {
            Window::Leaf { bounds, .. } | Window::Internal { bounds, .. } => bounds,
        }
    }

    /// Mutable reference to bounds.
    pub fn bounds_mut(&mut self) -> &mut Rect {
        match self {
            Window::Leaf { bounds, .. } | Window::Internal { bounds, .. } => bounds,
        }
    }

    /// Set bounds.
    pub fn set_bounds(&mut self, new_bounds: Rect) {
        match self {
            Window::Leaf { bounds, .. } | Window::Internal { bounds, .. } => {
                *bounds = new_bounds;
            }
        }
    }

    /// Whether this is a leaf window.
    pub fn is_leaf(&self) -> bool {
        matches!(self, Window::Leaf { .. })
    }

    /// Get the combination limit for an internal window.
    pub fn combination_limit(&self) -> Option<bool> {
        match self {
            Window::Internal {
                combination_limit, ..
            } => Some(*combination_limit),
            Window::Leaf { .. } => None,
        }
    }

    /// Set the combination limit for an internal window.
    pub fn set_combination_limit(&mut self, limit: bool) {
        if let Window::Internal {
            combination_limit, ..
        } = self
        {
            *combination_limit = limit;
        }
    }

    /// Buffer displayed in this window (leaf only).
    pub fn buffer_id(&self) -> Option<BufferId> {
        match self {
            Window::Leaf { buffer_id, .. } => Some(*buffer_id),
            Window::Internal { .. } => None,
        }
    }

    /// Set the buffer displayed in this window (leaf only).
    pub fn set_buffer(&mut self, new_id: BufferId) {
        if let Window::Leaf {
            buffer_id,
            window_start,
            window_end_pos,
            window_end_bytepos,
            window_end_vpos,
            window_end_valid,
            point,
            ..
        } = self
        {
            *buffer_id = new_id;
            // Emacs positions are 1-based; switching the displayed buffer resets
            // window-start/point to point-min.
            *window_start = 1;
            *window_end_pos = 0;
            *window_end_bytepos = 0;
            *window_end_vpos = 0;
            *window_end_valid = false;
            *point = 1;
        }
    }

    /// Stored Lisp-visible `window-end` for this leaf window.
    pub fn window_end_charpos(&self, buffer_z: usize) -> Option<usize> {
        match self {
            Window::Leaf { window_end_pos, .. } => Some(buffer_z.saturating_sub(*window_end_pos)),
            Window::Internal { .. } => None,
        }
    }

    /// Stored byte-position `window-end` for this leaf window.
    pub fn window_end_bytepos(&self, buffer_z_byte: usize) -> Option<usize> {
        match self {
            Window::Leaf {
                window_end_bytepos, ..
            } => Some(buffer_z_byte.saturating_sub(*window_end_bytepos)),
            Window::Internal { .. } => None,
        }
    }

    /// Whether the stored window-end came from a completed redisplay.
    pub fn window_end_valid(&self) -> Option<bool> {
        match self {
            Window::Leaf {
                window_end_valid, ..
            } => Some(*window_end_valid),
            Window::Internal { .. } => None,
        }
    }

    /// Publish the last redisplay's window-end state for this leaf window.
    pub fn set_window_end_from_positions(
        &mut self,
        buffer_z_char: usize,
        buffer_z_byte: usize,
        end_charpos: usize,
        end_bytepos: usize,
        vpos: usize,
    ) {
        if let Window::Leaf {
            window_end_pos,
            window_end_bytepos,
            window_end_vpos,
            window_end_valid,
            ..
        } = self
        {
            *window_end_pos = buffer_z_char.saturating_sub(end_charpos.min(buffer_z_char));
            *window_end_bytepos = buffer_z_byte.saturating_sub(end_bytepos.min(buffer_z_byte));
            *window_end_vpos = vpos;
            *window_end_valid = true;
        }
    }

    /// Replace a displayed buffer id in all leaf windows under this node.
    ///
    /// This is used when a buffer is killed; any window still attached to the
    /// dead buffer is moved back to a replacement buffer (typically `*scratch*`).
    pub fn replace_buffer_id(&mut self, old_id: BufferId, new_id: BufferId) {
        match self {
            Window::Leaf { buffer_id, .. } => {
                if *buffer_id == old_id {
                    self.set_buffer(new_id);
                }
            }
            Window::Internal { children, .. } => {
                for child in children {
                    child.replace_buffer_id(old_id, new_id);
                }
            }
        }
    }

    /// Find a leaf window by ID in this subtree.
    pub fn find(&self, target: WindowId) -> Option<&Window> {
        if self.id() == target {
            return Some(self);
        }
        if let Window::Internal { children, .. } = self {
            for child in children {
                if let Some(w) = child.find(target) {
                    return Some(w);
                }
            }
        }
        None
    }

    /// Find a mutable leaf window by ID in this subtree.
    pub fn find_mut(&mut self, target: WindowId) -> Option<&mut Window> {
        if self.id() == target {
            return Some(self);
        }
        if let Window::Internal { children, .. } = self {
            for child in children {
                if let Some(w) = child.find_mut(target) {
                    return Some(w);
                }
            }
        }
        None
    }

    /// Collect all leaf window IDs.
    pub fn leaf_ids(&self) -> Vec<WindowId> {
        let mut result = Vec::new();
        self.collect_leaves(&mut result);
        result
    }

    fn collect_leaves(&self, out: &mut Vec<WindowId>) {
        match self {
            Window::Leaf { id, .. } => out.push(*id),
            Window::Internal { children, .. } => {
                for child in children {
                    child.collect_leaves(out);
                }
            }
        }
    }

    /// Find the window at pixel coordinates.
    pub fn window_at(&self, px: f32, py: f32) -> Option<WindowId> {
        match self {
            Window::Leaf { id, bounds, .. } => {
                if bounds.contains(px, py) {
                    Some(*id)
                } else {
                    None
                }
            }
            Window::Internal {
                children, bounds, ..
            } => {
                if !bounds.contains(px, py) {
                    return None;
                }
                for child in children {
                    if let Some(id) = child.window_at(px, py) {
                        return Some(id);
                    }
                }
                None
            }
        }
    }

    /// Count leaf windows in this subtree.
    pub fn leaf_count(&self) -> usize {
        match self {
            Window::Leaf { .. } => 1,
            Window::Internal { children, .. } => children.iter().map(|c| c.leaf_count()).sum(),
        }
    }

    /// Invalidate redisplay-derived window-end state for this subtree.
    pub fn invalidate_display_state(&mut self) {
        match self {
            Window::Leaf {
                window_end_valid, ..
            } => {
                *window_end_valid = false;
            }
            Window::Internal { children, .. } => {
                for child in children {
                    child.invalidate_display_state();
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Last Display Snapshot
// ---------------------------------------------------------------------------

/// Authoritative glyph geometry for a single visible buffer position.
///
/// These records are published by redisplay after layout so editor-side
/// queries like `posn-at-point` can answer from the actual rendered result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayPointSnapshot {
    /// 1-based buffer position of the source character.
    pub buffer_pos: usize,
    /// X relative to the text area's left edge, in pixels.
    pub x: i64,
    /// Y relative to the window's top edge, in pixels.
    pub y: i64,
    /// Rendered advance/width in pixels.
    pub width: i64,
    /// Rendered glyph height in pixels.
    pub height: i64,
    /// Visual row number in the window (0-based).
    pub row: i64,
    /// Visual column start for this source position.
    pub col: i64,
}

/// Per-row metrics from the last redisplay of a window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayRowSnapshot {
    /// Visual row number in the window (0-based).
    pub row: i64,
    /// Y relative to the window's top edge, in pixels.
    pub y: i64,
    /// Row height in pixels.
    pub height: i64,
    /// First buffer position represented on this row, if any.
    pub start_buffer_pos: Option<usize>,
    /// Last visible/source position associated with this row, if any.
    pub end_buffer_pos: Option<usize>,
}

/// Last authoritative redisplay geometry for a live leaf window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowDisplaySnapshot {
    /// Window identifier this snapshot belongs to.
    pub window_id: WindowId,
    /// Text-area offset from the window's left edge, in pixels.
    pub text_area_left_offset: i64,
    /// Last redisplay mode-line height in pixels.
    pub mode_line_height: i64,
    /// Last redisplay header-line height in pixels.
    pub header_line_height: i64,
    /// Last redisplay tab-line height in pixels.
    pub tab_line_height: i64,
    /// Visible source-position geometry, sorted by `buffer_pos`.
    pub points: Vec<DisplayPointSnapshot>,
    /// Visible row metrics, sorted by `row`.
    pub rows: Vec<DisplayRowSnapshot>,
}

impl WindowDisplaySnapshot {
    pub fn visible_buffer_span(&self) -> Option<(usize, usize)> {
        let start = self
            .rows
            .iter()
            .find_map(|row| row.start_buffer_pos)
            .or_else(|| self.points.first().map(|point| point.buffer_pos))?;
        let end = self
            .rows
            .iter()
            .rev()
            .find_map(|row| row.end_buffer_pos)
            .or_else(|| self.points.last().map(|point| point.buffer_pos))?;
        Some((start, end))
    }

    fn row_for_buffer_pos(&self, pos: usize) -> Option<&DisplayRowSnapshot> {
        self.rows.iter().find(|row| {
            let Some(start) = row.start_buffer_pos else {
                return false;
            };
            let Some(end) = row.end_buffer_pos else {
                return false;
            };
            start <= pos && pos <= end
        })
    }

    /// Return the visible point for POS, or the nearest visible neighbor when
    /// POS itself is hidden by redisplay within the visible span.
    ///
    /// Off-window positions return `None`, matching GNU Emacs `posn-at-point`
    /// and `pos-visible-in-window-p` semantics.
    pub fn point_for_buffer_pos(&self, pos: usize) -> Option<&DisplayPointSnapshot> {
        if self.points.is_empty() {
            return None;
        }
        let (visible_start, visible_end) = self.visible_buffer_span()?;
        if pos < visible_start || pos > visible_end {
            return None;
        }
        match self
            .points
            .binary_search_by_key(&pos, |point| point.buffer_pos)
        {
            Ok(idx) => self.points.get(idx),
            Err(_) => {
                let row = self.row_for_buffer_pos(pos)?;
                let next_on_row = self
                    .points
                    .iter()
                    .find(|point| point.row == row.row && point.buffer_pos > pos);
                let prev_on_row = self
                    .points
                    .iter()
                    .rev()
                    .find(|point| point.row == row.row && point.buffer_pos < pos);
                match (prev_on_row, next_on_row) {
                    // GNU `posn-at-point` may report neighboring positions when
                    // the requested buffer position is hidden by redisplay
                    // within the same visible row, but it returns nil when the
                    // position is not visible at all.
                    (Some(_), Some(next)) => Some(next),
                    _ => None,
                }
            }
        }
    }

    /// Return the visible point nearest to window-relative coordinates.
    ///
    /// `x` is relative to the text area's left edge. `y` is relative to the
    /// window's top edge, matching GNU Emacs `posn-at-x-y` conventions.
    pub fn point_at_coords(&self, x: i64, y: i64) -> Option<&DisplayPointSnapshot> {
        let row = self
            .rows
            .iter()
            .find(|row| y >= row.y && y < row.y.saturating_add(row.height.max(1)))?;
        let mut row_points = self.points.iter().filter(|point| point.row == row.row);
        let mut last = row_points.next()?;
        if x <= last.x {
            return Some(last);
        }
        for point in row_points {
            let right = last.x.saturating_add(last.width.max(1));
            if x < right {
                return Some(last);
            }
            if x < point.x {
                return Some(last);
            }
            last = point;
        }
        Some(last)
    }

    /// Row metrics for visual row ROW.
    pub fn row_metrics(&self, row: i64) -> Option<&DisplayRowSnapshot> {
        self.rows.iter().find(|metrics| metrics.row == row)
    }
}

impl Default for WindowDisplaySnapshot {
    fn default() -> Self {
        Self {
            window_id: WindowId(0),
            text_area_left_offset: 0,
            mode_line_height: 0,
            header_line_height: 0,
            tab_line_height: 0,
            points: Vec::new(),
            rows: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Frame
// ---------------------------------------------------------------------------

/// A frame (top-level window/screen).
pub struct Frame {
    pub id: FrameId,
    pub name: String,
    /// Root of the window tree.
    pub root_window: Window,
    /// The selected (active) window.
    pub selected_window: WindowId,
    /// Minibuffer window (always a leaf).
    pub minibuffer_window: Option<WindowId>,
    /// Storage for the minibuffer leaf, which is not part of the split tree.
    pub minibuffer_leaf: Option<Window>,
    /// Frame pixel dimensions.
    pub width: u32,
    pub height: u32,
    /// Internal window-system kind, mirroring GNU Emacs frame state rather
    /// than the mutable Lisp-visible frame parameter alist.
    pub window_system: Option<Value>,
    /// Frame parameters.
    pub parameters: HashMap<String, Value>,
    /// Whether the frame is visible.
    pub visible: bool,
    /// Frame title.
    pub title: String,
    /// Menu bar height in pixels.
    pub menu_bar_height: u32,
    /// Tool bar height in pixels.
    pub tool_bar_height: u32,
    /// Tab bar height in pixels.
    pub tab_bar_height: u32,
    /// Default font size in pixels.
    pub font_pixel_size: f32,
    /// Default character width.
    pub char_width: f32,
    /// Default character height.
    pub char_height: f32,
    /// Authoritative last-redisplay geometry keyed by live leaf window.
    pub display_snapshots: HashMap<WindowId, WindowDisplaySnapshot>,
    /// Per-frame realized Lisp faces, mirroring GNU's `frame->face_hash_table`
    /// ownership instead of exposing the global runtime face registry.
    pub realized_faces: HashMap<String, RuntimeFace>,
}

impl Frame {
    pub fn new(id: FrameId, name: String, width: u32, height: u32, root_window: Window) -> Self {
        let minibuffer_window = WindowId(MINIBUFFER_WINDOW_ID_BASE + id.0);
        let minibuffer_buffer_id = root_window.buffer_id().unwrap_or(BufferId(0));
        let mut minibuffer_leaf = Window::new_leaf(
            minibuffer_window,
            minibuffer_buffer_id,
            Rect::new(0.0, height as f32, width as f32, 16.0),
        );
        if let Window::Leaf {
            window_start,
            point,
            ..
        } = &mut minibuffer_leaf
        {
            *window_start = 1;
            *point = 1;
        }
        let selected = root_window
            .leaf_ids()
            .first()
            .copied()
            .unwrap_or(WindowId(0));
        Self {
            id,
            name,
            root_window,
            selected_window: selected,
            minibuffer_window: Some(minibuffer_window),
            minibuffer_leaf: Some(minibuffer_leaf),
            width,
            height,
            window_system: None,
            parameters: HashMap::new(),
            visible: true,
            title: String::new(),
            menu_bar_height: 0,
            tool_bar_height: 0,
            tab_bar_height: 0,
            font_pixel_size: 16.0,
            char_width: 8.0,
            char_height: 16.0,
            display_snapshots: HashMap::new(),
            realized_faces: HashMap::new(),
        }
    }

    /// Recalculate minibuffer bounds based on the root window's current bounds.
    ///
    /// Like GNU Emacs's `resize_frame_windows()` which sets:
    ///   `m->pixel_top = r->pixel_top + r->pixel_height`
    ///
    /// Must be called after any operation that changes the window tree
    /// (split, delete, resize).
    pub fn recalculate_minibuffer_bounds(&mut self) {
        self.sync_window_area_bounds();
    }

    /// Get the selected window.
    pub fn selected_window(&self) -> Option<&Window> {
        self.root_window.find(self.selected_window)
    }

    /// Get a mutable reference to the selected window.
    pub fn selected_window_mut(&mut self) -> Option<&mut Window> {
        self.root_window.find_mut(self.selected_window)
    }

    /// Replace all leaf window buffer bindings for `old_id` with `new_id`.
    pub fn replace_buffer_bindings(&mut self, old_id: BufferId, new_id: BufferId) {
        self.root_window.replace_buffer_id(old_id, new_id);
        if let Some(minibuffer_leaf) = self.minibuffer_leaf.as_mut() {
            minibuffer_leaf.replace_buffer_id(old_id, new_id);
        }
    }

    /// Return the effective window-system symbol for this frame.
    pub fn effective_window_system(&self) -> Option<Value> {
        self.window_system
            .or_else(|| self.parameters.get("window-system").copied())
    }

    /// Update the frame's internal window-system kind and keep the Lisp-visible
    /// frame parameter in sync.
    pub fn set_window_system(&mut self, window_system: Option<Value>) {
        self.window_system = window_system;
        match window_system {
            Some(value) => {
                self.parameters.insert("window-system".to_string(), value);
            }
            None => {
                self.parameters.remove("window-system");
            }
        }
    }

    pub fn frame_parameter_int(&self, key: &str) -> Option<i64> {
        self.parameters.get(key).and_then(Value::as_int)
    }

    pub fn realized_face(&self, name: &str) -> Option<&RuntimeFace> {
        self.realized_faces.get(name)
    }

    pub fn set_realized_face(&mut self, name: String, face: RuntimeFace) {
        self.realized_faces.insert(name, face);
    }

    pub fn clear_realized_faces(&mut self) {
        self.realized_faces.clear();
    }

    fn chrome_top_height(&self) -> f32 {
        self.menu_bar_height
            .saturating_add(self.tool_bar_height)
            .saturating_add(self.tab_bar_height) as f32
    }

    fn window_text_area_bounds(&self) -> Rect {
        let frame_w = self.width as f32;
        let frame_h = self.height as f32;
        let chrome_top = self.chrome_top_height().min(frame_h);
        let minibuffer_height = self
            .minibuffer_leaf
            .as_ref()
            .map(|mini| mini.bounds().height.max(0.0))
            .unwrap_or(0.0)
            .min((frame_h - chrome_top).max(0.0));
        let root_height = (frame_h - chrome_top - minibuffer_height).max(0.0);
        Rect::new(0.0, chrome_top, frame_w, root_height)
    }

    pub fn sync_window_area_bounds(&mut self) {
        let root_bounds = self.window_text_area_bounds();
        resize_window_subtree(&mut self.root_window, root_bounds);

        if let Some(mini) = self.minibuffer_leaf.as_mut() {
            let mini_h = mini
                .bounds()
                .height
                .max(0.0)
                .min((self.height as f32 - (root_bounds.y + root_bounds.height)).max(0.0));
            mini.set_bounds(Rect::new(
                root_bounds.x,
                root_bounds.y + root_bounds.height,
                root_bounds.width,
                mini_h,
            ));
            mini.invalidate_display_state();
        }

        self.root_window.invalidate_display_state();
        self.display_snapshots.clear();
    }

    pub fn sync_tab_bar_height_from_parameters(&mut self) {
        let lines = self
            .frame_parameter_int("tab-bar-lines")
            .unwrap_or(0)
            .max(0) as u32;
        let char_height = self.char_height.max(1.0).round() as u32;
        self.tab_bar_height = lines.saturating_mul(char_height);
        self.sync_window_area_bounds();
    }

    /// Select a window by ID.
    pub fn select_window(&mut self, id: WindowId) -> bool {
        if self.find_window(id).is_some() {
            self.selected_window = id;
            true
        } else {
            false
        }
    }

    /// Find a window by ID.
    pub fn find_window(&self, id: WindowId) -> Option<&Window> {
        if let Some(window) = self.root_window.find(id) {
            return Some(window);
        }
        self.minibuffer_leaf.as_ref().and_then(|window| {
            if window.id() == id {
                Some(window)
            } else {
                None
            }
        })
    }

    /// Find a mutable window by ID.
    pub fn find_window_mut(&mut self, id: WindowId) -> Option<&mut Window> {
        if let Some(window) = self.root_window.find_mut(id) {
            return Some(window);
        }
        self.minibuffer_leaf.as_mut().and_then(|window| {
            if window.id() == id {
                Some(window)
            } else {
                None
            }
        })
    }

    /// All leaf window IDs.
    pub fn window_list(&self) -> Vec<WindowId> {
        self.root_window.leaf_ids()
    }

    /// Number of visible windows (leaves).
    pub fn window_count(&self) -> usize {
        self.root_window.leaf_count()
    }

    /// Find which window is at pixel coordinates.
    pub fn window_at(&self, px: f32, py: f32) -> Option<WindowId> {
        self.root_window.window_at(px, py)
    }

    /// Columns (based on default char width).
    pub fn columns(&self) -> u32 {
        (self.width as f32 / self.char_width) as u32
    }

    /// Lines (based on default char height).
    pub fn lines(&self) -> u32 {
        (self.height as f32 / self.char_height) as u32
    }

    /// Replace the last-redisplay geometry for this frame's live windows.
    pub fn replace_display_snapshots(&mut self, snapshots: Vec<WindowDisplaySnapshot>) {
        self.display_snapshots.clear();
        for snapshot in snapshots {
            if self.find_window(snapshot.window_id).is_some() {
                self.display_snapshots.insert(snapshot.window_id, snapshot);
            }
        }
    }

    /// Last redisplay geometry for WINDOW-ID, if available.
    pub fn window_display_snapshot(&self, id: WindowId) -> Option<&WindowDisplaySnapshot> {
        self.display_snapshots.get(&id)
    }

    /// Resize the frame and window tree to new pixel dimensions.
    pub fn resize_pixelwise(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.sync_window_area_bounds();

        let char_width = self.char_width.max(1.0);
        let char_height = self.char_height.max(1.0);
        let root_height = self.root_window.bounds().height;
        let cols = ((width as f32) / char_width).floor().max(1.0) as i64;
        let text_lines = (root_height / char_height).floor().max(1.0) as i64;
        let total_lines = text_lines.saturating_add(1);
        self.parameters
            .insert("width".to_string(), Value::Int(cols));
        self.parameters
            .insert("height".to_string(), Value::Int(total_lines));
    }
}

// ---------------------------------------------------------------------------
// FrameManager
// ---------------------------------------------------------------------------

/// Manages all frames and tracks the selected frame.
pub struct FrameManager {
    frames: HashMap<FrameId, Frame>,
    selected: Option<FrameId>,
    next_frame_id: u64,
    next_window_id: u64,
    old_selected_window: Option<WindowId>,
    deleted_windows: HashSet<WindowId>,
    window_parameters: HashMap<WindowId, Vec<(Value, Value)>>,
    window_display_tables: HashMap<WindowId, Value>,
    window_cursor_types: HashMap<WindowId, Value>,
    window_prev_buffers: HashMap<WindowId, Value>,
    window_next_buffers: HashMap<WindowId, Value>,
    window_buffer_positions: HashMap<WindowId, HashMap<BufferId, (usize, usize)>>,
    window_use_times: HashMap<WindowId, i64>,
    window_select_count: i64,
}

impl FrameManager {
    pub fn new() -> Self {
        Self {
            frames: HashMap::new(),
            selected: None,
            next_frame_id: FRAME_ID_BASE,
            next_window_id: 1,
            old_selected_window: None,
            deleted_windows: HashSet::new(),
            window_parameters: HashMap::new(),
            window_display_tables: HashMap::new(),
            window_cursor_types: HashMap::new(),
            window_prev_buffers: HashMap::new(),
            window_next_buffers: HashMap::new(),
            window_buffer_positions: HashMap::new(),
            window_use_times: HashMap::new(),
            window_select_count: 0,
        }
    }

    /// Allocate a new window ID.
    pub fn next_window_id(&mut self) -> WindowId {
        let id = WindowId(self.next_window_id);
        self.next_window_id += 1;
        self.deleted_windows.remove(&id);
        id
    }

    /// Create a new frame with a single window displaying `buffer_id`.
    pub fn create_frame(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        buffer_id: BufferId,
    ) -> FrameId {
        let frame_id = FrameId(self.next_frame_id);
        self.next_frame_id += 1;

        let window_id = self.next_window_id();
        let bounds = Rect::new(0.0, 0.0, width as f32, height as f32);
        let root = Window::new_leaf(window_id, buffer_id, bounds);

        let frame = Frame::new(frame_id, name.to_string(), width, height, root);
        let selected_wid = frame.selected_window;
        self.frames.insert(frame_id, frame);
        self.note_window_selected(selected_wid);

        if self.selected.is_none() {
            self.selected = Some(frame_id);
            self.old_selected_window = Some(selected_wid);
        }

        frame_id
    }

    /// Get a frame by ID.
    pub fn get(&self, id: FrameId) -> Option<&Frame> {
        self.frames.get(&id)
    }

    /// Get a mutable frame by ID.
    pub fn get_mut(&mut self, id: FrameId) -> Option<&mut Frame> {
        self.frames.get_mut(&id)
    }

    /// Get the selected frame.
    pub fn selected_frame(&self) -> Option<&Frame> {
        self.selected.and_then(|id| self.frames.get(&id))
    }

    /// Get a mutable reference to the selected frame.
    pub fn selected_frame_mut(&mut self) -> Option<&mut Frame> {
        self.selected.and_then(|id| self.frames.get_mut(&id))
    }

    /// Select a frame.
    pub fn select_frame(&mut self, id: FrameId) -> bool {
        if self.frames.contains_key(&id) {
            self.selected = Some(id);
            true
        } else {
            false
        }
    }

    /// Delete a frame.
    pub fn delete_frame(&mut self, id: FrameId) -> bool {
        if let Some(frame) = self.frames.remove(&id) {
            let previous_selected = self.selected;
            for wid in frame.window_list() {
                self.deleted_windows.insert(wid);
                self.window_buffer_positions.remove(&wid);
                self.window_use_times.remove(&wid);
            }
            if let Some(minibuffer_wid) = frame.minibuffer_window {
                self.deleted_windows.insert(minibuffer_wid);
                self.window_buffer_positions.remove(&minibuffer_wid);
                self.window_use_times.remove(&minibuffer_wid);
            }
            if self.selected == Some(id) {
                self.selected = self.frames.keys().next().copied();
            }
            true
        } else {
            false
        }
    }

    /// List all frame IDs.
    pub fn frame_list(&self) -> Vec<FrameId> {
        self.frames.keys().copied().collect()
    }

    /// Split a window horizontally or vertically.
    /// Returns the new window's ID, or None if the window wasn't found.
    pub fn split_window(
        &mut self,
        frame_id: FrameId,
        window_id: WindowId,
        direction: SplitDirection,
        new_buffer_id: BufferId,
    ) -> Option<WindowId> {
        let internal_id = self.alloc_window_id();
        let new_id = self.alloc_window_id();
        let frame = self.frames.get_mut(&frame_id)?;

        split_window_in_tree(
            &mut frame.root_window,
            window_id,
            direction,
            internal_id,
            new_id,
            new_buffer_id,
        )?;

        frame.recalculate_minibuffer_bounds();
        Some(new_id)
    }

    /// Delete a window from a frame. Cannot delete the last window.
    pub fn delete_window(&mut self, frame_id: FrameId, window_id: WindowId) -> bool {
        let Some(frame) = self.frames.get_mut(&frame_id) else {
            return false;
        };
        if frame.root_window.leaf_count() <= 1 {
            return false; // Can't delete last window
        }

        let removed = delete_window_in_tree(&mut frame.root_window, window_id);
        if removed {
            self.deleted_windows.insert(window_id);
            self.window_buffer_positions.remove(&window_id);
            self.window_use_times.remove(&window_id);
            frame.recalculate_minibuffer_bounds();
        }

        if removed && frame.selected_window == window_id {
            // Select the first remaining leaf.
            if let Some(first) = frame.root_window.leaf_ids().first() {
                frame.selected_window = *first;
            }
        }

        removed
    }

    fn alloc_window_id(&mut self) -> WindowId {
        let id = WindowId(self.next_window_id);
        self.next_window_id += 1;
        self.deleted_windows.remove(&id);
        id
    }

    /// Replace dead-buffer bindings in every live frame.
    pub fn replace_buffer_in_windows(&mut self, old_id: BufferId, new_id: BufferId) {
        for frame in self.frames.values_mut() {
            frame.replace_buffer_bindings(old_id, new_id);
        }
    }

    /// Return the frame containing a live window ID, if any.
    pub fn find_window_frame_id(&self, window_id: WindowId) -> Option<FrameId> {
        self.frames.iter().find_map(|(frame_id, frame)| {
            if frame.minibuffer_window == Some(window_id) {
                return Some(*frame_id);
            }
            frame.find_window(window_id).and_then(|window| {
                if window.is_leaf() {
                    Some(*frame_id)
                } else {
                    None
                }
            })
        })
    }

    /// Return the frame containing a valid window ID, if any.
    ///
    /// Valid windows include live leaf windows, internal windows, and the
    /// minibuffer window of a live frame.
    pub fn find_valid_window_frame_id(&self, window_id: WindowId) -> Option<FrameId> {
        self.frames.iter().find_map(|(frame_id, frame)| {
            if frame.minibuffer_window == Some(window_id) {
                return Some(*frame_id);
            }
            frame.find_window(window_id).map(|_| *frame_id)
        })
    }

    /// Return true when WINDOW-ID designates a live window in any frame.
    pub fn is_live_window_id(&self, window_id: WindowId) -> bool {
        self.find_window_frame_id(window_id).is_some()
    }

    /// Return true when WINDOW-ID designates a valid live or internal window.
    pub fn is_valid_window_id(&self, window_id: WindowId) -> bool {
        self.find_valid_window_frame_id(window_id).is_some()
    }

    /// Return true when WINDOW-ID designates a live or stale window object.
    pub fn is_window_object_id(&self, window_id: WindowId) -> bool {
        self.is_valid_window_id(window_id) || self.deleted_windows.contains(&window_id)
    }

    /// Return window parameter KEY for WINDOW-ID, or nil when unset.
    pub fn window_parameter(&self, window_id: WindowId, key: &Value) -> Option<Value> {
        self.window_parameters.get(&window_id).and_then(|pairs| {
            pairs
                .iter()
                .find(|(k, _)| eq_value(k, key))
                .map(|(_, v)| *v)
        })
    }

    /// Set window parameter KEY on WINDOW-ID to VALUE.
    pub fn set_window_parameter(&mut self, window_id: WindowId, key: Value, value: Value) {
        let params = self.window_parameters.entry(window_id).or_default();
        if let Some((_, existing)) = params.iter_mut().find(|(k, _)| eq_value(k, &key)) {
            *existing = value;
        } else {
            params.push((key, value));
        }
    }

    /// Return window parameters alist for WINDOW-ID.
    pub fn window_parameters_alist(&self, window_id: WindowId) -> Value {
        let Some(params) = self.window_parameters.get(&window_id) else {
            return Value::Nil;
        };
        if params.is_empty() {
            return Value::Nil;
        }
        let alist = params
            .iter()
            .rev()
            .map(|(k, v)| Value::cons(*k, *v))
            .collect::<Vec<_>>();
        Value::list(alist)
    }

    /// Return window display table object for WINDOW-ID, or nil when unset.
    pub fn window_display_table(&self, window_id: WindowId) -> Value {
        self.window_display_tables
            .get(&window_id)
            .cloned()
            .unwrap_or(Value::Nil)
    }

    /// Set window display table object for WINDOW-ID.
    pub fn set_window_display_table(&mut self, window_id: WindowId, table: Value) {
        if table.is_nil() {
            self.window_display_tables.remove(&window_id);
        } else {
            self.window_display_tables.insert(window_id, table);
        }
    }

    /// Return window cursor-type object for WINDOW-ID.
    ///
    /// GNU Emacs defaults to `t` when no explicit per-window cursor-type is set.
    pub fn window_cursor_type(&self, window_id: WindowId) -> Value {
        self.window_cursor_types
            .get(&window_id)
            .cloned()
            .unwrap_or(Value::True)
    }

    /// Set window cursor-type object for WINDOW-ID.
    pub fn set_window_cursor_type(&mut self, window_id: WindowId, cursor_type: Value) {
        if cursor_type == Value::True {
            self.window_cursor_types.remove(&window_id);
        } else {
            self.window_cursor_types.insert(window_id, cursor_type);
        }
    }

    /// Return previous-buffer list object for WINDOW-ID, or nil when unset.
    pub fn window_prev_buffers(&self, window_id: WindowId) -> Value {
        self.window_prev_buffers
            .get(&window_id)
            .cloned()
            .unwrap_or(Value::Nil)
    }

    /// Set previous-buffer list object for WINDOW-ID.
    pub fn set_window_prev_buffers(&mut self, window_id: WindowId, prev_buffers: Value) {
        if prev_buffers.is_nil() {
            self.window_prev_buffers.remove(&window_id);
        } else {
            self.window_prev_buffers.insert(window_id, prev_buffers);
        }
    }

    /// Return next-buffer list object for WINDOW-ID, or nil when unset.
    pub fn window_next_buffers(&self, window_id: WindowId) -> Value {
        self.window_next_buffers
            .get(&window_id)
            .cloned()
            .unwrap_or(Value::Nil)
    }

    /// Set next-buffer list object for WINDOW-ID.
    pub fn set_window_next_buffers(&mut self, window_id: WindowId, next_buffers: Value) {
        if next_buffers.is_nil() {
            self.window_next_buffers.remove(&window_id);
        } else {
            self.window_next_buffers.insert(window_id, next_buffers);
        }
    }

    /// Return the use-time for WINDOW-ID.
    pub fn window_use_time(&self, window_id: WindowId) -> i64 {
        self.window_use_times.get(&window_id).copied().unwrap_or(0)
    }

    /// Mark WINDOW-ID as the most recently selected window.
    pub fn note_window_selected(&mut self, window_id: WindowId) -> i64 {
        self.window_select_count = self.window_select_count.saturating_add(1);
        self.window_use_times
            .insert(window_id, self.window_select_count);
        self.window_select_count
    }

    /// Mark WINDOW-ID as second-most recently used.
    ///
    /// Returns the new use-time of WINDOW-ID when the bump happened, nil-like
    /// behavior (`None`) otherwise.
    pub fn bump_window_use_time(
        &mut self,
        selected_window_id: WindowId,
        window_id: WindowId,
    ) -> Option<i64> {
        if window_id == selected_window_id {
            return None;
        }
        if self.window_use_time(selected_window_id) != self.window_select_count {
            return None;
        }

        let bumped_use_time = self.window_select_count;
        self.window_use_times.insert(window_id, bumped_use_time);
        self.window_select_count = self.window_select_count.saturating_add(1);
        self.window_use_times
            .insert(selected_window_id, self.window_select_count);
        Some(bumped_use_time)
    }

    /// Return the old selected window, when tracked.
    pub fn old_selected_window(&self) -> Option<WindowId> {
        self.old_selected_window
    }

    /// Return saved window state (window-start, point) for BUFFER-ID in WINDOW-ID.
    pub fn window_buffer_position(
        &self,
        window_id: WindowId,
        buffer_id: BufferId,
    ) -> Option<(usize, usize)> {
        self.window_buffer_positions
            .get(&window_id)
            .and_then(|by_buffer| by_buffer.get(&buffer_id).copied())
    }

    /// Save per-window state (window-start, point) for BUFFER-ID in WINDOW-ID.
    pub fn set_window_buffer_position(
        &mut self,
        window_id: WindowId,
        buffer_id: BufferId,
        window_start: usize,
        point: usize,
    ) {
        let by_buffer = self.window_buffer_positions.entry(window_id).or_default();
        by_buffer.insert(buffer_id, (window_start.max(1), point.max(1)));
    }
}

impl Default for FrameManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tree manipulation helpers
// ---------------------------------------------------------------------------

/// Split a window in the tree by wrapping it in an Internal node.
fn split_window_in_tree(
    tree: &mut Window,
    target: WindowId,
    direction: SplitDirection,
    internal_id: WindowId,
    new_id: WindowId,
    new_buffer_id: BufferId,
) -> Option<()> {
    if tree.id() == target {
        // Extract what we need before mutating.
        let old_id = tree.id();
        let old_bounds = *tree.bounds();
        let old_buffer = tree.buffer_id();

        if let Some(buf_id) = old_buffer {
            let (left_bounds, right_bounds) = match direction {
                SplitDirection::Horizontal => {
                    let half = old_bounds.width / 2.0;
                    (
                        Rect::new(old_bounds.x, old_bounds.y, half, old_bounds.height),
                        Rect::new(
                            old_bounds.x + half,
                            old_bounds.y,
                            old_bounds.width - half,
                            old_bounds.height,
                        ),
                    )
                }
                SplitDirection::Vertical => {
                    let half = old_bounds.height / 2.0;
                    (
                        Rect::new(old_bounds.x, old_bounds.y, old_bounds.width, half),
                        Rect::new(
                            old_bounds.x,
                            old_bounds.y + half,
                            old_bounds.width,
                            old_bounds.height - half,
                        ),
                    )
                }
            };

            let old_leaf = Window::new_leaf(old_id, buf_id, left_bounds);
            let new_leaf = Window::new_leaf(new_id, new_buffer_id, right_bounds);

            *tree = Window::Internal {
                id: internal_id,
                direction,
                children: vec![old_leaf, new_leaf],
                bounds: old_bounds,
                combination_limit: false,
            };

            return Some(());
        }
    }

    // Recurse into children.
    if let Window::Internal { children, .. } = tree {
        for child in children {
            if split_window_in_tree(child, target, direction, internal_id, new_id, new_buffer_id)
                .is_some()
            {
                return Some(());
            }
        }
    }

    None
}

/// Delete a window from the tree. Returns true if found and removed.
fn delete_window_in_tree(tree: &mut Window, target: WindowId) -> bool {
    if let Window::Internal {
        children, bounds, ..
    } = tree
    {
        // Check if any direct child is the target.
        if let Some(idx) = children.iter().position(|c| c.id() == target) {
            children.remove(idx);

            // If only one child remains, replace this internal node with it.
            if children.len() == 1 {
                let mut remaining = children.pop().unwrap();
                remaining.set_bounds(*bounds);
                *tree = remaining;
            } else {
                // Redistribute space among remaining children.
                redistribute_bounds(children, *bounds);
            }
            return true;
        }

        // Recurse.
        for child in children {
            if delete_window_in_tree(child, target) {
                return true;
            }
        }
    }

    false
}

fn find_parent_in_tree(node: &Window, target: WindowId) -> Option<WindowId> {
    let Window::Internal { children, .. } = node else {
        return None;
    };

    for child in children {
        if child.id() == target {
            return Some(node.id());
        }
        if let Some(parent) = find_parent_in_tree(child, target) {
            return Some(parent);
        }
    }

    None
}

fn find_sibling_in_tree(node: &Window, target: WindowId, next: bool) -> Option<WindowId> {
    let Window::Internal { children, .. } = node else {
        return None;
    };

    if let Some(index) = children.iter().position(|child| child.id() == target) {
        let sibling = if next {
            children.get(index + 1)
        } else {
            index.checked_sub(1).and_then(|idx| children.get(idx))
        };
        return sibling.map(Window::id);
    }

    children
        .iter()
        .find_map(|child| find_sibling_in_tree(child, target, next))
}

fn find_first_child_in_tree(
    node: &Window,
    target: WindowId,
    direction: SplitDirection,
) -> Option<WindowId> {
    match node {
        Window::Leaf { .. } => None,
        Window::Internal {
            id,
            direction: node_direction,
            children,
            ..
        } => {
            if *id == target {
                return (*node_direction == direction)
                    .then(|| children.first().map(Window::id))
                    .flatten();
            }
            children
                .iter()
                .find_map(|child| find_first_child_in_tree(child, target, direction))
        }
    }
}

/// Return the parent of WINDOW-ID inside FRAME, if any.
pub fn window_parent_id(frame: &Frame, window_id: WindowId) -> Option<WindowId> {
    if frame.minibuffer_window == Some(window_id) {
        return None;
    }
    find_parent_in_tree(&frame.root_window, window_id)
}

/// Return the first child of WINDOW-ID when it is combined in DIRECTION.
pub fn window_first_child_id(
    frame: &Frame,
    window_id: WindowId,
    direction: SplitDirection,
) -> Option<WindowId> {
    if frame.minibuffer_window == Some(window_id) {
        return None;
    }
    find_first_child_in_tree(&frame.root_window, window_id, direction)
}

/// Return the next sibling of WINDOW-ID, if any.
pub fn window_next_sibling_id(frame: &Frame, window_id: WindowId) -> Option<WindowId> {
    if frame.minibuffer_window == Some(window_id) {
        return None;
    }
    find_sibling_in_tree(&frame.root_window, window_id, true)
}

/// Return the previous sibling of WINDOW-ID, if any.
pub fn window_prev_sibling_id(frame: &Frame, window_id: WindowId) -> Option<WindowId> {
    if frame.minibuffer_window == Some(window_id) {
        return None;
    }
    find_sibling_in_tree(&frame.root_window, window_id, false)
}

/// Apply pixel-based resize values to a window tree.
///
/// Mirrors GNU Emacs `window_resize_apply()` in window.c:
/// - Reads `new_pixel` for each window from the provided map
/// - Sets window bounds accordingly
/// - Recursively processes children, tracking edge positions
/// - For vertical combinations: accumulates vertical edge
/// - For horizontal combinations: accumulates horizontal edge
///
/// `horflag`: true = applying horizontal sizes, false = applying vertical sizes.
pub fn window_resize_apply(
    window: &mut Window,
    horflag: bool,
    new_pixel_map: &HashMap<u64, i64>,
    new_normal_map: &HashMap<u64, f64>,
    char_width: f32,
    char_height: f32,
) {
    let wid = window.id().0;
    let new_px = new_pixel_map.get(&wid).copied();

    // Apply new_pixel to this window's bounds.
    let bounds = *window.bounds();
    if let Some(px) = new_px {
        let px = px.max(0) as f32;
        if horflag {
            window.set_bounds(Rect::new(bounds.x, bounds.y, px, bounds.height));
        } else {
            window.set_bounds(Rect::new(bounds.x, bounds.y, bounds.width, px));
        }
    }

    // Get updated bounds after applying new_pixel.
    let bounds = *window.bounds();
    let edge = if horflag { bounds.x } else { bounds.y };

    if let Window::Internal {
        direction,
        children,
        ..
    } = window
    {
        let mut edge = edge;
        let dir = *direction;
        for child in children.iter_mut() {
            // Position child at current edge.
            let cb = *child.bounds();
            if horflag {
                child.set_bounds(Rect::new(edge, cb.y, cb.width, cb.height));
            } else {
                child.set_bounds(Rect::new(cb.x, edge, cb.width, cb.height));
            }

            // Recurse.
            window_resize_apply(
                child,
                horflag,
                new_pixel_map,
                new_normal_map,
                char_width,
                char_height,
            );

            // Accumulate edge in the combination direction.
            let child_bounds = *child.bounds();
            match (dir, horflag) {
                (SplitDirection::Horizontal, true) => edge += child_bounds.width,
                (SplitDirection::Vertical, false) => edge += child_bounds.height,
                _ => {}
            }
        }
    }
}

/// Check that a resize is valid: the sum of children's new_pixel values
/// must equal the parent's new_pixel value in the combination direction.
pub fn window_resize_check(
    window: &Window,
    horflag: bool,
    new_pixel_map: &HashMap<u64, i64>,
) -> bool {
    let wid = window.id().0;
    let my_new = new_pixel_map.get(&wid).copied().unwrap_or_else(|| {
        let b = window.bounds();
        if horflag {
            b.width as i64
        } else {
            b.height as i64
        }
    });

    match window {
        Window::Leaf { .. } => true,
        Window::Internal {
            direction,
            children,
            ..
        } => {
            // In the combination direction, sum of children must equal parent.
            let combines = (*direction == SplitDirection::Horizontal) == horflag;
            if combines {
                let child_sum: i64 = children
                    .iter()
                    .map(|c| {
                        let cid = c.id().0;
                        new_pixel_map.get(&cid).copied().unwrap_or_else(|| {
                            let b = c.bounds();
                            if horflag {
                                b.width as i64
                            } else {
                                b.height as i64
                            }
                        })
                    })
                    .sum();
                if child_sum != my_new {
                    return false;
                }
            }
            // All children must also pass the check.
            children
                .iter()
                .all(|c| window_resize_check(c, horflag, new_pixel_map))
        }
    }
}

/// Apply character-cell-based resize values to a window tree.
///
/// Mirrors GNU Emacs `window_resize_apply_total()` in window.c:
/// - Reads `new_total` for each window from the provided map
/// - Sets character-cell sizes and positions accordingly
/// - This does NOT modify pixel bounds — it only updates the character-cell
///   grid positions used by Emacs internals.
///
/// Since neomacs uses pixel bounds as the source of truth, this function
/// converts new_total back to pixels using char_width/char_height and
/// applies the result to window bounds.
pub fn window_resize_apply_total(
    window: &mut Window,
    horflag: bool,
    new_total_map: &HashMap<u64, i64>,
    char_width: f32,
    char_height: f32,
) {
    let wid = window.id().0;
    let new_total = new_total_map.get(&wid).copied();

    // Apply new_total converted to pixels.
    let bounds = *window.bounds();
    if let Some(total) = new_total {
        let total = total.max(0) as f32;
        if horflag {
            let px = total * char_width;
            window.set_bounds(Rect::new(bounds.x, bounds.y, px, bounds.height));
        } else {
            let px = total * char_height;
            window.set_bounds(Rect::new(bounds.x, bounds.y, bounds.width, px));
        }
    }

    let bounds = *window.bounds();
    let edge = if horflag { bounds.x } else { bounds.y };

    if let Window::Internal {
        direction,
        children,
        ..
    } = window
    {
        let mut edge = edge;
        let dir = *direction;
        for child in children.iter_mut() {
            // Position child at current edge.
            let cb = *child.bounds();
            if horflag {
                child.set_bounds(Rect::new(edge, cb.y, cb.width, cb.height));
            } else {
                child.set_bounds(Rect::new(cb.x, edge, cb.width, cb.height));
            }

            // Recurse.
            window_resize_apply_total(child, horflag, new_total_map, char_width, char_height);

            // Accumulate edge.
            let child_bounds = *child.bounds();
            match (dir, horflag) {
                (SplitDirection::Horizontal, true) => edge += child_bounds.width,
                (SplitDirection::Vertical, false) => edge += child_bounds.height,
                _ => {}
            }
        }
    }
}

/// Redistribute bounds equally among children.
fn redistribute_bounds(children: &mut [Window], parent: Rect) {
    if children.is_empty() {
        return;
    }

    let n = children.len() as f32;

    // Detect direction from first two children if possible.
    if children.len() >= 2 {
        let first = children[0].bounds();
        let second = children[1].bounds();

        if (first.x - second.x).abs() > 0.1 {
            // Horizontal split
            let w = parent.width / n;
            for (i, child) in children.iter_mut().enumerate() {
                child.set_bounds(Rect::new(
                    parent.x + i as f32 * w,
                    parent.y,
                    w,
                    parent.height,
                ));
            }
        } else {
            // Vertical split
            let h = parent.height / n;
            for (i, child) in children.iter_mut().enumerate() {
                child.set_bounds(Rect::new(
                    parent.x,
                    parent.y + i as f32 * h,
                    parent.width,
                    h,
                ));
            }
        }
    } else {
        // Single child gets full bounds.
        children[0].set_bounds(parent);
    }
}

fn resize_window_subtree(window: &mut Window, bounds: Rect) {
    window.set_bounds(bounds);
    if let Window::Internal { children, .. } = window {
        redistribute_bounds(children, bounds);
        for child in children {
            let child_bounds = *child.bounds();
            resize_window_subtree(child, child_bounds);
        }
    }
}

// ===========================================================================
// GcTrace
// ===========================================================================

impl GcTrace for FrameManager {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        // Window-level parameter maps
        for params in self.window_parameters.values() {
            for (k, v) in params {
                roots.push(*k);
                roots.push(*v);
            }
        }
        for v in self.window_display_tables.values() {
            roots.push(*v);
        }
        for v in self.window_cursor_types.values() {
            roots.push(*v);
        }
        for v in self.window_prev_buffers.values() {
            roots.push(*v);
        }
        for v in self.window_next_buffers.values() {
            roots.push(*v);
        }
        // Frame and window tree parameters
        for frame in self.frames.values() {
            for v in frame.parameters.values() {
                roots.push(*v);
            }
            trace_window(&frame.root_window, roots);
            if let Some(mb) = &frame.minibuffer_leaf {
                trace_window(mb, roots);
            }
        }
    }
}

fn trace_window(window: &Window, roots: &mut Vec<Value>) {
    match window {
        Window::Leaf { parameters, .. } => {
            for v in parameters.values() {
                roots.push(*v);
            }
        }
        Window::Internal { children, .. } => {
            for child in children {
                trace_window(child, roots);
            }
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_frame_and_window() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let frame = mgr.get(fid).unwrap();

        assert_eq!(frame.window_count(), 1);
        assert!(frame.selected_window().is_some());
        assert!(frame.selected_window().unwrap().is_leaf());
    }

    #[test]
    fn split_window_horizontal() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        let new_wid = mgr.split_window(fid, wid, SplitDirection::Horizontal, BufferId(2));
        assert!(new_wid.is_some());

        let frame = mgr.get(fid).unwrap();
        assert_eq!(frame.window_count(), 2);
    }

    #[test]
    fn split_window_vertical() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        let new_wid = mgr.split_window(fid, wid, SplitDirection::Vertical, BufferId(2));
        assert!(new_wid.is_some());

        let frame = mgr.get(fid).unwrap();
        assert_eq!(frame.window_count(), 2);
    }

    #[test]
    fn delete_window() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        // Split first.
        let new_wid = mgr
            .split_window(fid, wid, SplitDirection::Horizontal, BufferId(2))
            .unwrap();

        // Delete the new window.
        assert!(mgr.delete_window(fid, new_wid));
        assert_eq!(mgr.get(fid).unwrap().window_count(), 1);
    }

    #[test]
    fn cannot_delete_last_window() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        assert!(!mgr.delete_window(fid, wid));
    }

    #[test]
    fn select_window() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        let new_wid = mgr
            .split_window(fid, wid, SplitDirection::Horizontal, BufferId(2))
            .unwrap();

        assert!(mgr.get_mut(fid).unwrap().select_window(new_wid));
        assert_eq!(mgr.get(fid).unwrap().selected_window.0, new_wid.0,);
    }

    #[test]
    fn window_at_coordinates() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        mgr.split_window(fid, wid, SplitDirection::Horizontal, BufferId(2));

        let frame = mgr.get(fid).unwrap();
        // Left half
        let left = frame.window_at(100.0, 300.0);
        assert!(left.is_some());
        // Right half
        let right = frame.window_at(600.0, 300.0);
        assert!(right.is_some());
        // Should be different windows
        assert_ne!(left, right);
    }

    #[test]
    fn frame_columns_and_lines() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let frame = mgr.get(fid).unwrap();

        assert_eq!(frame.columns(), 100); // 800/8
        assert_eq!(frame.lines(), 37); // 600/16 = 37
    }

    #[test]
    fn delete_frame() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        assert!(mgr.delete_frame(fid));
        assert!(mgr.get(fid).is_none());
    }

    #[test]
    fn multiple_frames() {
        let mut mgr = FrameManager::new();
        let f1 = mgr.create_frame("F1", 800, 600, BufferId(1));
        let f2 = mgr.create_frame("F2", 1024, 768, BufferId(2));

        assert_eq!(mgr.frame_list().len(), 2);
        assert!(mgr.select_frame(f2));
        assert_eq!(mgr.selected_frame().unwrap().id, f2);

        mgr.delete_frame(f1);
        assert_eq!(mgr.frame_list().len(), 1);
    }

    #[test]
    fn rect_contains() {
        let r = Rect::new(10.0, 20.0, 100.0, 50.0);
        assert!(r.contains(10.0, 20.0));
        assert!(r.contains(50.0, 40.0));
        assert!(!r.contains(9.0, 20.0));
        assert!(!r.contains(110.0, 70.0));
    }

    #[test]
    fn find_window_frame_id() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        assert_eq!(mgr.find_window_frame_id(wid), Some(fid));
        assert_eq!(mgr.find_window_frame_id(WindowId(99999)), None);
    }

    #[test]
    fn is_live_window_id() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        assert!(mgr.is_live_window_id(wid));
        assert!(!mgr.is_live_window_id(WindowId(99999)));
    }

    #[test]
    fn window_parameters() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        let key = Value::symbol("my-param");
        let val = Value::Int(42);

        // Initially no parameter
        assert!(mgr.window_parameter(wid, &key).is_none());

        mgr.set_window_parameter(wid, key, val);
        assert_eq!(mgr.window_parameter(wid, &key), Some(Value::Int(42)));
    }

    #[test]
    fn replace_buffer_in_windows() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        // Window should show buffer 1
        let frame = mgr.get(fid).unwrap();
        assert_eq!(
            frame.find_window(wid).unwrap().buffer_id(),
            Some(BufferId(1))
        );

        // Replace buffer 1 with buffer 2
        mgr.replace_buffer_in_windows(BufferId(1), BufferId(2));

        let frame = mgr.get(fid).unwrap();
        assert_eq!(
            frame.find_window(wid).unwrap().buffer_id(),
            Some(BufferId(2))
        );
    }

    #[test]
    fn deep_split_and_delete() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let w1 = mgr.get(fid).unwrap().window_list()[0];

        // Split w1 horizontally → w2
        let w2 = mgr
            .split_window(fid, w1, SplitDirection::Horizontal, BufferId(2))
            .unwrap();

        // Split w2 vertically → w3
        let w3 = mgr
            .split_window(fid, w2, SplitDirection::Vertical, BufferId(3))
            .unwrap();

        assert_eq!(mgr.get(fid).unwrap().window_count(), 3);

        // Delete w3
        assert!(mgr.delete_window(fid, w3));
        assert_eq!(mgr.get(fid).unwrap().window_count(), 2);

        // Delete w2
        assert!(mgr.delete_window(fid, w2));
        assert_eq!(mgr.get(fid).unwrap().window_count(), 1);

        // w1 is the last one, can't delete
        assert!(!mgr.delete_window(fid, w1));
    }

    #[test]
    fn note_window_selected_updates_use_time() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let w1 = mgr.get(fid).unwrap().window_list()[0];
        let w2 = mgr
            .split_window(fid, w1, SplitDirection::Horizontal, BufferId(2))
            .unwrap();

        let t1 = mgr.note_window_selected(w1);
        let t2 = mgr.note_window_selected(w2);
        // Each selection should get a monotonically increasing use-time
        assert!(t2 > t1);
    }

    #[test]
    fn window_set_buffer_resets_position() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        // Modify point
        let frame = mgr.get_mut(fid).unwrap();
        if let Some(w) = frame.find_window_mut(wid) {
            if let Window::Leaf { point, .. } = w {
                *point = 100;
            }
        }

        // Set buffer resets point to 1
        let frame = mgr.get_mut(fid).unwrap();
        if let Some(w) = frame.find_window_mut(wid) {
            w.set_buffer(BufferId(2));
        }

        let frame = mgr.get(fid).unwrap();
        let w = frame.find_window(wid).unwrap();
        if let Window::Leaf {
            point, buffer_id, ..
        } = w
        {
            assert_eq!(*buffer_id, BufferId(2));
            assert_eq!(*point, 1);
        }
    }

    #[test]
    fn frame_resize_pixelwise_updates_window_tree_and_invalidates_display_state() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let w1 = mgr.get(fid).unwrap().window_list()[0];
        let w2 = mgr
            .split_window(fid, w1, SplitDirection::Horizontal, BufferId(2))
            .unwrap();

        let frame = mgr.get_mut(fid).unwrap();
        frame.char_width = 10.0;
        frame.char_height = 20.0;
        frame.replace_display_snapshots(vec![WindowDisplaySnapshot {
            window_id: w1,
            ..WindowDisplaySnapshot::default()
        }]);

        frame
            .find_window_mut(w1)
            .unwrap()
            .set_window_end_from_positions(200, 200, 50, 50, 3);
        frame
            .find_window_mut(w2)
            .unwrap()
            .set_window_end_from_positions(200, 200, 60, 60, 3);

        frame.resize_pixelwise(400, 260);

        assert_eq!(frame.width, 400);
        assert_eq!(frame.height, 260);
        assert!(frame.display_snapshots.is_empty());
        assert_eq!(frame.parameters.get("width"), Some(&Value::Int(40)));
        assert_eq!(frame.parameters.get("height"), Some(&Value::Int(13)));

        let root_bounds = *frame.root_window.bounds();
        assert_eq!(root_bounds, Rect::new(0.0, 0.0, 400.0, 244.0));

        let mini_bounds = *frame.minibuffer_leaf.as_ref().unwrap().bounds();
        assert_eq!(mini_bounds, Rect::new(0.0, 244.0, 400.0, 16.0));

        assert_eq!(
            frame.find_window(w1).unwrap().bounds(),
            &Rect::new(0.0, 0.0, 200.0, 244.0)
        );
        assert_eq!(
            frame.find_window(w2).unwrap().bounds(),
            &Rect::new(200.0, 0.0, 200.0, 244.0)
        );
        assert_eq!(
            frame.find_window(w1).unwrap().window_end_valid(),
            Some(false)
        );
        assert_eq!(
            frame.find_window(w2).unwrap().window_end_valid(),
            Some(false)
        );
        assert_eq!(
            frame.minibuffer_leaf.as_ref().unwrap().window_end_valid(),
            Some(false)
        );
    }

    #[test]
    fn frame_resize_pixelwise_reserves_tab_bar_height_above_root_window_tree() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let frame = mgr.get_mut(fid).unwrap();
        frame.char_width = 10.0;
        frame.char_height = 20.0;
        frame
            .parameters
            .insert("tab-bar-lines".to_string(), Value::Int(1));

        frame.sync_tab_bar_height_from_parameters();
        frame.resize_pixelwise(400, 260);

        assert_eq!(frame.tab_bar_height, 20);
        assert_eq!(
            *frame.root_window.bounds(),
            Rect::new(0.0, 20.0, 400.0, 224.0)
        );
        assert_eq!(
            *frame.minibuffer_leaf.as_ref().unwrap().bounds(),
            Rect::new(0.0, 244.0, 400.0, 16.0)
        );
        assert_eq!(frame.parameters.get("height"), Some(&Value::Int(12)));
    }
}
