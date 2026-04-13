//! Window and frame management for the editor.
//!
//! Implements the Emacs window tree model:
//! - A **frame** contains a root window (which may be split).
//! - A **window** is either a *leaf* (displays a buffer) or an *internal*
//!   node with children (horizontal or vertical split).
//! - The **selected window** is the one receiving input.
//! - The **minibuffer window** is a special single-line window at the bottom.

use crate::buffer::BufferId;
use crate::emacs_core::value::{HashTableTest, Value};
use crate::face::Face as RuntimeFace;
use crate::gc_trace::GcTrace;
use std::collections::{HashMap, HashSet};

mod display;
mod history;
mod parameters;

pub use display::WindowBufferDisplayDefaults;

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
// Window display state
// ---------------------------------------------------------------------------

/// Per-window display settings that GNU Emacs stores on `struct window`.
///
/// # Cursor audit follow-through
///
/// neomacs now stores GNU-like cursor state directly on the live window:
///
/// - `cursor`: intended cursor position in the latest redisplay result
/// - `output_cursor`: the nominal output position last committed by redisplay
/// - `phys_cursor`: the last physical cursor geometry emitted on screen
///
/// This mirrors GNU's `struct window` ownership model closely enough for
/// `window-cursor-info` and related stateful cursor queries. The Rust
/// redisplay path now drives this state through an explicit per-window output
/// pass before frame snapshots are published. Text rows and window chrome rows
/// now advance `output_cursor` as they are emitted. The main remaining gap is
/// that neomacs still advances output at Rust row-emission granularity rather
/// than GNU's lower-level draw-call pipeline.
#[derive(Clone, Debug)]
pub struct WindowDisplayState {
    /// Window-local display table; nil means inherit from the buffer/frame.
    pub display_table: Value,
    /// Window-local cursor type; t means use the buffer-local value.
    pub cursor_type: Value,
    /// Intended cursor position in the latest redisplay result.
    pub cursor: Option<WindowCursorPos>,
    /// Last physical cursor geometry produced by redisplay for this window.
    pub phys_cursor: Option<WindowCursorSnapshot>,
    /// Last nominal output position actually committed by redisplay.
    pub output_cursor: Option<WindowCursorPos>,
    /// Last physical cursor type emitted by redisplay.
    pub phys_cursor_type: WindowCursorKind,
    /// Whether the window currently owns a live physical cursor.
    pub phys_cursor_on_p: bool,
    /// Whether the cursor is hidden without invalidating the geometry.
    pub cursor_off_p: bool,
    /// Cursor visibility state committed by the last completed redisplay.
    pub last_cursor_off_p: bool,
    /// Last visual row where redisplay placed the cursor.
    pub last_cursor_vpos: i64,
    /// Raw fringe widths; `-1` means use the frame default.
    pub left_fringe_width: i32,
    pub right_fringe_width: i32,
    pub fringes_outside_margins: bool,
    pub fringes_persistent: bool,
    /// Raw scroll bar sizes; `-1` means use the frame default.
    pub scroll_bar_width: i32,
    pub vertical_scroll_bar_type: Value,
    pub scroll_bar_height: i32,
    pub horizontal_scroll_bar_type: Value,
    pub scroll_bars_persistent: bool,
}

impl Default for WindowDisplayState {
    fn default() -> Self {
        Self {
            display_table: Value::NIL,
            cursor_type: Value::T,
            cursor: None,
            phys_cursor: None,
            output_cursor: None,
            phys_cursor_type: WindowCursorKind::NoCursor,
            phys_cursor_on_p: false,
            cursor_off_p: false,
            last_cursor_off_p: false,
            last_cursor_vpos: 0,
            left_fringe_width: -1,
            right_fringe_width: -1,
            fringes_outside_margins: false,
            fringes_persistent: false,
            scroll_bar_width: -1,
            vertical_scroll_bar_type: Value::T,
            scroll_bar_height: -1,
            horizontal_scroll_bar_type: Value::T,
            scroll_bars_persistent: false,
        }
    }
}

impl WindowDisplayState {
    pub fn clear_cursor_state(&mut self) {
        self.cursor = None;
        self.clear_output_cursor_state();
        self.clear_physical_cursor_state();
    }

    /// Start a new output pass for this window.
    ///
    /// The last committed output cursor remains authoritative until redisplay
    /// emits a new cursor position for this window.
    pub fn begin_output_pass(&mut self) {
        self.cursor = None;
        self.clear_physical_cursor_state();
    }

    /// Start a new output update for a window that will actively emit rows in
    /// the current redisplay pass.
    pub fn begin_window_output_update(&mut self) {
        self.begin_output_pass();
        self.clear_output_cursor_state();
    }

    pub fn clear_output_cursor_state(&mut self) {
        self.output_cursor = None;
    }

    pub fn clear_physical_cursor_state(&mut self) {
        self.phys_cursor = None;
        self.phys_cursor_type = WindowCursorKind::NoCursor;
        self.phys_cursor_on_p = false;
    }

    pub fn install_logical_cursor(&mut self, cursor: Option<WindowCursorPos>) {
        self.cursor = cursor;
    }

    pub fn commit_output_cursor_from_cursor(&mut self) {
        self.output_cursor = self.cursor;
    }

    pub fn replay_output_rows(&mut self, rows: &[DisplayRowSnapshot]) {
        if rows.is_empty() {
            self.clear_output_cursor_state();
            return;
        }
        for row in rows {
            self.output_cursor_to(row.row, row.end_col, row.y, row.end_x);
        }
    }

    pub fn commit_output_cursor_from_display_snapshot(&mut self, snapshot: &WindowDisplaySnapshot) {
        self.replay_output_rows(&snapshot.rows);
    }

    pub fn commit_output_row(&mut self, row: &DisplayRowSnapshot) {
        self.output_cursor_to(row.row, row.end_col, row.y, row.end_x);
    }

    pub fn output_cursor_to(&mut self, row: i64, col: i64, y: i64, x: i64) {
        self.output_cursor = Some(WindowCursorPos { x, y, row, col });
    }

    pub fn apply_physical_cursor_snapshot(&mut self, cursor: Option<WindowCursorSnapshot>) {
        self.phys_cursor = cursor.clone();
        self.phys_cursor_type = cursor
            .as_ref()
            .map(|c| c.kind)
            .unwrap_or(WindowCursorKind::NoCursor);
        self.phys_cursor_on_p = cursor.is_some();
    }

    pub fn commit_completed_redisplay(&mut self) {
        self.last_cursor_off_p = self.cursor_off_p;
        if let Some(cursor) = self.phys_cursor.as_ref() {
            self.last_cursor_vpos = cursor.row;
        } else if let Some(cursor) = self.cursor.as_ref() {
            self.last_cursor_vpos = cursor.row;
        }
    }
}

/// Live-window history state that GNU Emacs stores directly on `struct window`.
#[derive(Clone, Debug)]
pub struct WindowHistoryState {
    pub prev_buffers: Value,
    pub next_buffers: Value,
    pub use_time: i64,
}

impl Default for WindowHistoryState {
    fn default() -> Self {
        Self {
            prev_buffers: Value::NIL,
            next_buffers: Value::NIL,
            use_time: 0,
        }
    }
}

