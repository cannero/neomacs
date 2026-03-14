//! Buffer and BufferManager — the core text container for the Elisp VM.
//!
//! A `Buffer` wraps a [`BufferText`] with Emacs-style point, mark, narrowing,
//! markers, and buffer-local variables.  `BufferManager` owns all live buffers
//! and tracks the current buffer.

use std::collections::HashMap;

use super::buffer_text::BufferText;
use super::overlay::OverlayList;
use super::text_props::TextPropertyTable;
use super::undo::{UndoList, UndoRecord};
use crate::emacs_core::syntax::SyntaxTable;
use crate::emacs_core::value::Value;
use crate::gc::GcTrace;

// ---------------------------------------------------------------------------
// BufferId
// ---------------------------------------------------------------------------

/// Opaque, cheaply-copyable identifier for a buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BufferId(pub u64);

// ---------------------------------------------------------------------------
// InsertionType
// ---------------------------------------------------------------------------

/// Controls whether a marker advances when text is inserted exactly at its
/// position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertionType {
    /// Marker stays before the new text (does NOT advance).
    Before,
    /// Marker moves after the new text (advances).
    After,
}

// ---------------------------------------------------------------------------
// MarkerEntry
// ---------------------------------------------------------------------------

/// A tracked position inside a buffer.
#[derive(Clone, Debug)]
pub struct MarkerEntry {
    pub id: u64,
    pub byte_pos: usize,
    pub insertion_type: InsertionType,
}

// ---------------------------------------------------------------------------
// Buffer
// ---------------------------------------------------------------------------

/// A single text buffer with point, mark, narrowing, markers, and local vars.
#[derive(Clone)]
pub struct Buffer {
    /// Unique identifier.
    pub id: BufferId,
    /// Buffer name (e.g. `"*scratch*"`).
    pub name: String,
    /// Base buffer when this is an indirect buffer.
    pub base_buffer: Option<BufferId>,
    /// The underlying text storage.
    pub text: BufferText,
    /// Point — the current cursor byte position.
    pub pt: usize,
    /// Mark — optional byte position for region operations.
    pub mark: Option<usize>,
    /// Beginning of accessible (narrowed) portion (byte pos, inclusive).
    pub begv: usize,
    /// End of accessible (narrowed) portion (byte pos, exclusive).
    pub zv: usize,
    /// Whether the buffer has been modified since last save.
    pub modified: bool,
    /// Monotonic buffer modification tick.
    pub modified_tick: i64,
    /// Monotonic character-content modification tick.
    pub chars_modified_tick: i64,
    /// If true, insertions/deletions are forbidden.
    pub read_only: bool,
    /// Multi-byte encoding flag.  Always `true` for now.
    pub multibyte: bool,
    /// Associated file path, if any.
    pub file_name: Option<String>,
    /// Active markers that track positions across edits.
    pub markers: Vec<MarkerEntry>,
    /// Buffer-local variables (name -> Lisp value).
    pub properties: HashMap<String, Value>,
    /// Text properties attached to ranges of text.
    pub text_props: TextPropertyTable,
    /// Overlays attached to the buffer.
    pub overlays: OverlayList,
    /// Syntax table for character classification.
    pub syntax_table: SyntaxTable,
    /// Undo history.
    pub undo_list: UndoList,
}

impl Buffer {
    // -- Construction --------------------------------------------------------

    /// Create a new, empty buffer.
    pub fn new(id: BufferId, name: String) -> Self {
        let mut properties = HashMap::new();
        properties.insert("buffer-read-only".to_string(), Value::Nil);
        properties.insert("buffer-undo-list".to_string(), Value::Nil);
        properties.insert("major-mode".to_string(), Value::symbol("fundamental-mode"));
        properties.insert("mode-name".to_string(), Value::string("Fundamental"));

        Self {
            id,
            name,
            base_buffer: None,
            text: BufferText::new(),
            pt: 0,
            mark: None,
            begv: 0,
            zv: 0,
            modified: false,
            modified_tick: 1,
            chars_modified_tick: 1,
            read_only: false,
            multibyte: true,
            file_name: None,
            markers: Vec::new(),
            properties,
            text_props: TextPropertyTable::new(),
            overlays: OverlayList::new(),
            syntax_table: SyntaxTable::new_standard(),
            undo_list: UndoList::new(),
        }
    }

    // -- Point queries -------------------------------------------------------

    /// Current point as a byte position.
    pub fn point(&self) -> usize {
        self.pt
    }

    /// Current point converted to a character position.
    pub fn point_char(&self) -> usize {
        self.text.byte_to_char(self.pt)
    }

    /// Beginning of the accessible portion (byte position).
    pub fn point_min(&self) -> usize {
        self.begv
    }

    /// End of the accessible portion (byte position).
    pub fn point_max(&self) -> usize {
        self.zv
    }

    // -- Point movement ------------------------------------------------------

    /// Set point, clamping to the accessible region `[begv, zv]`.
    pub fn goto_char(&mut self, pos: usize) {
        self.pt = pos.clamp(self.begv, self.zv);
    }

    // -- Editing -------------------------------------------------------------

    /// Insert `text` at point, advancing point past the inserted text.
    ///
    /// Markers at the insertion site move according to their `InsertionType`.
    pub fn insert(&mut self, text: &str) {
        let insert_pos = self.pt;
        let len = text.len();
        if len == 0 {
            return;
        }

        // Record undo before modifying.
        self.undo_list.prepare_change(insert_pos, self.pt);
        self.undo_list.record_insert(insert_pos, len);

        self.text.insert_str(insert_pos, text);

        // Advance point past inserted text.
        self.pt += len;

        // Adjust zv (end of accessible region grows with buffer).
        self.zv += len;

        // Adjust mark.
        if let Some(m) = self.mark {
            if m > insert_pos {
                self.mark = Some(m + len);
            }
        }

        // Adjust markers.
        for marker in &mut self.markers {
            if marker.byte_pos > insert_pos {
                marker.byte_pos += len;
            } else if marker.byte_pos == insert_pos {
                // Insertion at exact marker position: behaviour depends on type.
                if marker.insertion_type == InsertionType::After {
                    marker.byte_pos += len;
                }
                // InsertionType::Before => marker stays put.
            }
        }

        // Adjust text properties and overlays.
        self.text_props.adjust_for_insert(insert_pos, len);
        self.overlays.adjust_for_insert(insert_pos, len);

        self.modified = true;
        self.modified_tick += 1;
        self.chars_modified_tick += 1;
    }