pub(crate) type WindowParameters = Vec<(Value, Value)>;

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
        ///
        /// GNU stores this as a marker (`w->start`) so that
        /// buffer edits before the start position auto-shift it.
        /// neomacs uses a `usize` byte offset and patches it
        /// manually. Window audit Critical 9 in
        /// `drafts/window-system-audit.md` — see the matching
        /// note on `point` below.
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
        ///
        /// GNU stores this as a marker (`w->pointm`, a
        /// `Lisp_Marker`) so that buffer insertions before the
        /// position auto-shift it. neomacs uses a `usize` byte
        /// offset and patches it manually from the buffer edit
        /// hooks. Window audit Critical 9 in
        /// `drafts/window-system-audit.md`: any path that misses
        /// the manual patching can leave a stale point that GNU
        /// would have updated automatically. Converting to a
        /// real marker is a multi-day cross-cutting change that
        /// touches every read site, every edit hook, and the
        /// pdump round-trip.
        point: usize,
        /// Previous point value mirrored from GNU `w->old_pointm`.
        old_point: usize,
        /// Whether this is a dedicated window.
        dedicated: bool,
        /// Lisp-visible per-window parameter alist, newest entries first.
        parameters: WindowParameters,
        /// Live-window history state mirrored from GNU `struct window`.
        history: WindowHistoryState,
        /// Desired height in lines (for fixed windows, 0 = flexible).
        fixed_height: usize,
        /// Desired width in columns (for fixed windows, 0 = flexible).
        fixed_width: usize,
        /// Horizontal scroll offset (columns).
        hscroll: usize,
        /// Raw GNU `w->vscroll` value in pixels: zero or negative.
        ///
        /// Lisp-visible `window-vscroll` reports `-vscroll`, either in pixels
        /// or in canonical line units depending on the call site.
        vscroll: i32,
        /// Mirrors GNU `w->preserve_vscroll_p`.
        preserve_vscroll_p: bool,
        /// Window margins (left, right) in columns.
        margins: (usize, usize),
        /// Window-local display settings mirrored from GNU `struct window`.
        display: WindowDisplayState,
        /// Pending pixel size queued by `set-window-new-pixel`. GNU
        /// stores this as `w->new_pixel`
        /// (`src/window.h:283`). Cleared by `window-resize-apply`
        /// once committed. Window audit Structural 1 in
        /// `drafts/window-system-audit.md` moved this off a
        /// thread-local HashMap onto the window struct so
        /// window-configuration save/restore round-trips it
        /// automatically.
        new_pixel: Option<i64>,
        /// Pending total (line-cell) size queued by
        /// `set-window-new-total`. GNU `w->new_total`
        /// (`src/window.h:284`).
        new_total: Option<i64>,
        /// Pending normal-size fraction queued by
        /// `set-window-new-normal`. GNU `w->new_normal`
        /// (`src/window.h:285`). Stored as a `Value` to mirror
        /// GNU's Lisp_Object slot — `Value::NIL` means "unset".
        new_normal: Value,
        /// Authoritative proportional vertical size
        /// (height fraction of parent). GNU `w->normal_lines`
        /// (`src/window.h:128`). Initialized to 1.0 on the root
        /// and updated by `window-resize-apply` from
        /// `new_normal`. `(window-normal-size w nil)` returns
        /// this value. Window audit Critical 7 in
        /// `drafts/window-system-audit.md`.
        normal_lines: Value,
        /// Authoritative proportional horizontal size
        /// (width fraction of parent). GNU `w->normal_cols`
        /// (`src/window.h:129`).
        normal_cols: Value,
    },

    /// Internal node: contains children split in a direction.
    Internal {
        id: WindowId,
        direction: SplitDirection,
        children: Vec<Window>,
        bounds: Rect,
        /// Lisp-visible per-window parameter alist, newest entries first.
        parameters: WindowParameters,
        /// Combination limit — prevents recombination when non-nil.
        /// Mirrors GNU Emacs `w->combination_limit`.
        combination_limit: bool,
        /// Pending pixel size — see `Leaf::new_pixel`. GNU keeps
        /// the same `new_pixel` slot on every `struct window`,
        /// regardless of leaf/internal split state.
        new_pixel: Option<i64>,
        /// Pending total size — see `Leaf::new_total`.
        new_total: Option<i64>,
        /// Pending normal-size fraction — see `Leaf::new_normal`.
        new_normal: Value,
        /// Persistent normal-size fraction — see
        /// `Leaf::normal_lines`.
        normal_lines: Value,
        /// Persistent normal-size fraction — see
        /// `Leaf::normal_cols`.
        normal_cols: Value,
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
            old_point: 1,
            dedicated: false,
            parameters: Vec::new(),
            history: WindowHistoryState::default(),
            fixed_height: 0,
            fixed_width: 0,
            hscroll: 0,
            vscroll: 0,
            preserve_vscroll_p: false,
            margins: (0, 0),
            display: WindowDisplayState::default(),
            new_pixel: None,
            new_total: None,
            new_normal: Value::NIL,
            // GNU `make_window` initializes `normal_lines` and
            // `normal_cols` to 1.0 (`src/window.c:4603-4604`).
            normal_lines: Value::make_float(1.0),
            normal_cols: Value::make_float(1.0),
        }
    }

    /// Read the pending `new_pixel` slot. GNU `w->new_pixel`.
    pub fn new_pixel(&self) -> Option<i64> {
        match self {
            Window::Leaf { new_pixel, .. } | Window::Internal { new_pixel, .. } => *new_pixel,
        }
    }

    /// Write the pending `new_pixel` slot. GNU `wset_new_pixel`.
    pub fn set_new_pixel(&mut self, value: Option<i64>) {
        match self {
            Window::Leaf { new_pixel, .. } | Window::Internal { new_pixel, .. } => {
                *new_pixel = value;
            }
        }
    }

    /// Read the pending `new_total` slot. GNU `w->new_total`.
    pub fn new_total(&self) -> Option<i64> {
        match self {
            Window::Leaf { new_total, .. } | Window::Internal { new_total, .. } => *new_total,
        }
    }

    /// Write the pending `new_total` slot. GNU `wset_new_total`.
    pub fn set_new_total(&mut self, value: Option<i64>) {
        match self {
            Window::Leaf { new_total, .. } | Window::Internal { new_total, .. } => {
                *new_total = value;
            }
        }
    }

    /// Read the pending `new_normal` Lisp slot. GNU `w->new_normal`.
    pub fn new_normal(&self) -> Value {
        match self {
            Window::Leaf { new_normal, .. } | Window::Internal { new_normal, .. } => *new_normal,
        }
    }

    /// Write the pending `new_normal` Lisp slot.
    pub fn set_new_normal(&mut self, value: Value) {
        match self {
            Window::Leaf { new_normal, .. } | Window::Internal { new_normal, .. } => {
                *new_normal = value;
            }
        }
    }

    /// Read the persistent `normal_lines` Lisp slot. GNU
    /// `w->normal_lines`.
    pub fn normal_lines(&self) -> Value {
        match self {
            Window::Leaf { normal_lines, .. } | Window::Internal { normal_lines, .. } => {
                *normal_lines
            }
        }
    }

    /// Write the persistent `normal_lines` Lisp slot. GNU
    /// `wset_normal_lines`.
    pub fn set_normal_lines(&mut self, value: Value) {
        match self {
            Window::Leaf { normal_lines, .. } | Window::Internal { normal_lines, .. } => {
                *normal_lines = value;
            }
        }
    }

    /// Read the persistent `normal_cols` Lisp slot. GNU
    /// `w->normal_cols`.
    pub fn normal_cols(&self) -> Value {
        match self {
            Window::Leaf { normal_cols, .. } | Window::Internal { normal_cols, .. } => *normal_cols,
        }
    }

    /// Write the persistent `normal_cols` Lisp slot. GNU
    /// `wset_normal_cols`.
    pub fn set_normal_cols(&mut self, value: Value) {
        match self {
            Window::Leaf { normal_cols, .. } | Window::Internal { normal_cols, .. } => {
                *normal_cols = value;
            }
        }
    }

    /// Set the window's point from a buffer position.
    /// GNU Emacs xdisp.c:20616 syncs w->pointm from buffer PT before redisplay.
    pub fn set_point(&mut self, pos: usize) {
        if let Window::Leaf { point, .. } = self {
            *point = pos;
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

    /// Return this leaf window's display state.
    pub fn display(&self) -> Option<&WindowDisplayState> {
        match self {
            Window::Leaf { display, .. } => Some(display),
            Window::Internal { .. } => None,
        }
    }

    /// Return a mutable reference to this leaf window's display state.
    pub fn display_mut(&mut self) -> Option<&mut WindowDisplayState> {
        match self {
            Window::Leaf { display, .. } => Some(display),
            Window::Internal { .. } => None,
        }
    }

    /// Return this window's Lisp-visible parameter alist.
    pub fn parameters(&self) -> &WindowParameters {
        match self {
            Window::Leaf { parameters, .. } | Window::Internal { parameters, .. } => parameters,
        }
    }

    /// Return a mutable reference to this window's Lisp-visible parameter alist.
    pub fn parameters_mut(&mut self) -> &mut WindowParameters {
        match self {
            Window::Leaf { parameters, .. } | Window::Internal { parameters, .. } => parameters,
        }
    }

    /// Return this live window's history state.
    pub fn history(&self) -> Option<&WindowHistoryState> {
        match self {
            Window::Leaf { history, .. } => Some(history),
            Window::Internal { .. } => None,
        }
    }

    /// Return a mutable reference to this live window's history state.
    pub fn history_mut(&mut self) -> Option<&mut WindowHistoryState> {
        match self {
            Window::Leaf { history, .. } => Some(history),
            Window::Internal { .. } => None,
        }
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
                window_end_valid,
                display,
                ..
            } => {
                *window_end_valid = false;
                display.clear_physical_cursor_state();
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
    /// X position where redisplay finished emitting this row, relative to the
    /// text area's left edge.
    pub end_x: i64,
    /// Visual column where redisplay finished emitting this row.
    pub end_col: i64,
    /// First buffer position represented on this row, if any.
    pub start_buffer_pos: Option<usize>,
    /// Last visible/source position associated with this row, if any.
    pub end_buffer_pos: Option<usize>,
}

/// Last authoritative physical cursor geometry for a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowCursorKind {
    NoCursor,
    FilledBox,
    HollowBox,
    Bar,
    Hbar,
}

/// Cursor position within a window's text area.
///
/// Mirrors GNU's lightweight `struct cursor_pos`; physical cursor size and
/// style live separately on `WindowCursorSnapshot`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowCursorPos {
    /// X relative to the text area's left edge, in pixels.
    pub x: i64,
    /// Y relative to the text area's top edge, in pixels.
    pub y: i64,
    /// Visual row within the window's text area.
    pub row: i64,
    /// Visual column within that row.
    pub col: i64,
}

impl WindowCursorPos {
    pub fn from_snapshot(snapshot: &WindowCursorSnapshot) -> Self {
        Self {
            x: snapshot.x,
            y: snapshot.y,
            row: snapshot.row,
            col: snapshot.col,
        }
    }
}

/// Last authoritative physical cursor geometry for a window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowCursorSnapshot {
    /// Physical cursor kind that redisplay emitted for this window.
    pub kind: WindowCursorKind,
    /// X relative to the text area's left edge, in pixels.
    pub x: i64,
    /// Y relative to the text area's top edge, in pixels.
    pub y: i64,
    /// Cursor width in pixels.
    pub width: i64,
    /// Cursor height in pixels.
    pub height: i64,
    /// Pixels above the baseline.
    pub ascent: i64,
    /// Visual row within the window's text area.
    pub row: i64,
    /// Visual column within that row.
    pub col: i64,
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
    /// Intended cursor position in the redisplay result, even when no physical
    /// cursor was emitted.
    pub logical_cursor: Option<WindowCursorPos>,
    /// Last redisplay physical cursor geometry for this window, if the cursor
    /// was shown.
    pub phys_cursor: Option<WindowCursorSnapshot>,
    /// Visible source-position geometry, sorted by `buffer_pos`.
    pub points: Vec<DisplayPointSnapshot>,
    /// Visible row metrics, sorted by `row`.
    pub rows: Vec<DisplayRowSnapshot>,
}

impl WindowDisplaySnapshot {
    pub fn logical_cursor_pos(&self) -> Option<WindowCursorPos> {
        self.logical_cursor.or_else(|| {
            self.phys_cursor
                .as_ref()
                .map(WindowCursorPos::from_snapshot)
        })
    }

    pub fn output_cursor_pos(&self) -> Option<WindowCursorPos> {
        self.rows.last().map(|row| WindowCursorPos {
            x: row.end_x,
            y: row.y,
            row: row.row,
            col: row.end_col,
        })
    }

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
            logical_cursor: None,
            phys_cursor: None,
            points: Vec::new(),
            rows: Vec::new(),
        }
    }
}

/// Redisplay-owned runtime state used to decide which GNU window hooks fire.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowHookSnapshot {
    /// Buffer currently shown in the window.
    pub buffer_id: BufferId,
    /// Last known live bounds of the window.
    pub bounds: Rect,
}

/// Per-frame redisplay record for GNU window change hook ownership.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameWindowHookRecord {
    /// Last known live windows on the frame.
    pub windows: HashMap<WindowId, WindowHookSnapshot>,
    /// Selected window the last time window change hooks were recorded.
    pub selected_window: Option<WindowId>,
    /// Whether this frame was the selected frame at last record time.
    pub was_selected_frame: bool,
}

// ---------------------------------------------------------------------------
// Frame
// ---------------------------------------------------------------------------

/// A frame (top-level window/screen).
pub struct Frame {
    pub id: FrameId,
    pub name: String,
    /// Terminal owner id for GNU `frame-terminal` / terminal lifecycle.
    pub terminal_id: u64,
    /// Root of the window tree.
    pub root_window: Window,
    /// The selected (active) window.
    pub selected_window: WindowId,
    /// The previously-selected window. GNU stores this as
    /// `frame->old_selected_window` and returns it from
    /// `frame-old-selected-window` (`src/frame.c`). Window audit
    /// Critical 8 in `drafts/window-system-audit.md` flagged the
    /// builtin as a stub returning nil because this field did not
    /// exist; the builtin now reads it.
    ///
    /// Initialized to `None` (nil) on a fresh frame to match GNU
    /// `make_frame_without_minibuffer`, then set to whichever
    /// window was previously selected on every `select-window`,
    /// `set-frame-selected-window`, and `set-window-configuration`
    /// transition.
    pub old_selected_window: Option<WindowId>,
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
    ///
    /// Window audit Medium 12 in
    /// `drafts/window-system-audit.md`: GNU's
    /// `Fset_frame_parameter` calls into the per-toolkit
    /// backend (`x_set_*`, `pgtk_set_*`, etc.) for each parameter
    /// class (position, size, fonts, fullscreen, scroll bars).
    /// neomacs writes to this HashMap unconditionally, so a
    /// `(modify-frame-parameters f '((width . 100)))` call
    /// updates the parameter alist but does not always reach the
    /// active display backend. Wiring the dispatch is tracked as
    /// audit Phase 6.
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
    ///
    /// Window audit Medium 10 / Medium 11 in
    /// `drafts/window-system-audit.md`: GNU keeps `change_stamp`,
    /// `use_time`, `sequence_number`, `old_pixel_width`,
    /// `old_pixel_height`, `old_body_pixel_width`,
    /// `old_body_pixel_height`, and `old_buffer` directly on
    /// `struct window`. neomacs centralizes the redisplay-time
    /// geometry inside `display_snapshots` and the change-detection
    /// state inside `window_hook_record`. The fields below are the
    /// neomacs-side equivalents — adding the GNU names verbatim is
    /// tracked as future work in the audit's Phase 4 plan.
    pub display_snapshots: HashMap<WindowId, WindowDisplaySnapshot>,
    /// Last recorded redisplay state for GNU window change hooks.
    pub(crate) window_hook_record: FrameWindowHookRecord,
    /// GNU `frame-window-state-change` flag.
    pub(crate) window_state_change: bool,
    /// Real frame-local Lisp face hash table, mirroring GNU `frame->face_hash_table`.
    pub face_hash_table: Value,
    /// Per-frame realized Lisp faces, mirroring GNU's `frame->face_hash_table`
    /// runtime surface for renderer-facing consumers.
    pub realized_faces: HashMap<String, RuntimeFace>,
}

impl Frame {
    pub fn new(
        id: FrameId,
        name: String,
        terminal_id: u64,
        width: u32,
        height: u32,
        root_window: Window,
    ) -> Self {
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
            terminal_id,
            root_window,
            selected_window: selected,
            // GNU `make_frame_without_minibuffer` leaves
            // `old_selected_window` as Qnil. The first
            // `select-window` records the outgoing selection.
            old_selected_window: None,
            minibuffer_window: Some(minibuffer_window),
            minibuffer_leaf: Some(minibuffer_leaf),
            width,
            height,
            window_system: None,
            // GNU Emacs frame.c make_frame() initializes the foreground-color
            // and background-color parameters before any Lisp startup runs:
            // see Fmake_terminal_frame and the GUI equivalents, which call
            // store_frame_param with the framework defaults ("black" /
            // "white") so that frame-parameter never returns nil for them.
            // Lisp code in startup (e.g. frame--current-background-mode in
            // frame.el) calls (color-values (frame-parameter f
            // 'background-color)), and color-values -> xw-color-values
            // signals wrong-type-argument: stringp nil if the value is not a
            // string. Match GNU and pre-populate the defaults here so the
            // parameter alist is never missing them.
            parameters: {
                let mut params = HashMap::new();
                params.insert("foreground-color".to_string(), Value::string("black"));
                params.insert("background-color".to_string(), Value::string("white"));
                params.insert("cursor-color".to_string(), Value::string("black"));
                params
            },
            visible: true,
            title: String::new(),
            menu_bar_height: 0,
            tool_bar_height: 0,
            tab_bar_height: 0,
            font_pixel_size: 16.0,
            char_width: 8.0,
            char_height: 16.0,
            display_snapshots: HashMap::new(),
            window_hook_record: FrameWindowHookRecord::default(),
            window_state_change: false,
            face_hash_table: Value::hash_table(HashTableTest::Eq),
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
        self.parameters.get(key).and_then(|v| v.as_int())
    }

    pub fn realized_face(&self, name: &str) -> Option<&RuntimeFace> {
        self.realized_faces.get(name)
    }

    pub fn face_hash_table(&self) -> Value {
        self.face_hash_table
    }

    pub fn set_realized_face(&mut self, name: String, face: RuntimeFace) {
        self.realized_faces.insert(name, face);
    }