    /// Delete the byte range `[start, end)`.
    ///
    /// Adjusts point, mark, markers, and the narrowing boundary.
    pub fn delete_region(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let len = end - start;

        // Record undo: save the deleted text for restoration.
        let deleted_text = self.text.text_range(start, end);
        self.undo_list.prepare_change(start, self.pt);
        self.undo_list.record_delete(start, &deleted_text);

        self.text.delete_range(start, end);

        // Adjust point.
        if self.pt > end {
            self.pt -= len;
        } else if self.pt > start {
            self.pt = start;
        }

        // Adjust mark.
        if let Some(m) = self.mark {
            if m > end {
                self.mark = Some(m - len);
            } else if m > start {
                self.mark = Some(start);
            }
        }

        // Adjust markers.
        for marker in &mut self.markers {
            if marker.byte_pos > end {
                marker.byte_pos -= len;
            } else if marker.byte_pos > start {
                marker.byte_pos = start;
            }
        }

        // Adjust zv.
        if self.zv > end {
            self.zv -= len;
        } else if self.zv > start {
            self.zv = start;
        }

        // Adjust text properties and overlays.
        self.text_props.adjust_for_delete(start, end);
        self.overlays.adjust_for_delete(start, end);

        self.modified = true;
        self.modified_tick += 1;
        self.chars_modified_tick += 1;
    }

    // -- Text queries --------------------------------------------------------

    /// Return a `String` copy of the byte range `[start, end)`.
    pub fn buffer_substring(&self, start: usize, end: usize) -> String {
        self.text.text_range(start, end)
    }

    /// Return the entire accessible portion of the buffer as a `String`.
    pub fn buffer_string(&self) -> String {
        self.text.text_range(self.begv, self.zv)
    }

    /// Byte-length of the accessible portion.
    pub fn buffer_size(&self) -> usize {
        self.zv - self.begv
    }

    /// Character at byte position `pos`, or `None` if out of range.
    pub fn char_after(&self, pos: usize) -> Option<char> {
        if pos >= self.text.len() {
            return None;
        }
        self.text.char_at(pos)
    }

    /// Character immediately before byte position `pos`, or `None`.
    pub fn char_before(&self, pos: usize) -> Option<char> {
        if pos == 0 || pos > self.text.len() {
            return None;
        }
        // Walk backwards to find the start of the previous UTF-8 character.
        // The gap buffer stores valid UTF-8, so we can probe up to 4 bytes back.
        let text = self.text.to_string();
        let bytes = text.as_bytes();
        let mut back = 1;
        while back <= 4 && back <= pos {
            let idx = pos - back;
            if (bytes[idx] & 0xC0) != 0x80 {
                // Found a leading byte.
                return text[idx..pos].chars().next();
            }
            back += 1;
        }
        None
    }

    // -- Narrowing -----------------------------------------------------------

    /// Restrict the accessible portion to `[start, end)`.
    pub fn narrow_to_region(&mut self, start: usize, end: usize) {
        let total = self.text.len();
        let s = start.min(total);
        let e = end.clamp(s, total);
        self.begv = s;
        self.zv = e;
        // Clamp point into the new accessible region.
        self.pt = self.pt.clamp(self.begv, self.zv);
    }

    /// Remove narrowing — make the entire buffer accessible again.
    pub fn widen(&mut self) {
        self.begv = 0;
        self.zv = self.text.len();
    }

    // -- Mark ----------------------------------------------------------------

    /// Set the mark to `pos`.
    pub fn set_mark(&mut self, pos: usize) {
        self.mark = Some(pos);
    }

    /// Return the mark, if set.
    pub fn mark(&self) -> Option<usize> {
        self.mark
    }

    // -- Modified flag -------------------------------------------------------

    pub fn is_modified(&self) -> bool {
        self.modified
    }

    pub fn set_modified(&mut self, flag: bool) {
        self.modified = flag;
    }

    // -- Buffer-local variables ----------------------------------------------

    pub fn set_buffer_local(&mut self, name: &str, value: Value) {
        self.properties.insert(name.to_string(), value);
    }

    pub fn get_buffer_local(&self, name: &str) -> Option<&Value> {
        self.properties.get(name)
    }

    pub fn buffer_local_value(&self, name: &str) -> Option<Value> {
        match name {
            "buffer-undo-list" => match self.properties.get(name).copied() {
                Some(Value::True) => Some(Value::True),
                _ => Some(self.undo_list.to_value()),
            },
            _ => self.properties.get(name).copied(),
        }
    }
}

// ---------------------------------------------------------------------------
// BufferManager
// ---------------------------------------------------------------------------

/// Owns every live buffer, tracks the current buffer, and hands out ids.
#[derive(Clone)]
pub struct BufferManager {
    buffers: HashMap<BufferId, Buffer>,
    current: Option<BufferId>,
    next_id: u64,
    next_marker_id: u64,
    dead_buffer_last_names: HashMap<BufferId, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UndoExecutionResult {
    pub had_any_records: bool,
    pub had_boundary: bool,
    pub applied_any: bool,
    pub skipped_apply: bool,
}

impl BufferManager {
    /// Create a new `BufferManager` pre-populated with a `*scratch*` buffer.
    pub fn new() -> Self {
        let mut mgr = Self {
            buffers: HashMap::new(),
            current: None,
            next_id: 1,
            next_marker_id: 1,
            dead_buffer_last_names: HashMap::new(),
        };
        let scratch = mgr.create_buffer("*scratch*");
        mgr.current = Some(scratch);
        mgr
    }

    /// Allocate a new buffer with the given name and return its id.
    pub fn create_buffer(&mut self, name: &str) -> BufferId {
        let id = BufferId(self.next_id);
        self.next_id += 1;
        let buf = Buffer::new(id, name.to_string());
        self.buffers.insert(id, buf);
        id
    }

    /// Allocate a new indirect buffer that shares its root base buffer's text.
    ///
    /// This mirrors GNU Emacs's `make-indirect-buffer` C boundary:
    /// indirect buffers share the root base buffer's text object, and double
    /// indirection is flattened so every indirect points at the same root.
    pub fn create_indirect_buffer(
        &mut self,
        base_id: BufferId,
        name: &str,
        clone: bool,
    ) -> Option<BufferId> {
        if name.is_empty() || self.find_buffer_by_name(name).is_some() {
            return None;
        }

        let root_id = self.shared_text_root_id(base_id)?;
        let root = self.buffers.get(&root_id)?.clone();
        let shared_text = self.buffers.get(&root_id)?.text.shared_clone();

        let id = BufferId(self.next_id);
        self.next_id += 1;

        let mut indirect = if clone {
            let mut cloned = root.clone();
            cloned.id = id;
            cloned.name = name.to_string();
            cloned
        } else {
            Buffer::new(id, name.to_string())
        };

        indirect.base_buffer = Some(root_id);
        indirect.text = shared_text;
        indirect.narrow_to_region(root.begv, root.zv);
        indirect.goto_char(root.pt);
        indirect.multibyte = root.multibyte;
        indirect.modified = root.modified;
        indirect.modified_tick = root.modified_tick;
        indirect.chars_modified_tick = root.chars_modified_tick;
        indirect.file_name = None;
        indirect.text_props = root.text_props.clone();
        if !clone {
            indirect.overlays = OverlayList::new();
            indirect.mark = None;
            indirect.markers.clear();
            indirect.undo_list = root.undo_list.clone();
        }

        self.buffers.insert(id, indirect);
        Some(id)
    }

    /// Immutable access to a buffer by id.
    pub fn get(&self, id: BufferId) -> Option<&Buffer> {
        self.buffers.get(&id)
    }

    /// Mutable access to a buffer by id.
    pub fn get_mut(&mut self, id: BufferId) -> Option<&mut Buffer> {
        self.buffers.get_mut(&id)
    }

    /// Immutable access to the current buffer.
    pub fn current_buffer(&self) -> Option<&Buffer> {
        self.current.and_then(|id| self.buffers.get(&id))
    }

    /// Mutable access to the current buffer.
    pub fn current_buffer_mut(&mut self) -> Option<&mut Buffer> {
        self.current.and_then(|id| self.buffers.get_mut(&id))
    }

    /// Return the current buffer id.
    pub fn current_buffer_id(&self) -> Option<BufferId> {
        self.current
    }

    /// Switch the current buffer.
    pub fn set_current(&mut self, id: BufferId) {
        if self.buffers.contains_key(&id) {
            self.current = Some(id);
        }
    }

    /// Find a buffer by name, returning its id if it exists.
    pub fn find_buffer_by_name(&self, name: &str) -> Option<BufferId> {
        self.buffers.values().find(|b| b.name == name).map(|b| b.id)
    }

    /// Find a killed buffer by its last known name.
    pub fn find_dead_buffer_by_name(&self, name: &str) -> Option<BufferId> {
        self.dead_buffer_last_names
            .iter()
            .find_map(|(id, last_name)| (last_name == name).then_some(*id))
    }

    /// Remove a buffer.  Returns `true` if the buffer existed.
    ///
    /// If the killed buffer was current, `current` is set to `None`.
    pub fn kill_buffer(&mut self, id: BufferId) -> bool {
        if let Some(buf) = self.buffers.remove(&id) {
            self.dead_buffer_last_names.insert(id, buf.name);
            if self.current == Some(id) {
                self.current = None;
            }
            true
        } else {
            false
        }
    }

    /// Return the last known name for a dead buffer id, if available.
    pub fn dead_buffer_last_name(&self, id: BufferId) -> Option<&str> {
        self.dead_buffer_last_names.get(&id).map(|s| s.as_str())
    }

    /// List all live buffer ids (arbitrary order).
    pub fn buffer_list(&self) -> Vec<BufferId> {
        self.buffers.keys().copied().collect()
    }

    fn shared_text_root_id(&self, id: BufferId) -> Option<BufferId> {
        let buf = self.buffers.get(&id)?;
        Some(buf.base_buffer.unwrap_or(buf.id))
    }

    fn buffers_sharing_root_ids(&self, root_id: BufferId) -> Vec<BufferId> {
        self.buffers
            .values()
            .filter_map(|buf| (buf.base_buffer.unwrap_or(buf.id) == root_id).then_some(buf.id))
            .collect()
    }

    fn adjust_shared_insert_metadata(buf: &mut Buffer, insert_pos: usize, len: usize) {
        if len == 0 {
            return;
        }

        if buf.pt > insert_pos {
            buf.pt += len;
        }
        if buf.begv > insert_pos {
            buf.begv += len;
        }
        if buf.zv >= insert_pos {
            buf.zv += len;
        }
        if let Some(mark) = buf.mark
            && mark > insert_pos
        {
            buf.mark = Some(mark + len);
        }
        for marker in &mut buf.markers {
            if marker.byte_pos > insert_pos {
                marker.byte_pos += len;
            } else if marker.byte_pos == insert_pos && marker.insertion_type == InsertionType::After
            {
                marker.byte_pos += len;
            }
        }
        buf.text_props.adjust_for_insert(insert_pos, len);
        buf.overlays.adjust_for_insert(insert_pos, len);
        buf.modified = true;
        buf.modified_tick += 1;
        buf.chars_modified_tick += 1;
    }

    fn adjust_shared_delete_metadata(buf: &mut Buffer, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let len = end - start;

        if buf.pt > end {
            buf.pt -= len;
        } else if buf.pt > start {
            buf.pt = start;
        }

        if buf.begv > end {
            buf.begv -= len;
        } else if buf.begv > start {
            buf.begv = start;
        }

        if buf.zv > end {
            buf.zv -= len;
        } else if buf.zv > start {
            buf.zv = start;
        }

        if let Some(mark) = buf.mark {
            if mark > end {
                buf.mark = Some(mark - len);
            } else if mark > start {
                buf.mark = Some(start);
            }
        }

        for marker in &mut buf.markers {
            if marker.byte_pos > end {
                marker.byte_pos -= len;
            } else if marker.byte_pos > start {
                marker.byte_pos = start;
            }
        }

        buf.text_props.adjust_for_delete(start, end);
        buf.overlays.adjust_for_delete(start, end);
        buf.modified = true;
        buf.modified_tick += 1;
        buf.chars_modified_tick += 1;
    }

    fn sync_shared_undo_lists(&mut self, root_id: BufferId, source_id: BufferId) -> Option<()> {
        let source_undo = self.buffers.get(&source_id)?.undo_list.clone();
        for shared_id in self.buffers_sharing_root_ids(root_id) {
            if shared_id == source_id {
                continue;
            }
            self.buffers.get_mut(&shared_id)?.undo_list = source_undo.clone();
        }
        Some(())
    }

    /// Centralized structural text mutations.
    ///
    /// Indirect buffers will eventually share a single text object.  When that
    /// happens, sibling buffers must be updated from one place instead of every
    /// ad hoc `buf.insert` / `buf.delete_region` call site in the tree.
    pub fn goto_buffer_byte(&mut self, id: BufferId, pos: usize) -> Option<usize> {
        let buf = self.buffers.get_mut(&id)?;
        buf.goto_char(pos);
        Some(buf.point())
    }

    pub fn insert_into_buffer(&mut self, id: BufferId, text: &str) -> Option<()> {
        let len = text.len();
        if len == 0 {
            return Some(());
        }

        let root_id = self.shared_text_root_id(id)?;
        let shared_ids = self.buffers_sharing_root_ids(root_id);
        let insert_pos = self.buffers.get(&id)?.pt;

        self.buffers.get_mut(&id)?.insert(text);

        for sibling_id in shared_ids {
            if sibling_id == id {
                continue;
            }
            let sibling = self.buffers.get_mut(&sibling_id)?;
            Self::adjust_shared_insert_metadata(sibling, insert_pos, len);
        }
        self.sync_shared_undo_lists(root_id, id)?;
        Some(())
    }

    pub fn insert_into_buffer_before_markers(&mut self, id: BufferId, text: &str) -> Option<()> {
        let byte_len = text.len();
        if byte_len == 0 {
            return Some(());
        }
        let old_pt = self.buffers.get(&id)?.pt;
        self.insert_into_buffer(id, text)?;
        let buf = self.buffers.get_mut(&id)?;
        for marker in &mut buf.markers {
            if marker.byte_pos == old_pt {
                marker.byte_pos += byte_len;
            }
        }
        Some(())
    }