    pub fn clear_realized_faces(&mut self) {
        self.realized_faces.clear();
        if self.face_hash_table.is_hash_table() {
            let _ = self.face_hash_table.with_hash_table_mut(|table| {
                table.data.clear();
                table.key_snapshots.clear();
                table.insertion_order.clear();
            });
        }
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

    /// Recompute `menu_bar_height` from the `menu-bar-lines` frame parameter.
    ///
    /// Mirrors GNU `frame.c` (`x_set_menu_bar_lines` / TTY frame init at
    /// frame.c:1307-1309): `FRAME_MENU_BAR_LINES (f) = NILP (Vmenu_bar_mode) ? 0 : 1`.
    /// On TTY the menu bar takes one character row, identical to GNU's
    /// behaviour, so the resulting pixel height is `lines * char_height`
    /// where `char_height` is 1 for TTY frames.
    ///
    /// `chrome_top_height()` already adds `menu_bar_height` into the
    /// reserved top region used by `window_text_area_bounds()`, so calling
    /// `sync_window_area_bounds()` here is enough to push the root window
    /// (and its mode line / minibuffer) down to make room.
    pub fn sync_menu_bar_height_from_parameters(&mut self) {
        let lines = self
            .frame_parameter_int("menu-bar-lines")
            .unwrap_or(0)
            .max(0) as u32;
        let char_height = self.char_height.max(1.0).round() as u32;
        self.menu_bar_height = lines.saturating_mul(char_height);
        self.sync_window_area_bounds();
    }

    /// Select a window by ID.
    pub fn select_window(&mut self, id: WindowId) -> bool {
        if self.find_window(id).is_some() {
            // GNU `Fselect_window` does NOT touch
            // `frame->old_selected_window`. That field is only
            // updated by `window_change_record`, which runs from
            // `run_window_change_functions` at redisplay time
            // (`src/window.c:3954-3990`). neomacs's analog lives
            // in `builtins/hooks.rs::frame_window_hook_record_from_live_state`
            // — it stores the new "old" inside `window_hook_record`
            // and propagates it back to `Frame::old_selected_window`
            // there. Window audit Critical 8 in
            // `drafts/window-system-audit.md`.
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

    fn live_window_ids_with_minibuffer(&self) -> Vec<WindowId> {
        let mut ids = self.window_list();
        if let Some(wid) = self.minibuffer_window {
            ids.push(wid);
        }
        ids
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
        self.begin_display_output_pass();
        for snapshot in &snapshots {
            self.commit_window_output_snapshot(snapshot);
        }
        self.set_display_snapshots(snapshots);
    }

    /// Begin a GNU-shaped output pass for all live windows on this frame.
    pub fn begin_display_output_pass(&mut self) {
        let live_window_ids = self.live_window_ids_with_minibuffer();
        for wid in &live_window_ids {
            if let Some(window) = self.find_window_mut(*wid)
                && let Some(display) = window.display_mut()
            {
                display.begin_output_pass();
            }
        }
    }

    /// Begin a new output update for one live window on this frame.
    pub fn begin_window_output_update(&mut self, window_id: WindowId) {
        if let Some(window) = self.find_window_mut(window_id)
            && let Some(display) = window.display_mut()
        {
            display.begin_window_output_update();
        }
    }

    /// Advance one live window's output cursor from an emitted display row.
    pub fn commit_window_output_row(&mut self, window_id: WindowId, row: &DisplayRowSnapshot) {
        if let Some(window) = self.find_window_mut(window_id)
            && let Some(display) = window.display_mut()
        {
            display.commit_output_row(row);
        }
    }

    /// Commit the output/cursor state for one live window from a redisplay snapshot.
    pub fn commit_window_output_snapshot(&mut self, snapshot: &WindowDisplaySnapshot) {
        if let Some(window) = self.find_window_mut(snapshot.window_id)
            && let Some(display) = window.display_mut()
        {
            display.install_logical_cursor(snapshot.logical_cursor_pos());
            if display.output_cursor.is_none() {
                display.commit_output_cursor_from_display_snapshot(snapshot);
            }
            display.apply_physical_cursor_snapshot(snapshot.phys_cursor.clone());
            display.commit_completed_redisplay();
        }
    }

    /// Replace the authoritative per-window redisplay geometry map without
    /// mutating live cursor/output state.
    pub fn set_display_snapshots(&mut self, snapshots: Vec<WindowDisplaySnapshot>) {
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
            .insert("width".to_string(), Value::fixnum(cols));
        self.parameters
            .insert("height".to_string(), Value::fixnum(total_lines));
    }

    /// Grow the minibuffer window by `delta_rows` character-cell rows.
    ///
    /// Mirrors GNU `grow_mini_window` at `src/window.c:5896-5930`.
    /// The minibuffer height is clamped to the range [1 row,
    /// `max-mini-window-height` fraction of frame inner height].
    /// After adjusting the minibuffer bounds,
    /// `sync_window_area_bounds` propagates the change to the root
    /// window tree (the root shrinks by the same delta).
    pub fn grow_mini_window(&mut self, delta_rows: i32) {
        // Snapshot scalar values before taking mutable borrow of minibuffer_leaf.
        let char_h = self.char_height.max(1.0);
        let unit = char_h;
        let frame_inner_h = (self.height as f32) - self.chrome_top_height();
        // GNU default: max-mini-window-height = 0.25
        let max_h = (frame_inner_h * 0.25).max(unit);

        let Some(mini) = self.minibuffer_leaf.as_mut() else {
            return;
        };
        let current_h = mini.bounds().height;
        let new_h = (current_h + delta_rows as f32 * unit).clamp(unit, max_h);
        if (new_h - current_h).abs() < 0.5 {
            return;
        }
        let mut bounds = *mini.bounds();
        bounds.height = new_h;
        mini.set_bounds(bounds);
        self.sync_window_area_bounds();
    }

    /// Shrink the minibuffer window to its minimum height (1 row).
    ///
    /// Mirrors GNU `shrink_mini_window` at `src/window.c:5938-5960`.
    /// The freed space is returned to the root window via
    /// `sync_window_area_bounds`.
    pub fn shrink_mini_window(&mut self) {
        let Some(mini) = self.minibuffer_leaf.as_mut() else {
            return;
        };
        let unit = self.char_height.max(1.0);
        let mut bounds = *mini.bounds();
        if (bounds.height - unit).abs() < 0.5 {
            return;
        }
        bounds.height = unit;
        mini.set_bounds(bounds);
        self.sync_window_area_bounds();
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
    deleted_window_parameters: HashMap<WindowId, WindowParameters>,
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
            deleted_window_parameters: HashMap::new(),
            window_select_count: 0,
        }
    }

    /// Allocate a new window ID.
    pub fn next_window_id(&mut self) -> WindowId {
        let id = WindowId(self.next_window_id);
        self.next_window_id += 1;
        self.deleted_windows.remove(&id);
        self.deleted_window_parameters.remove(&id);
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
        self.create_frame_on_terminal(name, 0, width, height, buffer_id)
    }

    pub fn create_frame_on_terminal(
        &mut self,
        name: &str,
        terminal_id: u64,
        width: u32,
        height: u32,
        buffer_id: BufferId,
    ) -> FrameId {
        let frame_id = FrameId(self.next_frame_id);
        self.next_frame_id += 1;

        let window_id = self.next_window_id();
        let bounds = Rect::new(0.0, 0.0, width as f32, height as f32);
        let root = Window::new_leaf(window_id, buffer_id, bounds);

        let frame = Frame::new(frame_id, name.to_string(), terminal_id, width, height, root);
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
            for wid in frame.window_list() {
                self.deleted_windows.insert(wid);
                if let Some(window) = frame.find_window(wid) {
                    self.deleted_window_parameters
                        .insert(wid, window.parameters().clone());
                }
            }
            if let Some(minibuffer_wid) = frame.minibuffer_window {
                self.deleted_windows.insert(minibuffer_wid);
                if let Some(window) = frame.find_window(minibuffer_wid) {
                    self.deleted_window_parameters
                        .insert(minibuffer_wid, window.parameters().clone());
                }
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
    ///
    /// `size` controls how space is divided:
    /// - `None` or `Some(0)`: split 50/50
    /// - `Some(n)` where n > 0: the **new** window gets `n` units (lines or
    ///   columns), the old window gets the remainder.
    /// - `Some(n)` where n < 0: the **old** window gets `|n|` units, the new
    ///   window gets the remainder.
    pub fn split_window(
        &mut self,
        frame_id: FrameId,
        window_id: WindowId,
        direction: SplitDirection,
        new_buffer_id: BufferId,
        size: Option<i64>,
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
            size,
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

        let deleted_parameters = frame
            .find_window(window_id)
            .map(|window| window.parameters().clone());
        let removed = delete_window_in_tree(&mut frame.root_window, window_id);
        if removed {
            self.deleted_windows.insert(window_id);
            self.deleted_window_parameters
                .insert(window_id, deleted_parameters.unwrap_or_default());
            frame.recalculate_minibuffer_bounds();
        }

        if removed && frame.selected_window == window_id {
            // Select the first remaining leaf. We do NOT touch
            // `old_selected_window` here — that field is recorded
            // by `window_change_record` (GNU
            // `src/window.c:3954-3990`) at redisplay time, not
            // immediately on deletion.
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
        self.deleted_window_parameters.remove(&id);
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

    /// Look up a window by id across every live frame, returning a
    /// shared reference. Mirrors GNU's `decode_window` plus tree
    /// walk.
    pub fn lookup_window(&self, window_id: WindowId) -> Option<&Window> {
        for frame in self.frames.values() {
            if frame.minibuffer_window == Some(window_id) {
                return frame.minibuffer_leaf.as_ref();
            }
            if let Some(w) = frame.find_window(window_id) {
                return Some(w);
            }
        }
        None
    }

    /// Look up a window by id across every live frame, returning a
    /// mutable reference.
    pub fn lookup_window_mut(&mut self, window_id: WindowId) -> Option<&mut Window> {
        for frame in self.frames.values_mut() {
            if frame.minibuffer_window == Some(window_id) {
                return frame.minibuffer_leaf.as_mut();
            }
            if let Some(w) = frame.find_window_mut(window_id) {
                return Some(w);
            }
        }
        None
    }

    /// Read `w->new_pixel`. Mirrors GNU
    /// `Fwindow_new_pixel` (`src/window.c`). Returns `None` if the
    /// window doesn't exist or has no pending pixel size.
    pub fn window_new_pixel(&self, window_id: WindowId) -> Option<i64> {
        self.lookup_window(window_id).and_then(Window::new_pixel)
    }

    /// Read `w->new_total`. Mirrors GNU `Fwindow_new_total`.
    pub fn window_new_total(&self, window_id: WindowId) -> Option<i64> {
        self.lookup_window(window_id).and_then(Window::new_total)
    }

    /// Read `w->new_normal`. Mirrors GNU `Fwindow_new_normal`.
    pub fn window_new_normal(&self, window_id: WindowId) -> Value {
        self.lookup_window(window_id)
            .map(Window::new_normal)
            .unwrap_or(Value::NIL)
    }

    /// Write `w->new_pixel`. When `add` is true, accumulates onto
    /// the existing slot (mirroring GNU
    /// `Fset_window_new_pixel` ADD argument).
    pub fn set_window_new_pixel(&mut self, window_id: WindowId, size: i64, add: bool) -> i64 {
        if let Some(window) = self.lookup_window_mut(window_id) {
            let stored = if add {
                window.new_pixel().unwrap_or(0) + size
            } else {
                size
            };
            window.set_new_pixel(Some(stored));
            stored
        } else {
            size
        }
    }

    /// Write `w->new_total`. ADD semantics match GNU
    /// `Fset_window_new_total`.
    pub fn set_window_new_total(&mut self, window_id: WindowId, size: i64, add: bool) -> i64 {
        if let Some(window) = self.lookup_window_mut(window_id) {
            let stored = if add {
                window.new_total().unwrap_or(0) + size
            } else {
                size
            };
            window.set_new_total(Some(stored));
            stored
        } else {
            size
        }
    }

    /// Write `w->new_normal`. Mirrors GNU `Fset_window_new_normal`.
    pub fn set_window_new_normal(&mut self, window_id: WindowId, value: Value) {
        if let Some(window) = self.lookup_window_mut(window_id) {
            window.set_new_normal(value);
        }
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
///
/// `size` semantics (lines for vertical, columns for horizontal — 1 unit = 1.0
/// pixel in the abstract coordinate system):
/// - `None` / `Some(0)`: 50/50 split.
/// - `Some(n)` (n > 0): new window (right/bottom) gets `n` units.
/// - `Some(n)` (n < 0): old window (left/top) keeps `|n|` units.
fn split_window_in_tree(
    tree: &mut Window,
    target: WindowId,
    direction: SplitDirection,
    internal_id: WindowId,
    new_id: WindowId,
    new_buffer_id: BufferId,
    size: Option<i64>,
) -> Option<()> {
    if tree.id() == target {
        let old_id = tree.id();
        let old_bounds = *tree.bounds();
        let old_window = tree.clone();

        if let Window::Leaf {
            buffer_id: buf_id, ..
        } = old_window
        {
            let (left_bounds, right_bounds) = match direction {
                SplitDirection::Horizontal => {
                    let total = old_bounds.width;
                    // Compute size for the NEW window (right pane).
                    let new_size = match size {
                        Some(n) if n > 0 => (n as f32).min(total - 1.0).max(1.0),
                        Some(n) if n < 0 => (total - (-n) as f32).max(1.0).min(total - 1.0),
                        _ => total / 2.0,
                    };
                    let old_size = total - new_size;
                    (
                        Rect::new(old_bounds.x, old_bounds.y, old_size, old_bounds.height),
                        Rect::new(
                            old_bounds.x + old_size,
                            old_bounds.y,
                            new_size,
                            old_bounds.height,
                        ),
                    )
                }
                SplitDirection::Vertical => {
                    let total = old_bounds.height;
                    // Compute size for the NEW window (bottom pane).
                    let new_size = match size {
                        Some(n) if n > 0 => (n as f32).min(total - 1.0).max(1.0),
                        Some(n) if n < 0 => (total - (-n) as f32).max(1.0).min(total - 1.0),
                        _ => total / 2.0,
                    };
                    let old_size = total - new_size;
                    (
                        Rect::new(old_bounds.x, old_bounds.y, old_bounds.width, old_size),
                        Rect::new(
                            old_bounds.x,
                            old_bounds.y + old_size,
                            old_bounds.width,
                            new_size,
                        ),
                    )
                }
            };

            let mut old_leaf = old_window;
            old_leaf.set_bounds(left_bounds);

            let mut new_leaf = old_leaf.clone();
            if let Window::Leaf {
                id,
                buffer_id,
                bounds,
                parameters,
                history,
                window_start,
                window_end_pos,
                window_end_bytepos,
                window_end_vpos,
                window_end_valid,
                point,
                old_point,
                vscroll,
                preserve_vscroll_p,
                ..
            } = &mut new_leaf
            {
                *id = new_id;
                *buffer_id = new_buffer_id;
                *bounds = right_bounds;
                parameters.clear();
                *history = WindowHistoryState::default();
                *window_start = 1;
                *window_end_pos = 0;
                *window_end_bytepos = 0;
                *window_end_vpos = 0;
                *window_end_valid = false;
                *point = 1;
                *old_point = 1;
                *vscroll = 0;
                *preserve_vscroll_p = false;
            }

            // Capture the old leaf's pre-split normal-size
            // fractions before we mutate the children. The new
            // internal node will inherit them because it occupies
            // the slot the old leaf used to fill.
            let inherited_normal_lines = old_leaf.normal_lines();
            let inherited_normal_cols = old_leaf.normal_cols();

            // Compute the new normal-size fractions for both
            // children, mirroring GNU `Fsplit_window_internal`
            // (`src/window.c:5517-5644`). Each sibling's fraction
            // in the split direction is its bounds divided by the
            // parent. The orthogonal fraction is always 1.0
            // because both children fill the parent in that
            // direction.
            let parent_size = match direction {
                SplitDirection::Horizontal => old_bounds.width,
                SplitDirection::Vertical => old_bounds.height,
            };
            let (old_fraction, new_fraction) = if parent_size > 0.0 {
                let old_frac = match direction {
                    SplitDirection::Horizontal => left_bounds.width / parent_size,
                    SplitDirection::Vertical => left_bounds.height / parent_size,
                };
                let new_frac = match direction {
                    SplitDirection::Horizontal => right_bounds.width / parent_size,
                    SplitDirection::Vertical => right_bounds.height / parent_size,
                };
                (old_frac as f64, new_frac as f64)
            } else {
                (0.5, 0.5)
            };

            match direction {
                SplitDirection::Horizontal => {
                    old_leaf.set_normal_cols(Value::make_float(old_fraction));
                    old_leaf.set_normal_lines(Value::make_float(1.0));
                    new_leaf.set_normal_cols(Value::make_float(new_fraction));
                    new_leaf.set_normal_lines(Value::make_float(1.0));
                }
                SplitDirection::Vertical => {
                    old_leaf.set_normal_lines(Value::make_float(old_fraction));
                    old_leaf.set_normal_cols(Value::make_float(1.0));
                    new_leaf.set_normal_lines(Value::make_float(new_fraction));
                    new_leaf.set_normal_cols(Value::make_float(1.0));
                }
            }

            *tree = Window::Internal {
                id: internal_id,
                direction,
                children: vec![old_leaf, new_leaf],
                bounds: old_bounds,
                parameters: Vec::new(),
                combination_limit: false,
                new_pixel: None,
                new_total: None,
                new_normal: Value::NIL,
                // The new internal node takes the slot that the
                // old leaf used to fill, so it inherits the
                // leaf's pre-split proportional fractions.
                normal_lines: inherited_normal_lines,
                normal_cols: inherited_normal_cols,
            };

            return Some(());
        }
    }

    // Recurse into children.
    if let Window::Internal { children, .. } = tree {
        for child in children {
            if split_window_in_tree(
                child,
                target,
                direction,
                internal_id,
                new_id,
                new_buffer_id,
                size,
            )
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
///
/// The pending sizes are read from each window's own `new_pixel`
/// slot (set previously via `set-window-new-pixel`), mirroring the
/// way GNU `window_resize_apply` walks `w->new_pixel` on every
/// node it visits. After audit Structural 1 in
/// `drafts/window-system-audit.md`, the slot lives on
/// `Window::Leaf` / `Window::Internal` directly so the resize
/// function no longer needs a side-table HashMap.
pub fn window_resize_apply(
    window: &mut Window,
    horflag: bool,
    _char_width: f32,
    _char_height: f32,
) {
    // Apply new_pixel to this window's bounds.
    let new_px = window.new_pixel();
    let bounds = *window.bounds();
    if let Some(px) = new_px {
        let px = px.max(0) as f32;
        if horflag {
            window.set_bounds(Rect::new(bounds.x, bounds.y, px, bounds.height));
        } else {
            window.set_bounds(Rect::new(bounds.x, bounds.y, bounds.width, px));
        }
        // Clear the pending slot to mirror GNU's
        // `wset_new_pixel(w, make_fixnum(-1))` reset at the end of
        // `window_resize_apply`.
        window.set_new_pixel(None);
    }

    // Commit the pending normal-size fraction. GNU
    // `Fwindow_resize_apply` (`src/window.c:4826,4835`):
    //
    //   if (horflag) wset_normal_cols (w, w->new_normal);
    //   else         wset_normal_lines (w, w->new_normal);
    //
    // Audit Critical 7 in `drafts/window-system-audit.md`:
    // moving the persistent fraction onto the Window struct here
    // means `window-normal-size` reads it back instead of
    // re-deriving the ratio from current pixel bounds.
    let pending_normal = window.new_normal();
    if !pending_normal.is_nil() {
        if horflag {
            window.set_normal_cols(pending_normal);
        } else {
            window.set_normal_lines(pending_normal);
        }
        window.set_new_normal(Value::NIL);
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
            window_resize_apply(child, horflag, _char_width, _char_height);

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
///
/// Reads each window's own `new_pixel` slot, mirroring GNU's
/// recursive walk in `window_resize_check`.
pub fn window_resize_check(window: &Window, horflag: bool) -> bool {
    let my_new = window.new_pixel().unwrap_or_else(|| {
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
                        c.new_pixel().unwrap_or_else(|| {
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
            children.iter().all(|c| window_resize_check(c, horflag))
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
///
/// The pending size for each window is read from `w->new_total`
/// (now stored on the Window enum after audit Structural 1).
pub fn window_resize_apply_total(
    window: &mut Window,
    horflag: bool,
    char_width: f32,
    char_height: f32,
) {
    let new_total = window.new_total();

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
        // Mirror GNU `wset_new_total(w, make_fixnum(-1))`.
        window.set_new_total(None);
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
            window_resize_apply_total(child, horflag, char_width, char_height);

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
        // Deleted window parameter maps
        for params in self.deleted_window_parameters.values() {
            for (k, v) in params {
                roots.push(*k);
                roots.push(*v);
            }
        }
        // Frame and window tree parameters
        for frame in self.frames.values() {
            for v in frame.parameters.values() {
                roots.push(*v);
            }
            roots.push(frame.face_hash_table);
            trace_window(&frame.root_window, roots);
            if let Some(mb) = &frame.minibuffer_leaf {
                trace_window(mb, roots);
            }
        }
    }
}

fn trace_window(window: &Window, roots: &mut Vec<Value>) {
    match window {
        Window::Leaf { display, .. } => {
            for (key, value) in window.parameters() {
                roots.push(*key);
                roots.push(*value);
            }
            if let Some(history) = window.history() {
                roots.push(history.prev_buffers);
                roots.push(history.next_buffers);
            }
            roots.push(display.display_table);
            roots.push(display.cursor_type);
            roots.push(display.vertical_scroll_bar_type);
            roots.push(display.horizontal_scroll_bar_type);
        }
        Window::Internal { children, .. } => {
            for (key, value) in window.parameters() {
                roots.push(*key);
                roots.push(*value);
            }
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
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let frame = mgr.get(fid).unwrap();

        assert_eq!(frame.window_count(), 1);
        assert!(frame.selected_window().is_some());
        assert!(frame.selected_window().unwrap().is_leaf());
    }

    #[test]
    fn split_window_horizontal() {
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        let new_wid = mgr.split_window(fid, wid, SplitDirection::Horizontal, BufferId(2), None);
        assert!(new_wid.is_some());

        let frame = mgr.get(fid).unwrap();
        assert_eq!(frame.window_count(), 2);
    }

    #[test]
    fn split_window_vertical() {
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        let new_wid = mgr.split_window(fid, wid, SplitDirection::Vertical, BufferId(2), None);
        assert!(new_wid.is_some());

        let frame = mgr.get(fid).unwrap();
        assert_eq!(frame.window_count(), 2);
    }

    #[test]
    fn split_window_copies_window_display_state() {
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        {
            let frame = mgr.get_mut(fid).unwrap();
            frame.set_window_system(Some(Value::symbol("neo")));
            let wid = frame.window_list()[0];
            let display = frame
                .find_window_mut(wid)
                .and_then(Window::display_mut)
                .expect("leaf display");
            display.display_table = Value::fixnum(17);
            display.cursor_type = Value::NIL;
            display.left_fringe_width = 3;
            display.right_fringe_width = 5;
            display.fringes_outside_margins = true;
            display.fringes_persistent = true;
            display.scroll_bar_width = 11;
            display.vertical_scroll_bar_type = Value::T;
            display.scroll_bar_height = 7;
            display.horizontal_scroll_bar_type = Value::NIL;
            display.scroll_bars_persistent = true;
        }

        let original_wid = mgr.get(fid).unwrap().window_list()[0];
        let new_wid = mgr
            .split_window(
                fid,
                original_wid,
                SplitDirection::Horizontal,
                BufferId(2),
                None,
            )
            .expect("split");

        let frame = mgr.get(fid).unwrap();
        let original_display = frame
            .find_window(original_wid)
            .and_then(Window::display)
            .expect("original display");
        let new_display = frame
            .find_window(new_wid)
            .and_then(Window::display)
            .expect("new display");

        assert_eq!(original_display.display_table, Value::fixnum(17));
        assert_eq!(new_display.display_table, Value::fixnum(17));
        assert_eq!(original_display.cursor_type, Value::NIL);
        assert_eq!(new_display.cursor_type, Value::NIL);
        assert_eq!(original_display.left_fringe_width, 3);
        assert_eq!(new_display.left_fringe_width, 3);
        assert_eq!(original_display.right_fringe_width, 5);
        assert_eq!(new_display.right_fringe_width, 5);
        assert!(original_display.fringes_outside_margins);
        assert!(new_display.fringes_outside_margins);
        assert!(original_display.fringes_persistent);
        assert!(new_display.fringes_persistent);
        assert_eq!(original_display.scroll_bar_width, 11);
        assert_eq!(new_display.scroll_bar_width, 11);
        assert_eq!(original_display.vertical_scroll_bar_type, Value::T);
        assert_eq!(new_display.vertical_scroll_bar_type, Value::T);
        assert_eq!(original_display.scroll_bar_height, 7);
        assert_eq!(new_display.scroll_bar_height, 7);
        assert_eq!(original_display.horizontal_scroll_bar_type, Value::NIL);
        assert_eq!(new_display.horizontal_scroll_bar_type, Value::NIL);
        assert!(original_display.scroll_bars_persistent);
        assert!(new_display.scroll_bars_persistent);
    }

    #[test]
    fn split_window_resets_new_leaf_vscroll_state() {
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let original_wid = mgr.get(fid).unwrap().window_list()[0];

        if let Some(Window::Leaf {
            vscroll,
            preserve_vscroll_p,
            ..
        }) = mgr
            .get_mut(fid)
            .and_then(|frame| frame.find_window_mut(original_wid))
        {
            *vscroll = -19;
            *preserve_vscroll_p = true;
        }

        let new_wid = mgr
            .split_window(
                fid,
                original_wid,
                SplitDirection::Horizontal,
                BufferId(2),
                None,
            )
            .expect("split");

        let frame = mgr.get(fid).unwrap();
        let Window::Leaf {
            vscroll: original_vscroll,
            preserve_vscroll_p: original_preserve,
            ..
        } = frame.find_window(original_wid).unwrap()
        else {
            panic!("expected original leaf");
        };
        let Window::Leaf {
            vscroll: new_vscroll,
            preserve_vscroll_p: new_preserve,
            ..
        } = frame.find_window(new_wid).unwrap()
        else {
            panic!("expected new leaf");
        };

        assert_eq!(*original_vscroll, -19);
        assert!(*original_preserve);
        assert_eq!(*new_vscroll, 0);
        assert!(!*new_preserve);
    }

    #[test]
    fn delete_window() {
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        // Split first.
        let new_wid = mgr
            .split_window(fid, wid, SplitDirection::Horizontal, BufferId(2), None)
            .unwrap();

        // Delete the new window.
        assert!(mgr.delete_window(fid, new_wid));
        assert_eq!(mgr.get(fid).unwrap().window_count(), 1);
    }

    #[test]
    fn cannot_delete_last_window() {
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        assert!(!mgr.delete_window(fid, wid));
    }

    #[test]
    fn select_window() {
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        let new_wid = mgr
            .split_window(fid, wid, SplitDirection::Horizontal, BufferId(2), None)
            .unwrap();

        assert!(mgr.get_mut(fid).unwrap().select_window(new_wid));
        assert_eq!(mgr.get(fid).unwrap().selected_window.0, new_wid.0,);
    }

    #[test]
    fn window_at_coordinates() {
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        mgr.split_window(fid, wid, SplitDirection::Horizontal, BufferId(2), None);

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
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let frame = mgr.get(fid).unwrap();

        assert_eq!(frame.columns(), 100); // 800/8
        assert_eq!(frame.lines(), 37); // 600/16 = 37
    }

    #[test]
    fn delete_frame() {
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        assert!(mgr.delete_frame(fid));
        assert!(mgr.get(fid).is_none());
    }

    #[test]
    fn multiple_frames() {
        crate::test_utils::init_test_tracing();
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
        crate::test_utils::init_test_tracing();
        let r = Rect::new(10.0, 20.0, 100.0, 50.0);
        assert!(r.contains(10.0, 20.0));
        assert!(r.contains(50.0, 40.0));
        assert!(!r.contains(9.0, 20.0));
        assert!(!r.contains(110.0, 70.0));
    }

    #[test]
    fn find_window_frame_id() {
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        assert_eq!(mgr.find_window_frame_id(wid), Some(fid));
        assert_eq!(mgr.find_window_frame_id(WindowId(99999)), None);
    }

    #[test]
    fn is_live_window_id() {
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        assert!(mgr.is_live_window_id(wid));
        assert!(!mgr.is_live_window_id(WindowId(99999)));
    }

    #[test]
    fn window_parameters() {
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];

        let key = Value::symbol("my-param");
        let val = Value::fixnum(42);

        // Initially no parameter
        assert!(mgr.window_parameter(wid, &key).is_none());

        mgr.set_window_parameter(wid, key, val);
        assert_eq!(mgr.window_parameter(wid, &key), Some(Value::fixnum(42)));
    }

    #[test]
    fn split_window_does_not_copy_window_parameters() {
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];
        let key = Value::symbol("my-param");

        mgr.set_window_parameter(wid, key, Value::fixnum(42));
        let new_wid = mgr
            .split_window(fid, wid, SplitDirection::Horizontal, BufferId(2), None)
            .expect("split");

        assert_eq!(mgr.window_parameter(wid, &key), Some(Value::fixnum(42)));
        assert_eq!(mgr.window_parameter(new_wid, &key), None);
    }

    #[test]
    fn deleted_window_retains_window_parameters() {
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().window_list()[0];
        let other = mgr
            .split_window(fid, wid, SplitDirection::Horizontal, BufferId(2), None)
            .expect("split");
        let key = Value::symbol("deleted-param");

        mgr.set_window_parameter(other, key, Value::fixnum(7));
        assert!(mgr.delete_window(fid, other));
        assert_eq!(mgr.window_parameter(other, &key), Some(Value::fixnum(7)));
    }

    #[test]
    fn replace_buffer_in_windows() {
        crate::test_utils::init_test_tracing();
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
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let w1 = mgr.get(fid).unwrap().window_list()[0];

        // Split w1 horizontally → w2
        let w2 = mgr
            .split_window(fid, w1, SplitDirection::Horizontal, BufferId(2), None)
            .unwrap();

        // Split w2 vertically → w3
        let w3 = mgr
            .split_window(fid, w2, SplitDirection::Vertical, BufferId(3), None)
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
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let w1 = mgr.get(fid).unwrap().window_list()[0];
        let w2 = mgr
            .split_window(fid, w1, SplitDirection::Horizontal, BufferId(2), None)
            .unwrap();

        let t1 = mgr.note_window_selected(w1);
        let t2 = mgr.note_window_selected(w2);
        // Each selection should get a monotonically increasing use-time
        assert!(t2 > t1);
    }

    #[test]
    fn window_set_buffer_resets_position() {
        crate::test_utils::init_test_tracing();
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
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let w1 = mgr.get(fid).unwrap().window_list()[0];
        let w2 = mgr
            .split_window(fid, w1, SplitDirection::Horizontal, BufferId(2), None)
            .unwrap();

        let frame = mgr.get_mut(fid).unwrap();
        frame.char_width = 10.0;
        frame.char_height = 20.0;
        frame.replace_display_snapshots(vec![WindowDisplaySnapshot {
            window_id: w1,
            phys_cursor: Some(WindowCursorSnapshot {
                kind: WindowCursorKind::Bar,
                x: 7,
                y: 13,
                width: 9,
                height: 17,
                ascent: 12,
                row: 2,
                col: 5,
            }),
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
        assert!(
            frame
                .find_window(w1)
                .and_then(|window| window.display())
                .and_then(|display| display.phys_cursor.as_ref())
                .is_none()
        );
        assert_eq!(frame.parameters.get("width"), Some(&Value::fixnum(40)));
        assert_eq!(frame.parameters.get("height"), Some(&Value::fixnum(13)));

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
    fn replace_display_snapshots_syncs_live_window_cursor_state() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().selected_window;
        let cursor = WindowCursorSnapshot {
            kind: WindowCursorKind::Bar,
            x: 11,
            y: 29,
            width: 3,
            height: 16,
            ascent: 12,
            row: 1,
            col: 4,
        };
        let output_cursor = WindowCursorPos {
            x: 44,
            y: 29,
            row: 1,
            col: 8,
        };

        let frame = mgr.get_mut(fid).unwrap();
        frame.replace_display_snapshots(vec![WindowDisplaySnapshot {
            window_id: wid,
            phys_cursor: Some(cursor.clone()),
            rows: vec![DisplayRowSnapshot {
                row: 1,
                y: 29,
                height: 16,
                end_x: output_cursor.x,
                end_col: output_cursor.col,
                start_buffer_pos: Some(1),
                end_buffer_pos: Some(8),
            }],
            ..WindowDisplaySnapshot::default()
        }]);

        let display = frame
            .find_window(wid)
            .and_then(|window| window.display())
            .expect("window display state");
        let cursor_pos = WindowCursorPos::from_snapshot(&cursor);
        assert!(display.phys_cursor_on_p);
        assert_eq!(display.phys_cursor_type, WindowCursorKind::Bar);
        assert!(!display.last_cursor_off_p);
        assert_eq!(display.last_cursor_vpos, cursor.row);
        assert_eq!(display.cursor.as_ref(), Some(&cursor_pos));
        assert_eq!(display.phys_cursor.as_ref(), Some(&cursor));
        assert_eq!(display.output_cursor.as_ref(), Some(&output_cursor));
    }

    #[test]
    fn set_display_snapshots_preserves_live_window_cursor_state() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().selected_window;
        let cursor = WindowCursorSnapshot {
            kind: WindowCursorKind::Bar,
            x: 11,
            y: 29,
            width: 3,
            height: 16,
            ascent: 12,
            row: 1,
            col: 4,
        };
        let cursor_pos = WindowCursorPos::from_snapshot(&cursor);
        let output_cursor = WindowCursorPos {
            x: 44,
            y: 29,
            row: 1,
            col: 8,
        };
        let snapshot = WindowDisplaySnapshot {
            window_id: wid,
            phys_cursor: Some(cursor.clone()),
            rows: vec![DisplayRowSnapshot {
                row: 1,
                y: 29,
                height: 16,
                end_x: output_cursor.x,
                end_col: output_cursor.col,
                start_buffer_pos: Some(1),
                end_buffer_pos: Some(8),
            }],
            ..WindowDisplaySnapshot::default()
        };

        let frame = mgr.get_mut(fid).unwrap();
        frame.begin_display_output_pass();
        frame.commit_window_output_snapshot(&snapshot);
        frame.set_display_snapshots(vec![WindowDisplaySnapshot {
            window_id: wid,
            phys_cursor: None,
            ..WindowDisplaySnapshot::default()
        }]);

        let display = frame
            .find_window(wid)
            .and_then(|window| window.display())
            .expect("window display state");
        assert_eq!(display.cursor.as_ref(), Some(&cursor_pos));
        assert_eq!(display.output_cursor.as_ref(), Some(&output_cursor));
        assert_eq!(display.phys_cursor.as_ref(), Some(&cursor));
        assert_eq!(
            frame
                .window_display_snapshot(wid)
                .and_then(|snapshot| snapshot.phys_cursor.as_ref()),
            None
        );
    }

    #[test]
    fn replace_display_snapshots_preserves_logical_cursor_without_physical_cursor() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().selected_window;
        let logical_cursor = WindowCursorPos {
            x: 24,
            y: 16,
            row: 1,
            col: 3,
        };

        let frame = mgr.get_mut(fid).unwrap();
        frame.replace_display_snapshots(vec![WindowDisplaySnapshot {
            window_id: wid,
            logical_cursor: Some(logical_cursor),
            rows: vec![DisplayRowSnapshot {
                row: 1,
                y: 16,
                height: 16,
                end_x: 64,
                end_col: 8,
                start_buffer_pos: Some(10),
                end_buffer_pos: Some(18),
            }],
            ..WindowDisplaySnapshot::default()
        }]);

        let display = frame
            .find_window(wid)
            .and_then(|window| window.display())
            .expect("window display state");
        assert_eq!(display.cursor, Some(logical_cursor));
        assert_eq!(
            display.output_cursor,
            Some(WindowCursorPos {
                x: 64,
                y: 16,
                row: 1,
                col: 8,
            })
        );
        assert_eq!(display.phys_cursor, None);
        assert_eq!(display.phys_cursor_type, WindowCursorKind::NoCursor);
        assert!(!display.phys_cursor_on_p);
    }

    #[test]
    fn replace_display_snapshots_commits_last_cursor_visibility_state() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().selected_window;
        let cursor = WindowCursorSnapshot {
            kind: WindowCursorKind::FilledBox,
            x: 4,
            y: 8,
            width: 8,
            height: 16,
            ascent: 12,
            row: 0,
            col: 0,
        };

        let frame = mgr.get_mut(fid).unwrap();
        let display = frame
            .find_window_mut(wid)
            .and_then(|window| window.display_mut())
            .expect("window display state");
        display.cursor_off_p = true;

        frame.replace_display_snapshots(vec![WindowDisplaySnapshot {
            window_id: wid,
            phys_cursor: Some(cursor),
            ..WindowDisplaySnapshot::default()
        }]);

        let display = frame
            .find_window(wid)
            .and_then(|window| window.display())
            .expect("window display state");
        assert!(display.cursor_off_p);
        assert!(display.last_cursor_off_p);
    }

    #[test]
    fn clear_physical_cursor_state_preserves_committed_cursor_history() {
        let cursor = WindowCursorSnapshot {
            kind: WindowCursorKind::FilledBox,
            x: 9,
            y: 21,
            width: 8,
            height: 16,
            ascent: 12,
            row: 2,
            col: 5,
        };
        let snapshot = WindowDisplaySnapshot {
            window_id: WindowId(1),
            phys_cursor: Some(cursor.clone()),
            rows: vec![DisplayRowSnapshot {
                row: 2,
                y: 21,
                height: 16,
                end_x: 9,
                end_col: 5,
                start_buffer_pos: Some(11),
                end_buffer_pos: Some(11),
            }],
            ..WindowDisplaySnapshot::default()
        };
        let mut display = WindowDisplayState::default();
        display.begin_output_pass();
        display.install_logical_cursor(Some(WindowCursorPos::from_snapshot(&cursor)));
        display.commit_output_cursor_from_display_snapshot(&snapshot);
        display.apply_physical_cursor_snapshot(Some(cursor.clone()));
        display.commit_completed_redisplay();

        display.clear_physical_cursor_state();

        let cursor_pos = WindowCursorPos::from_snapshot(&cursor);
        assert_eq!(display.cursor, Some(cursor_pos));
        assert_eq!(display.output_cursor, Some(cursor_pos));
        assert_eq!(display.phys_cursor, None);
        assert_eq!(display.phys_cursor_type, WindowCursorKind::NoCursor);
        assert!(!display.phys_cursor_on_p);
        assert!(!display.last_cursor_off_p);
        assert_eq!(display.last_cursor_vpos, cursor.row);
    }

    #[test]
    fn begin_output_pass_preserves_committed_output_cursor_until_next_commit() {
        let cursor = WindowCursorSnapshot {
            kind: WindowCursorKind::FilledBox,
            x: 9,
            y: 21,
            width: 8,
            height: 16,
            ascent: 12,
            row: 2,
            col: 5,
        };
        let cursor_pos = WindowCursorPos::from_snapshot(&cursor);
        let mut display = WindowDisplayState::default();

        display.install_logical_cursor(Some(cursor_pos));
        display.commit_output_cursor_from_cursor();
        display.apply_physical_cursor_snapshot(Some(cursor.clone()));

        display.begin_output_pass();

        assert_eq!(display.cursor, None);
        assert_eq!(display.output_cursor, Some(cursor_pos));
        assert_eq!(display.phys_cursor, None);
        assert_eq!(display.phys_cursor_type, WindowCursorKind::NoCursor);
        assert!(!display.phys_cursor_on_p);
    }

    #[test]
    fn begin_window_output_update_clears_output_cursor_for_active_window() {
        let cursor = WindowCursorSnapshot {
            kind: WindowCursorKind::FilledBox,
            x: 9,
            y: 21,
            width: 8,
            height: 16,
            ascent: 12,
            row: 2,
            col: 5,
        };
        let cursor_pos = WindowCursorPos::from_snapshot(&cursor);
        let mut display = WindowDisplayState::default();

        display.install_logical_cursor(Some(cursor_pos));
        display.commit_output_cursor_from_cursor();
        display.apply_physical_cursor_snapshot(Some(cursor));

        display.begin_window_output_update();

        assert_eq!(display.cursor, None);
        assert_eq!(display.output_cursor, None);
        assert_eq!(display.phys_cursor, None);
        assert_eq!(display.phys_cursor_type, WindowCursorKind::NoCursor);
        assert!(!display.phys_cursor_on_p);
    }

    #[test]
    fn output_cursor_tracks_explicit_output_lifecycle() {
        let logical_cursor = WindowCursorPos {
            x: 12,
            y: 24,
            row: 1,
            col: 3,
        };
        let mut display = WindowDisplayState::default();

        display.install_logical_cursor(Some(logical_cursor));
        assert_eq!(display.cursor, Some(logical_cursor));
        assert_eq!(display.output_cursor, None);

        display.commit_output_cursor_from_cursor();
        assert_eq!(display.output_cursor, Some(logical_cursor));

        display.output_cursor_to(1, 6, 24, 36);
        assert_eq!(
            display.output_cursor,
            Some(WindowCursorPos {
                x: 36,
                y: 24,
                row: 1,
                col: 6,
            })
        );
        assert_eq!(display.cursor, Some(logical_cursor));
    }

    #[test]
    fn completed_redisplay_preserves_point_row_history_over_output_progress() {
        let mut display = WindowDisplayState::default();
        display.install_logical_cursor(Some(WindowCursorPos {
            x: 12,
            y: 24,
            row: 1,
            col: 3,
        }));
        display.output_cursor_to(4, 9, 72, 80);

        display.commit_completed_redisplay();

        assert_eq!(display.last_cursor_vpos, 1);
    }

    #[test]
    fn output_pass_commits_output_cursor_from_row_geometry() {
        let cursor = WindowCursorSnapshot {
            kind: WindowCursorKind::Bar,
            x: 44,
            y: 32,
            width: 3,
            height: 16,
            ascent: 12,
            row: 2,
            col: 7,
        };
        let snapshot = WindowDisplaySnapshot {
            window_id: WindowId(1),
            phys_cursor: Some(cursor.clone()),
            rows: vec![DisplayRowSnapshot {
                row: 2,
                y: 32,
                height: 16,
                end_x: 80,
                end_col: 12,
                start_buffer_pos: Some(20),
                end_buffer_pos: Some(32),
            }],
            ..WindowDisplaySnapshot::default()
        };
        let mut display = WindowDisplayState::default();

        display.begin_output_pass();
        display.install_logical_cursor(Some(WindowCursorPos::from_snapshot(&cursor)));
        display.commit_output_cursor_from_display_snapshot(&snapshot);
        display.apply_physical_cursor_snapshot(Some(cursor.clone()));
        display.commit_completed_redisplay();

        assert_eq!(
            display.output_cursor,
            Some(WindowCursorPos {
                x: 80,
                y: 32,
                row: 2,
                col: 12,
            })
        );
        assert_eq!(display.last_cursor_vpos, 2);
        assert_eq!(display.phys_cursor, Some(cursor));
    }

    #[test]
    fn commit_window_output_snapshot_prefers_live_output_progress() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().selected_window;
        let cursor = WindowCursorSnapshot {
            kind: WindowCursorKind::Bar,
            x: 44,
            y: 32,
            width: 3,
            height: 16,
            ascent: 12,
            row: 2,
            col: 7,
        };
        let snapshot = WindowDisplaySnapshot {
            window_id: wid,
            logical_cursor: Some(WindowCursorPos::from_snapshot(&cursor)),
            phys_cursor: Some(cursor.clone()),
            rows: vec![DisplayRowSnapshot {
                row: 4,
                y: 64,
                height: 16,
                end_x: 144,
                end_col: 18,
                start_buffer_pos: Some(20),
                end_buffer_pos: Some(38),
            }],
            ..WindowDisplaySnapshot::default()
        };
        let frame = mgr.get_mut(fid).expect("frame");
        frame.begin_display_output_pass();
        frame.begin_window_output_update(wid);
        frame.commit_window_output_row(
            wid,
            &DisplayRowSnapshot {
                row: 2,
                y: 32,
                height: 16,
                end_x: 80,
                end_col: 12,
                start_buffer_pos: Some(20),
                end_buffer_pos: Some(32),
            },
        );
        frame.commit_window_output_snapshot(&snapshot);

        let display = frame
            .find_window(wid)
            .and_then(|window| window.display())
            .expect("window display state");
        assert_eq!(
            display.output_cursor,
            Some(WindowCursorPos {
                x: 80,
                y: 32,
                row: 2,
                col: 12,
            })
        );
        assert_eq!(display.phys_cursor, Some(cursor));
    }

    #[test]
    fn output_pass_keeps_cursor_target_and_output_progress_separate() {
        let cursor = WindowCursorSnapshot {
            kind: WindowCursorKind::Bar,
            x: 18,
            y: 16,
            width: 3,
            height: 16,
            ascent: 12,
            row: 1,
            col: 2,
        };
        let snapshot = WindowDisplaySnapshot {
            window_id: WindowId(1),
            phys_cursor: Some(cursor.clone()),
            rows: vec![
                DisplayRowSnapshot {
                    row: 0,
                    y: 0,
                    height: 16,
                    end_x: 64,
                    end_col: 8,
                    start_buffer_pos: Some(1),
                    end_buffer_pos: Some(8),
                },
                DisplayRowSnapshot {
                    row: 1,
                    y: 16,
                    height: 16,
                    end_x: 72,
                    end_col: 9,
                    start_buffer_pos: Some(9),
                    end_buffer_pos: Some(17),
                },
                DisplayRowSnapshot {
                    row: 2,
                    y: 32,
                    height: 16,
                    end_x: 80,
                    end_col: 10,
                    start_buffer_pos: Some(18),
                    end_buffer_pos: Some(27),
                },
            ],
            ..WindowDisplaySnapshot::default()
        };
        let mut display = WindowDisplayState::default();

        display.begin_output_pass();
        display.install_logical_cursor(Some(WindowCursorPos::from_snapshot(&cursor)));
        display.commit_output_cursor_from_display_snapshot(&snapshot);
        display.apply_physical_cursor_snapshot(Some(cursor.clone()));
        display.commit_completed_redisplay();

        assert_eq!(
            display.cursor,
            Some(WindowCursorPos {
                x: 18,
                y: 16,
                row: 1,
                col: 2,
            })
        );
        assert_eq!(
            display.output_cursor,
            Some(WindowCursorPos {
                x: 80,
                y: 32,
                row: 2,
                col: 10,
            })
        );
        assert_eq!(display.last_cursor_vpos, 2);
        assert_eq!(display.phys_cursor, Some(cursor));
    }

    #[test]
    fn replace_display_snapshots_preserves_output_cursor_for_omitted_windows() {
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let wid = mgr.get(fid).unwrap().selected_window;
        let cursor = WindowCursorSnapshot {
            kind: WindowCursorKind::FilledBox,
            x: 18,
            y: 36,
            width: 8,
            height: 16,
            ascent: 12,
            row: 2,
            col: 6,
        };
        let cursor_pos = WindowCursorPos::from_snapshot(&cursor);

        let frame = mgr.get_mut(fid).unwrap();
        let display = frame
            .find_window_mut(wid)
            .and_then(|window| window.display_mut())
            .expect("window display state");
        display.install_logical_cursor(Some(cursor_pos));
        display.commit_output_cursor_from_cursor();
        display.apply_physical_cursor_snapshot(Some(cursor));
        display.commit_completed_redisplay();

        frame.replace_display_snapshots(Vec::new());

        let display = frame
            .find_window(wid)
            .and_then(|window| window.display())
            .expect("window display state");
        assert_eq!(display.cursor, None);
        assert_eq!(display.output_cursor, Some(cursor_pos));
        assert_eq!(display.phys_cursor, None);
        assert_eq!(display.phys_cursor_type, WindowCursorKind::NoCursor);
        assert!(!display.phys_cursor_on_p);
        assert_eq!(display.last_cursor_vpos, cursor_pos.row);
    }

    #[test]
    fn frame_resize_pixelwise_reserves_tab_bar_height_above_root_window_tree() {
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 800, 600, BufferId(1));
        let frame = mgr.get_mut(fid).unwrap();
        frame.char_width = 10.0;
        frame.char_height = 20.0;
        frame
            .parameters
            .insert("tab-bar-lines".to_string(), Value::fixnum(1));

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
        assert_eq!(frame.parameters.get("height"), Some(&Value::fixnum(12)));
    }

    #[test]
    fn grow_and_shrink_mini_window_adjusts_bounds() {
        crate::test_utils::init_test_tracing();
        let mut mgr = FrameManager::new();
        let fid = mgr.create_frame("F1", 80, 24, BufferId(1));
        // Treat the frame as a TTY-style frame where 1 px == 1 character row.
        // char_height=1.0 means `grow_mini_window` grows by 1 row per delta,
        // and max-mini-window-height (25% of 24 rows = 6 rows) is comfortably
        // above the 1-row minimum.
        // Re-initialize the minibuffer to exactly 1 row so that it starts at
        // the minimum height and has room to grow.
        {
            let frame = mgr.get_mut(fid).unwrap();
            frame.char_height = 1.0;
            frame.char_width = 1.0;
            if let Some(mini) = frame.minibuffer_leaf.as_mut() {
                let mut b = *mini.bounds();
                b.height = 1.0;
                mini.set_bounds(b);
            }
            frame.sync_window_area_bounds();
        }
        let frame = mgr.get(fid).unwrap();
        let initial_mini_h = frame.minibuffer_leaf.as_ref().unwrap().bounds().height;

        mgr.get_mut(fid).unwrap().grow_mini_window(3);
        let grown_h = mgr
            .get(fid)
            .unwrap()
            .minibuffer_leaf
            .as_ref()
            .unwrap()
            .bounds()
            .height;
        assert!(
            grown_h > initial_mini_h,
            "minibuffer should grow: initial={initial_mini_h} grown={grown_h}"
        );

        mgr.get_mut(fid).unwrap().shrink_mini_window();
        let shrunk_h = mgr
            .get(fid)
            .unwrap()
            .minibuffer_leaf
            .as_ref()
            .unwrap()
            .bounds()
            .height;
        assert!(
            shrunk_h < grown_h,
            "minibuffer should shrink: grown={grown_h} shrunk={shrunk_h}"
        );
    }
}