    pub fn delete_buffer_region(&mut self, id: BufferId, start: usize, end: usize) -> Option<()> {
        if start >= end {
            return Some(());
        }

        let root_id = self.shared_text_root_id(id)?;
        let shared_ids = self.buffers_sharing_root_ids(root_id);
        self.buffers.get_mut(&id)?.delete_region(start, end);

        for sibling_id in shared_ids {
            if sibling_id == id {
                continue;
            }
            let sibling = self.buffers.get_mut(&sibling_id)?;
            Self::adjust_shared_delete_metadata(sibling, start, end);
        }
        self.sync_shared_undo_lists(root_id, id)?;
        Some(())
    }

    pub fn delete_all_buffer_overlays(&mut self, id: BufferId) -> Option<()> {
        let buf = self.buffers.get_mut(&id)?;
        let ids = buf.overlays.overlays_in(buf.point_min(), buf.point_max());
        for ov_id in ids {
            buf.overlays.delete_overlay(ov_id);
        }
        Some(())
    }

    pub fn delete_buffer_overlay(&mut self, id: BufferId, overlay_id: u64) -> Option<()> {
        self.buffers
            .get_mut(&id)?
            .overlays
            .delete_overlay(overlay_id);
        Some(())
    }

    pub fn put_buffer_overlay_property(
        &mut self,
        id: BufferId,
        overlay_id: u64,
        name: &str,
        value: Value,
    ) -> Option<()> {
        self.buffers
            .get_mut(&id)?
            .overlays
            .overlay_put(overlay_id, name, value);
        Some(())
    }

    pub fn narrow_buffer_to_region(
        &mut self,
        id: BufferId,
        start: usize,
        end: usize,
    ) -> Option<()> {
        self.buffers.get_mut(&id)?.narrow_to_region(start, end);
        Some(())
    }

    pub fn widen_buffer(&mut self, id: BufferId) -> Option<()> {
        self.buffers.get_mut(&id)?.widen();
        Some(())
    }

    pub fn replace_buffer_contents(&mut self, id: BufferId, text: &str) -> Option<()> {
        let len = self.buffers.get(&id)?.text.len();
        if len > 0 {
            self.delete_buffer_region(id, 0, len)?;
        }
        {
            let buf = self.buffers.get_mut(&id)?;
            buf.widen();
            buf.goto_char(0);
        }
        if !text.is_empty() {
            self.insert_into_buffer(id, text)?;
            self.goto_buffer_byte(id, 0)?;
        }
        Some(())
    }

    pub fn clear_buffer_local_properties(&mut self, id: BufferId) -> Option<()> {
        let buf = self.buffers.get_mut(&id)?;
        buf.properties.clear();
        buf.properties
            .insert("buffer-read-only".to_string(), Value::Nil);
        Some(())
    }

    pub fn put_buffer_text_property(
        &mut self,
        id: BufferId,
        start: usize,
        end: usize,
        name: &str,
        value: Value,
    ) -> Option<()> {
        let root_id = self.shared_text_root_id(id)?;
        let shared_ids = self.buffers_sharing_root_ids(root_id);
        for shared_id in shared_ids {
            self.buffers
                .get_mut(&shared_id)?
                .text_props
                .put_property(start, end, name, value);
        }
        Some(())
    }

    pub fn append_buffer_text_properties(
        &mut self,
        id: BufferId,
        table: &TextPropertyTable,
        byte_offset: usize,
    ) -> Option<()> {
        let root_id = self.shared_text_root_id(id)?;
        let shared_ids = self.buffers_sharing_root_ids(root_id);
        for shared_id in shared_ids {
            self.buffers
                .get_mut(&shared_id)?
                .text_props
                .append_shifted(table, byte_offset);
        }
        Some(())
    }

    pub fn remove_buffer_text_property(
        &mut self,
        id: BufferId,
        start: usize,
        end: usize,
        name: &str,
    ) -> Option<bool> {
        let root_id = self.shared_text_root_id(id)?;
        let shared_ids = self.buffers_sharing_root_ids(root_id);
        let mut removed_any = false;
        for shared_id in shared_ids {
            if self
                .buffers
                .get_mut(&shared_id)?
                .text_props
                .remove_property(start, end, name)
            {
                removed_any = true;
            }
        }
        Some(removed_any)
    }

    pub fn clear_buffer_text_properties(
        &mut self,
        id: BufferId,
        start: usize,
        end: usize,
    ) -> Option<()> {
        let root_id = self.shared_text_root_id(id)?;
        let shared_ids = self.buffers_sharing_root_ids(root_id);
        for shared_id in shared_ids {
            self.buffers
                .get_mut(&shared_id)?
                .text_props
                .remove_all_properties(start, end);
        }
        Some(())
    }

    pub fn set_buffer_multibyte_flag(&mut self, id: BufferId, flag: bool) -> Option<()> {
        self.buffers.get_mut(&id)?.multibyte = flag;
        Some(())
    }

    pub fn set_buffer_modified_flag(&mut self, id: BufferId, flag: bool) -> Option<()> {
        self.buffers.get_mut(&id)?.set_modified(flag);
        Some(())
    }

    pub fn set_buffer_file_name(&mut self, id: BufferId, file_name: Option<String>) -> Option<()> {
        self.buffers.get_mut(&id)?.file_name = file_name;
        Some(())
    }

    pub fn set_buffer_name(&mut self, id: BufferId, name: String) -> Option<()> {
        self.buffers.get_mut(&id)?.name = name;
        Some(())
    }

    pub fn set_buffer_mark(&mut self, id: BufferId, pos: usize) -> Option<()> {
        self.buffers.get_mut(&id)?.set_mark(pos);
        Some(())
    }

    pub fn clear_buffer_mark(&mut self, id: BufferId) -> Option<()> {
        self.buffers.get_mut(&id)?.mark = None;
        Some(())
    }

    pub fn set_buffer_local_property(
        &mut self,
        id: BufferId,
        name: &str,
        value: Value,
    ) -> Option<()> {
        self.buffers.get_mut(&id)?.set_buffer_local(name, value);
        Some(())
    }

    pub fn remove_buffer_local_property(
        &mut self,
        id: BufferId,
        name: &str,
    ) -> Option<Option<Value>> {
        Some(self.buffers.get_mut(&id)?.properties.remove(name))
    }

    pub fn add_undo_boundary(&mut self, id: BufferId) -> Option<()> {
        let root_id = self.shared_text_root_id(id)?;
        self.buffers.get_mut(&id)?.undo_list.boundary();
        self.sync_shared_undo_lists(root_id, id)?;
        Some(())
    }

    pub fn restore_buffer_restriction(
        &mut self,
        id: BufferId,
        begv: usize,
        zv: usize,
    ) -> Option<()> {
        let buf = self.buffers.get_mut(&id)?;
        buf.narrow_to_region(begv, zv);
        Some(())
    }

    pub fn configure_buffer_undo_list(&mut self, id: BufferId, value: Value) -> Option<()> {
        let root_id = self.shared_text_root_id(id)?;
        {
            let buf = self.buffers.get_mut(&id)?;
            match value {
                Value::True => {
                    buf.undo_list.set_enabled(false);
                    buf.set_buffer_local("buffer-undo-list", Value::True);
                }
                Value::Nil => {
                    buf.undo_list.set_enabled(true);
                    buf.undo_list.clear();
                    buf.set_buffer_local("buffer-undo-list", Value::Nil);
                }
                other => {
                    buf.undo_list.set_enabled(true);
                    buf.set_buffer_local("buffer-undo-list", other);
                }
            }
        }
        self.sync_shared_undo_lists(root_id, id)?;
        Some(())
    }

    pub fn undo_buffer(&mut self, id: BufferId, mut count: i64) -> Option<UndoExecutionResult> {
        let (had_any_records, had_boundary, previous_undoing, groups) = {
            let buffer = self.buffers.get_mut(&id)?;

            let had_any_records = !buffer.undo_list.is_empty();
            let had_boundary = buffer.undo_list.contains_boundary();
            let had_trailing_boundary = buffer.undo_list.has_trailing_boundary();

            if count <= 0 && had_boundary {
                return Some(UndoExecutionResult {
                    had_any_records,
                    had_boundary,
                    applied_any: false,
                    skipped_apply: true,
                });
            }
            if count <= 0 {
                count = 1;
            }

            let previous_undoing = buffer.undo_list.undoing;
            buffer.undo_list.undoing = true;
            let groups_to_undo = if had_trailing_boundary {
                count as usize
            } else {
                (count as usize).saturating_add(1)
            };

            let mut groups = Vec::new();
            for _ in 0..groups_to_undo {
                let group = buffer.undo_list.pop_undo_group();
                if group.is_empty() {
                    break;
                }
                groups.push(group);
            }

            (had_any_records, had_boundary, previous_undoing, groups)
        };

        let mut applied_any = false;
        for group in groups {
            applied_any = true;
            for record in group {
                match record {
                    UndoRecord::Insert { pos, len } => {
                        let end = self
                            .buffers
                            .get(&id)
                            .map(|buffer| pos.saturating_add(len).min(buffer.text.len()))?;
                        self.delete_buffer_region(id, pos.min(end), end)?;
                    }
                    UndoRecord::Delete { pos, text } => {
                        let clamped = self
                            .buffers
                            .get(&id)
                            .map(|buffer| pos.min(buffer.text.len()))?;
                        self.goto_buffer_byte(id, clamped)?;
                        self.insert_into_buffer(id, &text)?;
                    }
                    UndoRecord::CursorMove { pos } => {
                        let clamped = self
                            .buffers
                            .get(&id)
                            .map(|buffer| pos.min(buffer.text.len()))?;
                        self.goto_buffer_byte(id, clamped)?;
                    }
                    UndoRecord::PropertyChange { .. }
                    | UndoRecord::FirstChange { .. }
                    | UndoRecord::Boundary => {}
                }
            }
        }

        self.buffers.get_mut(&id)?.undo_list.undoing = previous_undoing;
        let root_id = self.shared_text_root_id(id)?;
        self.sync_shared_undo_lists(root_id, id)?;
        Some(UndoExecutionResult {
            had_any_records,
            had_boundary,
            applied_any,
            skipped_apply: false,
        })
    }

    /// Generate a unique buffer name.  If `base` is not taken, returns it
    /// unchanged; otherwise appends `<2>`, `<3>`, ... until a free name is
    /// found.
    pub fn generate_new_buffer_name(&self, base: &str) -> String {
        if self.find_buffer_by_name(base).is_none() {
            return base.to_string();
        }
        let mut n = 2u64;
        loop {
            let candidate = format!("{}<{}>", base, n);
            if self.find_buffer_by_name(&candidate).is_none() {
                return candidate;
            }
            n += 1;
        }
    }

    /// Allocate a unique marker id without associating it with a buffer.
    pub fn allocate_marker_id(&mut self) -> u64 {
        let id = self.next_marker_id;
        self.next_marker_id += 1;
        id
    }

    /// Create a marker in `buffer_id` at byte position `pos` with the given
    /// insertion type.  Returns the new marker's id.
    pub fn create_marker(
        &mut self,
        buffer_id: BufferId,
        pos: usize,
        insertion_type: InsertionType,
    ) -> u64 {
        let marker_id = self.next_marker_id;
        self.next_marker_id += 1;
        let _ = self.register_marker_id(buffer_id, marker_id, pos, insertion_type);
        marker_id
    }

    /// Register an existing marker id in `buffer_id` at byte position `pos`.
    pub fn register_marker_id(
        &mut self,
        buffer_id: BufferId,
        marker_id: u64,
        pos: usize,
        insertion_type: InsertionType,
    ) -> Option<()> {
        let buf = self.buffers.get_mut(&buffer_id)?;
        let clamped = pos.min(buf.text.len());
        buf.markers.retain(|marker| marker.id != marker_id);
        buf.markers.push(MarkerEntry {
            id: marker_id,
            byte_pos: clamped,
            insertion_type,
        });
        Some(())
    }

    /// Query the current byte position of a marker.
    pub fn marker_position(&self, buffer_id: BufferId, marker_id: u64) -> Option<usize> {
        self.buffers.get(&buffer_id).and_then(|buf| {
            buf.markers
                .iter()
                .find(|m| m.id == marker_id)
                .map(|m| m.byte_pos)
        })
    }

    /// Remove a marker registration from any live buffer.
    pub fn remove_marker(&mut self, marker_id: u64) {
        for buf in self.buffers.values_mut() {
            buf.markers.retain(|marker| marker.id != marker_id);
        }
    }

    // pdump accessors
    pub(crate) fn dump_buffers(&self) -> &HashMap<BufferId, Buffer> {
        &self.buffers
    }
    pub(crate) fn dump_current(&self) -> Option<BufferId> {
        self.current
    }
    pub(crate) fn dump_next_id(&self) -> u64 {
        self.next_id
    }
    pub(crate) fn dump_next_marker_id(&self) -> u64 {
        self.next_marker_id
    }
    pub(crate) fn from_dump(
        buffers: HashMap<BufferId, Buffer>,
        current: Option<BufferId>,
        next_id: u64,
        next_marker_id: u64,
    ) -> Self {
        Self {
            buffers,
            current,
            next_id,
            next_marker_id,
            dead_buffer_last_names: HashMap::new(),
        }
    }
}

impl Default for BufferManager {
    fn default() -> Self {
        Self::new()
    }
}

impl GcTrace for BufferManager {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        for buffer in self.buffers.values() {
            for value in buffer.properties.values() {
                roots.push(*value);
            }
            buffer.text_props.trace_roots(roots);
            buffer.overlays.trace_roots(roots);
            buffer.undo_list.trace_roots(roots);
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Helper: create a buffer with some text and correct zv.
    // -----------------------------------------------------------------------
    fn buf_with_text(text: &str) -> Buffer {
        let mut buf = Buffer::new(BufferId(1), "test".into());
        buf.text = BufferText::from_str(text);
        buf.widen();
        buf
    }

    // -----------------------------------------------------------------------
    // Buffer creation & naming
    // -----------------------------------------------------------------------

    #[test]
    fn new_buffer_is_empty() {
        let buf = Buffer::new(BufferId(1), "*scratch*".into());
        assert_eq!(buf.name, "*scratch*");
        assert_eq!(buf.point(), 0);
        assert_eq!(buf.point_min(), 0);
        assert_eq!(buf.point_max(), 0);
        assert_eq!(buf.buffer_size(), 0);
        assert!(!buf.is_modified());
        assert!(!buf.read_only);
        assert!(buf.multibyte);
        assert!(buf.file_name.is_none());
        assert!(buf.mark().is_none());
    }

    #[test]
    fn buffer_id_equality() {
        let a = BufferId(1);
        let b = BufferId(1);
        let c = BufferId(2);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn create_indirect_buffer_shares_root_text_and_updates_siblings() {
        let mut mgr = BufferManager::new();
        let base_id = mgr.current_buffer_id().expect("scratch buffer");

        let _ = mgr.insert_into_buffer(base_id, "abcd");
        let indirect_id = mgr
            .create_indirect_buffer(base_id, "*indirect*", false)
            .expect("indirect buffer");

        let base = mgr.get(base_id).expect("base buffer");
        let indirect = mgr.get(indirect_id).expect("indirect buffer");
        assert_eq!(indirect.base_buffer, Some(base_id));
        assert!(base.text.shares_storage_with(&indirect.text));
        assert_eq!(indirect.buffer_string(), "abcd");

        let _ = mgr.goto_buffer_byte(base_id, 0);
        let _ = mgr.insert_into_buffer(base_id, "zz");
        assert_eq!(mgr.get(base_id).unwrap().buffer_string(), "zzabcd");
        assert_eq!(mgr.get(indirect_id).unwrap().buffer_string(), "zzabcd");

        let _ = mgr.delete_buffer_region(indirect_id, 2, 4);
        assert_eq!(mgr.get(base_id).unwrap().buffer_string(), "zzcd");
        assert_eq!(mgr.get(indirect_id).unwrap().buffer_string(), "zzcd");
    }

    #[test]
    fn create_indirect_buffer_flattens_double_indirection() {
        let mut mgr = BufferManager::new();
        let base_id = mgr.current_buffer_id().expect("scratch buffer");
        let first_id = mgr
            .create_indirect_buffer(base_id, "*indirect-one*", false)
            .expect("first indirect");
        let second_id = mgr
            .create_indirect_buffer(first_id, "*indirect-two*", false)
            .expect("second indirect");

        assert_eq!(mgr.get(first_id).unwrap().base_buffer, Some(base_id));
        assert_eq!(mgr.get(second_id).unwrap().base_buffer, Some(base_id));
        assert!(
            mgr.get(base_id)
                .unwrap()
                .text
                .shares_storage_with(&mgr.get(second_id).unwrap().text)
        );
    }

    #[test]
    fn indirect_buffers_keep_undo_state_in_sync() {
        let mut mgr = BufferManager::new();
        let base_id = mgr.current_buffer_id().expect("scratch buffer");
        let indirect_id = mgr
            .create_indirect_buffer(base_id, "*indirect-undo*", false)
            .expect("indirect buffer");

        let _ = mgr.insert_into_buffer(base_id, "abc");
        assert!(
            !matches!(
                mgr.get(indirect_id)
                    .and_then(|buf| buf.buffer_local_value("buffer-undo-list")),
                Some(Value::Nil) | None
            ),
            "indirect buffer should observe the base buffer's undo history"
        );

        let result = mgr.undo_buffer(indirect_id, 1).expect("undo result");
        assert!(result.applied_any);
        assert_eq!(mgr.get(base_id).unwrap().buffer_string(), "");
        assert_eq!(mgr.get(indirect_id).unwrap().buffer_string(), "");
    }

    // -----------------------------------------------------------------------
    // Point movement
    // -----------------------------------------------------------------------

    #[test]
    fn goto_char_clamps_to_accessible_region() {
        let mut buf = buf_with_text("hello");
        buf.goto_char(3);
        assert_eq!(buf.point(), 3);

        // Past end — clamped to zv.
        buf.goto_char(999);
        assert_eq!(buf.point(), buf.point_max());

        // Before start — clamped to begv.
        buf.goto_char(0);
        buf.begv = 2;
        buf.goto_char(0);
        assert_eq!(buf.point(), 2);
    }

    #[test]
    fn point_char_converts_byte_to_char_pos() {
        // "cafe\u{0301}" — 'e' + combining acute = 5 bytes, 5 chars in UTF-8
        let mut buf = buf_with_text("hello");
        buf.goto_char(3);
        assert_eq!(buf.point_char(), 3);
    }

    // -----------------------------------------------------------------------
    // Insertion
    // -----------------------------------------------------------------------

    #[test]
    fn insert_at_point_advances_point() {
        let mut buf = Buffer::new(BufferId(1), "test".into());
        // zv starts at 0 for an empty buffer; insert should extend it.
        buf.insert("hello");
        assert_eq!(buf.point(), 5);
        assert_eq!(buf.buffer_string(), "hello");
        assert_eq!(buf.buffer_size(), 5);
        assert!(buf.is_modified());
    }

    #[test]
    fn insert_in_middle() {
        let mut buf = buf_with_text("helo");
        buf.goto_char(3);
        buf.insert("l");
        assert_eq!(buf.buffer_string(), "hello");
        assert_eq!(buf.point(), 4);
    }

    #[test]
    fn insert_adjusts_mark() {
        let mut buf = buf_with_text("ab");
        buf.set_mark(1);
        buf.goto_char(0);
        buf.insert("X");
        // Mark was at 1, insert at 0 pushes it to 2.
        assert_eq!(buf.mark(), Some(2));
    }

    #[test]
    fn insert_empty_string_is_noop() {
        let mut buf = buf_with_text("hello");
        buf.goto_char(2);
        buf.insert("");
        assert_eq!(buf.buffer_string(), "hello");
        assert!(!buf.is_modified()); // still unmodified from initial state
    }

    // -----------------------------------------------------------------------
    // Deletion
    // -----------------------------------------------------------------------

    #[test]
    fn delete_region_basic() {
        let mut buf = buf_with_text("hello world");
        buf.goto_char(11); // at end
        buf.delete_region(5, 11);
        assert_eq!(buf.buffer_string(), "hello");
        assert_eq!(buf.point(), 5); // was past deleted range
    }

    #[test]
    fn delete_region_adjusts_point_inside() {
        let mut buf = buf_with_text("abcdef");
        buf.goto_char(3); // in middle of deleted range
        buf.delete_region(1, 5);
        assert_eq!(buf.point(), 1); // collapsed to start of deletion
        assert_eq!(buf.buffer_string(), "af");
    }

    #[test]
    fn delete_region_adjusts_mark() {
        let mut buf = buf_with_text("abcdef");
        buf.set_mark(4);
        buf.delete_region(1, 3);
        // mark was at 4, past deleted range end (3), so shifts by 2
        assert_eq!(buf.mark(), Some(2));
    }

    #[test]
    fn delete_region_adjusts_zv() {
        let mut buf = buf_with_text("abcdef");
        assert_eq!(buf.zv, 6);
        buf.delete_region(2, 4);
        assert_eq!(buf.zv, 4);
    }

    #[test]
    fn delete_empty_range_is_noop() {
        let mut buf = buf_with_text("hello");
        buf.delete_region(2, 2);
        assert_eq!(buf.buffer_string(), "hello");
    }

    // -----------------------------------------------------------------------
    // Substring / buffer_string
    // -----------------------------------------------------------------------

    #[test]
    fn buffer_substring_range() {
        let buf = buf_with_text("hello world");
        assert_eq!(buf.buffer_substring(6, 11), "world");
    }

    #[test]
    fn buffer_string_returns_accessible() {
        let mut buf = buf_with_text("hello world");
        buf.narrow_to_region(6, 11);
        assert_eq!(buf.buffer_string(), "world");
    }

    // -----------------------------------------------------------------------
    // char_after / char_before
    // -----------------------------------------------------------------------

    #[test]
    fn char_after_basic() {
        let buf = buf_with_text("hello");
        assert_eq!(buf.char_after(0), Some('h'));
        assert_eq!(buf.char_after(4), Some('o'));
        assert_eq!(buf.char_after(5), None);
    }

    #[test]
    fn char_before_basic() {
        let buf = buf_with_text("hello");
        assert_eq!(buf.char_before(0), None);
        assert_eq!(buf.char_before(1), Some('h'));
        assert_eq!(buf.char_before(5), Some('o'));
    }

    #[test]
    fn char_after_multibyte() {
        // Each Chinese character is 3 bytes in UTF-8.
        let buf = buf_with_text("\u{4f60}\u{597d}"); // "nihao" in Chinese
        assert_eq!(buf.char_after(0), Some('\u{4f60}'));
        assert_eq!(buf.char_after(3), Some('\u{597d}'));
    }

    #[test]
    fn char_before_multibyte() {
        let buf = buf_with_text("\u{4f60}\u{597d}");
        assert_eq!(buf.char_before(3), Some('\u{4f60}'));
        assert_eq!(buf.char_before(6), Some('\u{597d}'));
    }

    // -----------------------------------------------------------------------
    // Narrowing
    // -----------------------------------------------------------------------

    #[test]
    fn narrow_and_widen() {
        let mut buf = buf_with_text("hello world");
        buf.goto_char(8);
        buf.narrow_to_region(6, 11);
        assert_eq!(buf.point_min(), 6);
        assert_eq!(buf.point_max(), 11);
        assert_eq!(buf.buffer_size(), 5);
        assert_eq!(buf.buffer_string(), "world");
        // Point was 8 — still within [6, 11].
        assert_eq!(buf.point(), 8);

        buf.widen();
        assert_eq!(buf.point_min(), 0);
        assert_eq!(buf.point_max(), 11);
    }

    #[test]
    fn narrow_clamps_point() {
        let mut buf = buf_with_text("hello world");
        buf.goto_char(2);
        buf.narrow_to_region(5, 11);
        // Point 2 < begv 5 => clamped to 5.
        assert_eq!(buf.point(), 5);
    }

    // -----------------------------------------------------------------------
    // Markers
    // -----------------------------------------------------------------------

    #[test]
    fn marker_tracks_insertion_after() {
        let mut buf = buf_with_text("ab");
        buf.markers.push(MarkerEntry {
            id: 1,
            byte_pos: 1,
            insertion_type: InsertionType::After,
        });
        buf.goto_char(1);
        buf.insert("XY");
        // Marker was at 1 with After => advances to 3.
        assert_eq!(buf.markers[0].byte_pos, 3);
    }

    #[test]
    fn marker_stays_on_insertion_before() {
        let mut buf = buf_with_text("ab");
        buf.markers.push(MarkerEntry {
            id: 1,
            byte_pos: 1,
            insertion_type: InsertionType::Before,
        });
        buf.goto_char(1);
        buf.insert("XY");
        // Marker was at 1 with Before => stays at 1.
        assert_eq!(buf.markers[0].byte_pos, 1);
    }

    #[test]
    fn marker_adjusts_on_deletion() {
        let mut buf = buf_with_text("abcdef");
        buf.markers.push(MarkerEntry {
            id: 1,
            byte_pos: 4,
            insertion_type: InsertionType::After,
        });
        buf.delete_region(1, 3);
        // Marker was at 4 (past deleted range [1,3)), shifts by 2 => 2.
        assert_eq!(buf.markers[0].byte_pos, 2);
    }

    #[test]
    fn marker_inside_deleted_range_collapses() {
        let mut buf = buf_with_text("abcdef");
        buf.markers.push(MarkerEntry {
            id: 1,
            byte_pos: 2,
            insertion_type: InsertionType::After,
        });
        buf.delete_region(1, 5);
        // Marker at 2 inside [1,5) => collapses to 1.
        assert_eq!(buf.markers[0].byte_pos, 1);
    }

    // -----------------------------------------------------------------------
    // Buffer-local variables
    // -----------------------------------------------------------------------

    #[test]
    fn buffer_local_get_set() {
        let mut buf = Buffer::new(BufferId(1), "test".into());
        assert!(buf.get_buffer_local("tab-width").is_none());

        buf.set_buffer_local("tab-width", Value::Int(4));
        let val = buf.get_buffer_local("tab-width").unwrap();
        assert!(matches!(val, Value::Int(4)));

        buf.set_buffer_local("tab-width", Value::Int(8));
        let val = buf.get_buffer_local("tab-width").unwrap();
        assert!(matches!(val, Value::Int(8)));
    }

    #[test]
    fn buffer_local_multiple_vars() {
        let mut buf = Buffer::new(BufferId(1), "test".into());
        buf.set_buffer_local("fill-column", Value::Int(80));
        buf.set_buffer_local("major-mode", Value::symbol("text-mode"));

        assert!(buf.get_buffer_local("fill-column").is_some());
        assert!(buf.get_buffer_local("major-mode").is_some());
        assert!(buf.get_buffer_local("nonexistent").is_none());
    }

    #[test]
    fn buffer_local_defaults_include_builtin_per_buffer_vars() {
        let buf = Buffer::new(BufferId(1), "test".into());

        assert_eq!(
            buf.buffer_local_value("major-mode"),
            Some(Value::symbol("fundamental-mode"))
        );
        assert_eq!(
            buf.buffer_local_value("mode-name"),
            Some(Value::string("Fundamental"))
        );
        assert_eq!(buf.buffer_local_value("buffer-undo-list"), Some(Value::Nil));
    }

    // -----------------------------------------------------------------------
    // Modified flag
    // -----------------------------------------------------------------------

    #[test]
    fn modified_flag() {
        let mut buf = Buffer::new(BufferId(1), "test".into());
        assert!(!buf.is_modified());
        buf.insert("x");
        assert!(buf.is_modified());
        buf.set_modified(false);
        assert!(!buf.is_modified());
    }

    #[test]
    fn modification_ticks_track_content_changes() {
        let mut buf = Buffer::new(BufferId(1), "test".into());
        assert_eq!(buf.modified_tick, 1);
        assert_eq!(buf.chars_modified_tick, 1);

        buf.insert("x");
        assert_eq!(buf.modified_tick, 2);
        assert_eq!(buf.chars_modified_tick, 2);

        buf.set_modified(false);
        assert_eq!(buf.modified_tick, 2);
        assert_eq!(buf.chars_modified_tick, 2);

        buf.delete_region(0, 1);
        assert_eq!(buf.modified_tick, 3);
        assert_eq!(buf.chars_modified_tick, 3);
    }

    // -----------------------------------------------------------------------
    // BufferManager — creation, lookup, kill
    // -----------------------------------------------------------------------

    #[test]
    fn manager_starts_with_scratch() {
        let mgr = BufferManager::new();
        let scratch = mgr.find_buffer_by_name("*scratch*");
        assert!(scratch.is_some());
        assert!(mgr.current_buffer().is_some());
        assert_eq!(mgr.current_buffer().unwrap().name, "*scratch*");
    }

    #[test]
    fn manager_create_and_lookup() {
        let mut mgr = BufferManager::new();
        let id = mgr.create_buffer("foo.el");
        assert!(mgr.get(id).is_some());
        assert_eq!(mgr.get(id).unwrap().name, "foo.el");
        assert_eq!(mgr.find_buffer_by_name("foo.el"), Some(id));
        assert_eq!(mgr.find_buffer_by_name("bar.el"), None);
    }

    #[test]
    fn manager_set_current() {
        let mut mgr = BufferManager::new();
        let a = mgr.create_buffer("a");
        let b = mgr.create_buffer("b");
        mgr.set_current(a);
        assert_eq!(mgr.current_buffer().unwrap().name, "a");
        mgr.set_current(b);
        assert_eq!(mgr.current_buffer().unwrap().name, "b");
    }

    #[test]
    fn manager_kill_buffer() {
        let mut mgr = BufferManager::new();
        let id = mgr.create_buffer("doomed");
        assert!(mgr.kill_buffer(id));
        assert!(mgr.get(id).is_none());
        assert!(!mgr.kill_buffer(id)); // already dead
    }

    #[test]
    fn manager_kill_current_clears_current() {
        let mut mgr = BufferManager::new();
        let scratch = mgr.find_buffer_by_name("*scratch*").unwrap();
        mgr.set_current(scratch);
        mgr.kill_buffer(scratch);
        assert!(mgr.current_buffer().is_none());
    }

    #[test]
    fn manager_buffer_list() {
        let mut mgr = BufferManager::new();
        mgr.create_buffer("a");
        mgr.create_buffer("b");
        // *scratch* + a + b = 3
        assert_eq!(mgr.buffer_list().len(), 3);
    }

    #[test]
    fn manager_generate_new_buffer_name_unique() {
        let mgr = BufferManager::new();
        // "*scratch*" is taken, "foo" is not.
        assert_eq!(mgr.generate_new_buffer_name("foo"), "foo");
        assert_eq!(mgr.generate_new_buffer_name("*scratch*"), "*scratch*<2>");
    }

    #[test]
    fn manager_generate_new_buffer_name_increments() {
        let mut mgr = BufferManager::new();
        mgr.create_buffer("buf");
        assert_eq!(mgr.generate_new_buffer_name("buf"), "buf<2>");
        mgr.create_buffer("buf<2>");
        assert_eq!(mgr.generate_new_buffer_name("buf"), "buf<3>");
    }

    // -----------------------------------------------------------------------
    // BufferManager — markers
    // -----------------------------------------------------------------------

    #[test]
    fn manager_create_and_query_marker() {
        let mut mgr = BufferManager::new();
        let id = mgr.create_buffer("m");
        // Insert some text so there is room for a marker.
        mgr.get_mut(id).unwrap().text = BufferText::from_str("abcdef");
        mgr.get_mut(id).unwrap().zv = 6;

        let mid = mgr.create_marker(id, 3, InsertionType::After);
        assert_eq!(mgr.marker_position(id, mid), Some(3));
    }

    #[test]
    fn manager_marker_clamped_to_buffer_len() {
        let mut mgr = BufferManager::new();
        let id = mgr.create_buffer("m");
        // Buffer is empty (len = 0), marker at 100 should be clamped.
        let mid = mgr.create_marker(id, 100, InsertionType::Before);
        assert_eq!(mgr.marker_position(id, mid), Some(0));
    }

    #[test]
    fn manager_marker_nonexistent_buffer() {
        let mgr = BufferManager::new();
        let pos = mgr.marker_position(BufferId(9999), 1);
        assert_eq!(pos, None);
    }

    // -----------------------------------------------------------------------
    // BufferManager — current_buffer_mut
    // -----------------------------------------------------------------------

    #[test]
    fn manager_current_buffer_mut_insert() {
        let mut mgr = BufferManager::new();
        let current = mgr.current_buffer_id().unwrap();
        mgr.insert_into_buffer(current, "hello");
        assert_eq!(mgr.current_buffer().unwrap().buffer_string(), "hello");
    }

    #[test]
    fn manager_replace_buffer_contents_resets_narrowing_and_point() {
        let mut mgr = BufferManager::new();
        let current = mgr.current_buffer_id().unwrap();
        let buf = mgr.get_mut(current).unwrap();
        buf.insert("abcdefgh");
        buf.narrow_to_region(2, 6);
        buf.goto_char(4);

        mgr.replace_buffer_contents(current, "xy");

        let buf = mgr.get(current).unwrap();
        assert_eq!(buf.buffer_string(), "xy");
        assert_eq!(buf.point(), 0);
        assert_eq!(buf.point_min(), 0);
        assert_eq!(buf.point_max(), 2);
    }

    // -----------------------------------------------------------------------
    // Integration: multiple operations
    // -----------------------------------------------------------------------

    #[test]
    fn integration_edit_narrow_widen() {
        let mut buf = Buffer::new(BufferId(1), "work".into());
        buf.insert("abcdefghij");
        assert_eq!(buf.buffer_string(), "abcdefghij");

        buf.narrow_to_region(2, 8);
        assert_eq!(buf.buffer_string(), "cdefgh");

        buf.goto_char(5);
        buf.insert("XX");
        assert_eq!(buf.buffer_string(), "cdeXXfgh");

        buf.widen();
        assert_eq!(buf.buffer_string(), "abcdeXXfghij");
    }
}
