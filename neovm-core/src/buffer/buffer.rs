//! Buffer and BufferManager — the core text container for the Elisp VM.
//!
//! A `Buffer` wraps a [`BufferText`] with Emacs-style point, mark, narrowing,
//! markers, and buffer-local variables.  `BufferManager` owns all live buffers
//! and tracks the current buffer.

use std::collections::{HashMap, HashSet};

use super::buffer_text::BufferText;
use super::locals::BufferLocals;
use super::overlay::OverlayList;
use super::shared::SharedUndoState;
use super::text_props::TextPropertyTable;
use super::undo;
use crate::emacs_core::syntax::SyntaxTable;
use crate::emacs_core::value::{RuntimeBindingValue, Value, ValueKind};
use crate::gc_trace::GcTrace;
use crate::tagged::gc::with_tagged_heap;
use crate::window::WindowId;

// ---------------------------------------------------------------------------
// BUFFER_SLOT_COUNT — sized to mirror GNU's `MAX_PER_BUFFER_VARS = 50`.
// ---------------------------------------------------------------------------

/// Number of `BUFFER_OBJFWD` slots in [`Buffer::slots`]. Mirrors GNU's
/// `MAX_PER_BUFFER_VARS = 50` limit on per-buffer C-side variables
/// (`buffer.c:4719`). Bump this if NeoMacs registers more forwarders
/// than GNU does, but only after a careful audit — the number bounds
/// every Buffer's memory footprint.
pub const BUFFER_SLOT_COUNT: usize = 50;

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
    pub buffer_id: BufferId,
    pub byte_pos: usize,
    pub char_pos: usize,
    pub insertion_type: InsertionType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferStateMarkers {
    pub pt_marker: u64,
    pub begv_marker: u64,
    pub zv_marker: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LabeledRestrictionLabel {
    Outermost,
    User(Value),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LabeledRestriction {
    pub label: LabeledRestrictionLabel,
    pub beg_marker: u64,
    pub end_marker: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SavedRestrictionKind {
    None,
    Markers { beg_marker: u64, end_marker: u64 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct SavedRestrictionState {
    pub buffer_id: BufferId,
    pub restriction: SavedRestrictionKind,
    pub labeled_restrictions: Option<Vec<LabeledRestriction>>,
}

impl SavedRestrictionState {
    pub fn trace_roots(&self, roots: &mut Vec<Value>) {
        if let Some(restrictions) = &self.labeled_restrictions {
            for restriction in restrictions {
                if let LabeledRestrictionLabel::User(label) = restriction.label {
                    roots.push(label);
                }
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutermostRestrictionResetState {
    pub affected_buffers: Vec<BufferId>,
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
    /// Point — the current cursor character position.
    pub pt_char: usize,
    /// Mark — optional byte position for region operations.
    pub mark: Option<usize>,
    /// Mark — optional character position for region operations.
    pub mark_char: Option<usize>,
    /// Beginning of accessible (narrowed) portion (byte pos, inclusive).
    pub begv: usize,
    /// Beginning of accessible (narrowed) portion (char pos, inclusive).
    pub begv_char: usize,
    /// End of accessible (narrowed) portion (byte pos, exclusive).
    pub zv: usize,
    /// End of accessible (narrowed) portion (char pos, exclusive).
    pub zv_char: usize,
    /// Whether the buffer has been modified since last save.
    pub modified: bool,
    /// Monotonic buffer modification tick.
    pub modified_tick: i64,
    /// Monotonic character-content modification tick.
    pub chars_modified_tick: i64,
    /// GNU `SAVE_MODIFF`: buffer-modified state is `save_modified_tick < modified_tick`.
    pub save_modified_tick: i64,
    /// GNU `BUF_AUTOSAVE_MODIFF`: recent auto-save state is
    /// `save_modified_tick < autosave_modified_tick`.
    pub autosave_modified_tick: i64,
    /// GNU `last_window_start`: start position of the most recently
    /// disconnected window that showed this buffer.
    pub last_window_start: usize,
    /// GNU `last_selected_window`: most recently selected live window showing
    /// this buffer, when known.
    pub last_selected_window: Option<WindowId>,
    /// GNU `inhibit_buffer_hooks`: suppress buffer lifecycle hooks for
    /// temporary/internal buffers.
    pub inhibit_buffer_hooks: bool,
    /// If true, insertions/deletions are forbidden.
    pub read_only: bool,
    /// Multi-byte encoding flag.  Always `true` for now.
    pub multibyte: bool,
    /// Associated file path, if any.
    pub file_name: Option<String>,
    /// Associated auto-save file path, if any.
    pub auto_save_file_name: Option<String>,
    /// GNU-style noncurrent PT/BEGV/ZV markers for buffers that share text.
    pub state_markers: Option<BufferStateMarkers>,
    /// Buffer-local state, split between builtin slot-backed locals and
    /// ordinary Lisp locals.
    pub locals: BufferLocals,
    /// `local_var_alist` — list of `(SYMBOL . VALUE)` per-buffer
    /// bindings for `SYMBOL_LOCALIZED` variables. Mirrors GNU
    /// `BVAR(buffer, local_var_alist)` (`buffer.h:362`). Phase 4 of
    /// the symbol-redirect refactor adds this field; the legacy
    /// [`Self::locals`] map stays in place during the transition
    /// (Phase 10 deletes it).
    pub local_var_alist: crate::emacs_core::value::Value,
    /// `BUFFER_OBJFWD` slot table — per-buffer storage for variables
    /// that are forwarded into the C-side `struct buffer` in GNU.
    /// Mirrors the union of GNU's `Lisp_Object` slot fields in
    /// `buffer.h:319-462`. Indexed by [`crate::emacs_core::forward::LispBufferObjFwd::offset`].
    ///
    /// Phase 8a of the symbol-redirect refactor adds the slot table.
    /// Phase 8b will migrate the hardcoded fields ([`Self::file_name`],
    /// [`Self::auto_save_file_name`], [`Self::read_only`],
    /// [`Self::multibyte`]) into slots and remove the duplicates.
    pub slots: [crate::emacs_core::value::Value; BUFFER_SLOT_COUNT],
    /// Overlays attached to the buffer.
    pub overlays: OverlayList,
    /// Syntax table for character classification.
    pub syntax_table: SyntaxTable,
    /// Shared undo owner for this text.
    pub undo_state: SharedUndoState,
}

impl Buffer {
    // -- Construction --------------------------------------------------------

    /// Create a new, empty buffer.
    pub fn new(id: BufferId, name: String) -> Self {
        Self {
            id,
            name,
            base_buffer: None,
            text: BufferText::new(),
            pt: 0,
            pt_char: 0,
            mark: None,
            mark_char: None,
            begv: 0,
            begv_char: 0,
            zv: 0,
            zv_char: 0,
            modified: false,
            modified_tick: 1,
            chars_modified_tick: 1,
            save_modified_tick: 1,
            autosave_modified_tick: 1,
            last_window_start: 1,
            last_selected_window: None,
            inhibit_buffer_hooks: false,
            read_only: false,
            multibyte: true,
            file_name: None,
            auto_save_file_name: None,
            state_markers: None,
            locals: BufferLocals::new(),
            local_var_alist: crate::emacs_core::value::Value::NIL,
            slots: [crate::emacs_core::value::Value::NIL; BUFFER_SLOT_COUNT],
            overlays: OverlayList::new(),
            syntax_table: SyntaxTable::new_standard(),
            undo_state: SharedUndoState::new(),
        }
    }

    // -- Point queries -------------------------------------------------------

    /// Current point as a byte position.
    pub fn point_byte(&self) -> usize {
        self.pt
    }

    /// Legacy point accessor retained while buffer internals are byte-only.
    pub fn point(&self) -> usize {
        self.point_byte()
    }

    /// Current point converted to a character position.
    pub fn point_char(&self) -> usize {
        self.pt_char
    }

    /// Beginning of the accessible portion (byte position).
    pub fn point_min_byte(&self) -> usize {
        self.begv
    }

    /// Beginning of the accessible portion (character position).
    pub fn point_min_char(&self) -> usize {
        self.begv_char
    }

    /// Legacy narrowing accessor retained while buffer internals are byte-only.
    pub fn point_min(&self) -> usize {
        self.point_min_byte()
    }

    /// End of the accessible portion (byte position).
    pub fn point_max_byte(&self) -> usize {
        self.zv
    }

    /// End of the accessible portion (character position).
    pub fn point_max_char(&self) -> usize {
        self.zv_char
    }

    /// Total number of characters in the buffer text.
    pub fn total_chars(&self) -> usize {
        self.text.char_count()
    }

    /// Convert a 0-based character position to a byte position, clamping to
    /// the buffer text length.
    pub fn char_to_byte_clamped(&self, char_pos: usize) -> usize {
        self.text.char_to_byte(char_pos.min(self.total_chars()))
    }

    /// Convert a 1-based Lisp character position to a byte position, clamping
    /// to the full buffer.
    pub fn lisp_pos_to_byte(&self, lisp_pos: i64) -> usize {
        let char_pos = if lisp_pos > 0 {
            lisp_pos as usize - 1
        } else {
            0
        };
        self.char_to_byte_clamped(char_pos)
    }

    /// Convert a 1-based Lisp character position to a byte position, clamping
    /// to the accessible region.
    pub fn lisp_pos_to_accessible_byte(&self, lisp_pos: i64) -> usize {
        let char_pos = if lisp_pos > 0 {
            lisp_pos as usize - 1
        } else {
            0
        };
        let clamped_char = char_pos.clamp(self.point_min_char(), self.point_max_char());
        self.text.char_to_byte(clamped_char)
    }

    /// Convert a 1-based Lisp character position to a byte position, clamping
    /// to the *full* buffer range (ignoring narrowing).
    ///
    /// GNU Emacs: `set-marker` clamps to the full buffer, not the narrowed
    /// region, so markers can be placed outside the accessible range.
    pub fn lisp_pos_to_full_buffer_byte(&self, lisp_pos: i64) -> usize {
        let char_pos = if lisp_pos > 0 {
            lisp_pos as usize - 1
        } else {
            0
        };
        let clamped_char = char_pos.min(self.total_chars());
        self.text.char_to_byte(clamped_char)
    }

    /// Legacy narrowing accessor retained while buffer internals are byte-only.
    pub fn point_max(&self) -> usize {
        self.point_max_byte()
    }

    // -- Point movement ------------------------------------------------------

    /// Set point in bytes, clamping to the accessible region `[begv, zv]`.
    pub fn goto_byte(&mut self, pos: usize) {
        self.pt = pos.clamp(self.begv, self.zv);
        self.pt_char = if self.pt == self.begv {
            self.begv_char
        } else if self.pt == self.zv {
            self.zv_char
        } else {
            self.text.byte_to_char(self.pt)
        };
    }

    /// Legacy point setter retained while buffer internals are byte-only.
    pub fn goto_char(&mut self, pos: usize) {
        self.goto_byte(pos);
    }

    fn apply_byte_insert_side_effects(
        &mut self,
        insert_pos: usize,
        insert_char_pos: usize,
        byte_len: usize,
        char_len: usize,
        update_state_fields: bool,
        shift_begv: bool,
        advance_point_at_insert: bool,
        adjust_shared_markers: bool,
        adjust_shared_text_props: bool,
        overlay_before_markers: bool,
    ) {
        if byte_len == 0 {
            return;
        }

        if update_state_fields {
            if self.pt > insert_pos || (advance_point_at_insert && self.pt == insert_pos) {
                self.pt += byte_len;
                self.pt_char += char_len;
            }
            if shift_begv && self.begv > insert_pos {
                self.begv += byte_len;
                self.begv_char += char_len;
            }
            if self.zv >= insert_pos {
                self.zv += byte_len;
                self.zv_char += char_len;
            }
        }
        if let Some(mark) = self.mark
            && mark > insert_pos
        {
            self.mark = Some(mark + byte_len);
            self.mark_char = self.mark_char.map(|mark_char| mark_char + char_len);
        }
        if adjust_shared_markers {
            self.text
                .adjust_markers_for_insert(insert_pos, byte_len, char_len);
        }
        debug_assert_eq!(
            self.text.byte_to_char(insert_pos),
            insert_char_pos,
            "insert-side-effect char position drifted from the source edit site"
        );
        if adjust_shared_text_props {
            self.text.adjust_text_props_for_insert(insert_pos, byte_len);
        }
        self.overlays
            .adjust_for_insert(insert_pos, byte_len, overlay_before_markers);
        self.record_char_modification(char_len);
        self.sync_modified_flag();
    }

    fn apply_byte_delete_side_effects(
        &mut self,
        start: usize,
        end: usize,
        start_char: usize,
        end_char: usize,
        update_state_fields: bool,
        shift_begv: bool,
        adjust_shared_markers: bool,
        adjust_shared_text_props: bool,
    ) {
        if start >= end {
            return;
        }
        let byte_len = end - start;
        let char_len = end_char - start_char;

        if update_state_fields {
            if self.pt >= end {
                self.pt -= byte_len;
                self.pt_char -= char_len;
            } else if self.pt > start {
                self.pt = start;
                self.pt_char = start_char;
            }

            if shift_begv {
                if self.begv >= end {
                    self.begv -= byte_len;
                    self.begv_char -= char_len;
                } else if self.begv > start {
                    self.begv = start;
                    self.begv_char = start_char;
                }
            }

            if self.zv >= end {
                self.zv -= byte_len;
                self.zv_char -= char_len;
            } else if self.zv > start {
                self.zv = start;
                self.zv_char = start_char;
            }
        }

        if let Some(mark) = self.mark {
            if mark >= end {
                self.mark = Some(mark - byte_len);
                self.mark_char = self.mark_char.map(|mark_char| mark_char - char_len);
            } else if mark > start {
                self.mark = Some(start);
                self.mark_char = Some(start_char);
            }
        }

        if adjust_shared_markers {
            self.text
                .adjust_markers_for_delete(start, end, start_char, end_char);
        }

        if adjust_shared_text_props {
            self.text.adjust_text_props_for_delete(start, end);
        }
        self.overlays.adjust_for_delete(start, end);
        self.record_char_modification(char_len);
        self.sync_modified_flag();
    }

    fn apply_same_len_edit_side_effects(
        &mut self,
        changed_chars: usize,
        preserve_modified_state: bool,
    ) {
        let old_state = self.modified_state_value();
        self.record_char_modification(changed_chars);
        if preserve_modified_state && old_state.is_nil() {
            self.save_modified_tick = self.modified_tick;
        }
        self.sync_modified_flag();
    }

    fn modification_tick_delta(changed_chars: usize) -> i64 {
        if changed_chars == 0 {
            1
        } else {
            changed_chars.ilog2() as i64 + 1
        }
    }

    /// GNU `modiff` increments logarithmically with edit size, and
    /// `chars_modiff` is reset to the new `modiff` on each character change.
    fn record_char_modification(&mut self, changed_chars: usize) {
        self.modified_tick += Self::modification_tick_delta(changed_chars);
        self.chars_modified_tick = self.modified_tick;
    }

    // -- Undo helpers --------------------------------------------------------

    /// Get the current `buffer-undo-list` value from buffer-local properties.
    pub fn get_undo_list(&self) -> Value {
        self.undo_state.list()
    }

    /// Store the `buffer-undo-list` value into buffer-local properties.
    pub fn set_undo_list(&mut self, value: Value) {
        self.undo_state.set_list(value);
        self.locals
            .set_raw_binding("buffer-undo-list", RuntimeBindingValue::Bound(value));
    }

    /// Prepare to record a buffer change: ensure the first-change sentinel
    /// has been recorded if needed.
    fn undo_ensure_first_change(&mut self) {
        if self.undo_state.recorded_first_change() {
            return;
        }
        let mut ul = self.get_undo_list();
        if undo::undo_list_is_disabled(&ul) {
            return;
        }
        undo::undo_list_record_first_change(&mut ul);
        self.set_undo_list(ul);
        self.undo_state.set_recorded_first_change(true);
    }

    /// Prepare undo recording for a buffer edit at `beg` with point at `pt`.
    fn undo_prepare_change(&mut self, beg: usize, pt: usize) {
        let ul = self.get_undo_list();
        if undo::undo_list_is_disabled(&ul) || self.undo_state.in_progress() {
            return;
        }
        self.undo_ensure_first_change();
    }

    // -- Editing -------------------------------------------------------------

    /// Insert `text` at point, advancing point past the inserted text.
    ///
    /// Markers at the insertion site move according to their `InsertionType`.
    fn insert_internal(&mut self, text: &str, before_markers: bool) {
        let insert_pos = self.pt;
        let insert_char_pos = self.pt_char;
        let byte_len = text.len();
        if byte_len == 0 {
            return;
        }
        let char_len = text.chars().count();

        // Record undo before modifying.
        if !self.undo_state.in_progress() {
            self.undo_prepare_change(insert_pos, self.pt);
            let mut ul = self.get_undo_list();
            if !undo::undo_list_is_disabled(&ul) {
                undo::undo_list_record_insert(&mut ul, insert_pos, byte_len, self.pt);
                self.set_undo_list(ul);
            }
        }

        self.text.insert_str(insert_pos, text);
        self.apply_byte_insert_side_effects(
            insert_pos,
            insert_char_pos,
            byte_len,
            char_len,
            true,
            false,
            true,
            true,
            true,
            before_markers,
        );
        if before_markers {
            self.text.advance_markers_at(insert_pos, byte_len, char_len);
        }
    }

    pub fn insert(&mut self, text: &str) {
        self.insert_internal(text, false);
    }

    pub fn insert_before_markers(&mut self, text: &str) {
        self.insert_internal(text, true);
    }

    /// Delete the byte range `[start, end)`.
    ///
    /// Adjusts point, mark, markers, and the narrowing boundary.
    pub fn delete_region(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let start_char = self.text.byte_to_char(start);
        let end_char = self.text.byte_to_char(end);
        // Record undo: save the deleted text for restoration.
        let deleted_text = self.text.text_range(start, end);
        if !self.undo_state.in_progress() {
            self.undo_prepare_change(start, self.pt);
            let mut ul = self.get_undo_list();
            if !undo::undo_list_is_disabled(&ul) {
                undo::undo_list_record_delete(&mut ul, start, &deleted_text, self.pt);
                self.set_undo_list(ul);
            }
        }

        self.text.delete_range(start, end);
        self.apply_byte_delete_side_effects(
            start, end, start_char, end_char, true, false, true, true,
        );
    }

    /// Replace every occurrence of `from` with `to` in the byte range
    /// `[start, end)`.
    ///
    /// The replacement is performed in place, so callers must ensure the
    /// characters have the same UTF-8 byte length.
    pub fn subst_char_in_region(
        &mut self,
        start: usize,
        end: usize,
        from: char,
        to: char,
        noundo: bool,
    ) -> bool {
        if start >= end || from == to {
            return false;
        }
        let changed_chars = self.text.byte_to_char(end) - self.text.byte_to_char(start);

        let original = self.text.text_range(start, end);
        if !original.contains(from) {
            return false;
        }

        let replacement: String = original
            .chars()
            .map(|ch| if ch == from { to } else { ch })
            .collect();
        if replacement == original {
            return false;
        }

        if !noundo && !self.undo_state.in_progress() {
            self.undo_prepare_change(start, self.pt);
            let mut ul = self.get_undo_list();
            if !undo::undo_list_is_disabled(&ul) {
                undo::undo_list_record_delete(&mut ul, start, &original, self.pt);
                undo::undo_list_record_insert(&mut ul, start, replacement.len(), self.pt);
                self.set_undo_list(ul);
            }
        }

        self.text.replace_same_len_range(start, end, &replacement);
        self.apply_same_len_edit_side_effects(changed_chars, noundo);
        true
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

    /// Restrict the accessible portion to the byte range `[start, end)`.
    pub fn narrow_to_byte_region(&mut self, start: usize, end: usize) {
        let total = self.text.len();
        let s = start.min(total);
        let e = end.clamp(s, total);
        let total_chars = self.text.char_count();
        self.begv = s;
        self.begv_char = self.text.byte_to_char(s);
        self.zv = e;
        self.zv_char = if e == total {
            total_chars
        } else {
            self.text.byte_to_char(e)
        };
        // Clamp point into the new accessible region.
        self.goto_byte(self.pt);
    }

    /// Legacy narrowing API retained while buffer internals are byte-only.
    pub fn narrow_to_region(&mut self, start: usize, end: usize) {
        self.narrow_to_byte_region(start, end);
    }

    /// Remove narrowing — make the entire buffer accessible again.
    pub fn widen(&mut self) {
        self.narrow_to_byte_region(0, self.text.len());
    }

    pub fn register_marker(&mut self, marker_id: u64, pos: usize, insertion_type: InsertionType) {
        let clamped = pos.min(self.text.len());
        let char_pos = if clamped == self.begv {
            self.begv_char
        } else if clamped == self.zv {
            self.zv_char
        } else {
            self.text.byte_to_char(clamped)
        };
        self.text
            .register_marker(self.id, marker_id, clamped, char_pos, insertion_type);
    }

    pub fn marker_entry(&self, marker_id: u64) -> Option<MarkerEntry> {
        self.text.marker_entry(marker_id)
    }

    pub fn remove_marker_entry(&mut self, marker_id: u64) {
        self.text.remove_marker(marker_id);
    }

    pub fn update_marker_insertion_type(&mut self, marker_id: u64, insertion_type: InsertionType) {
        self.text
            .update_marker_insertion_type(marker_id, insertion_type);
    }

    pub fn advance_markers_at(&mut self, pos: usize, byte_len: usize, char_len: usize) {
        self.text.advance_markers_at(pos, byte_len, char_len);
    }

    pub fn clear_marker_entries(&mut self) {
        self.text.clear_markers();
    }

    // -- Mark ----------------------------------------------------------------

    /// Set the mark to the byte position `pos`.
    pub fn set_mark_byte(&mut self, pos: usize) {
        let clamped = pos.clamp(self.begv, self.zv);
        let char_pos = if clamped == self.begv {
            self.begv_char
        } else if clamped == self.zv {
            self.zv_char
        } else {
            self.text.byte_to_char(clamped)
        };
        self.mark = Some(clamped);
        self.mark_char = Some(char_pos);
    }

    /// Legacy mark setter retained while buffer internals are byte-only.
    pub fn set_mark(&mut self, pos: usize) {
        self.set_mark_byte(pos);
    }

    /// Return the mark, if set.
    pub fn mark_byte(&self) -> Option<usize> {
        self.mark
    }

    /// Return the mark character position, if set.
    pub fn mark_char(&self) -> Option<usize> {
        self.mark_char
    }

    /// Legacy mark accessor retained while buffer internals are byte-only.
    pub fn mark(&self) -> Option<usize> {
        self.mark_byte()
    }

    // -- Modified flag -------------------------------------------------------

    pub fn is_modified(&self) -> bool {
        self.modified
    }

    pub fn modified_state_value(&self) -> Value {
        if self.save_modified_tick < self.modified_tick {
            if self.autosave_modified_tick == self.modified_tick {
                Value::symbol("autosaved")
            } else {
                Value::T
            }
        } else {
            Value::NIL
        }
    }

    pub fn recent_auto_save_p(&self) -> bool {
        self.save_modified_tick < self.autosave_modified_tick
    }

    fn sync_modified_flag(&mut self) {
        self.modified = self.save_modified_tick < self.modified_tick;
    }

    pub fn set_modified(&mut self, flag: bool) {
        if flag {
            if self.save_modified_tick >= self.modified_tick {
                self.modified_tick += 1;
            }
        } else {
            self.save_modified_tick = self.modified_tick;
        }
        self.sync_modified_flag();
    }

    pub fn restore_modified_state(&mut self, flag: Value) -> Value {
        if flag.is_nil() {
            self.save_modified_tick = self.modified_tick;
        } else {
            if self.save_modified_tick >= self.modified_tick {
                self.modified_tick += 1;
            }
            if flag == Value::symbol("autosaved") {
                self.autosave_modified_tick = self.modified_tick;
            }
        }
        self.sync_modified_flag();
        flag
    }

    pub fn mark_auto_saved(&mut self) {
        self.autosave_modified_tick = self.modified_tick;
    }

    // -- Buffer-local variables ----------------------------------------------

    pub fn set_buffer_local(&mut self, name: &str, value: Value) {
        if name == "buffer-file-name" {
            self.file_name = match value.kind() {
                ValueKind::String => value.as_str_owned(),
                ValueKind::Nil => None,
                _ => self.file_name.take(),
            };
        }
        if name == "buffer-auto-save-file-name" {
            self.auto_save_file_name = match value.kind() {
                ValueKind::String => value.as_str_owned(),
                ValueKind::Nil => None,
                _ => self.auto_save_file_name.take(),
            };
        }
        if name == "buffer-undo-list" {
            self.undo_state.set_list(value);
            if value.is_nil() {
                self.undo_state.set_recorded_first_change(false);
            }
        }
        self.locals
            .set_raw_binding(name, RuntimeBindingValue::Bound(value));
    }

    pub fn set_buffer_local_void(&mut self, name: &str) {
        if name == "buffer-file-name" {
            self.file_name = None;
        }
        if name == "buffer-auto-save-file-name" {
            self.auto_save_file_name = None;
        }
        if name == "buffer-undo-list" {
            self.undo_state.set_list(Value::NIL);
            self.undo_state.set_recorded_first_change(false);
        }
        self.locals.set_raw_binding(name, RuntimeBindingValue::Void);
    }

    pub fn kill_buffer_local(&mut self, name: &str) -> Option<RuntimeBindingValue> {
        if name == "buffer-undo-list" {
            return None;
        }
        self.locals.remove(name)
    }

    pub fn kill_all_local_variables(
        &mut self,
        obarray: &crate::emacs_core::symbol::Obarray,
        kill_permanent: bool,
    ) {
        self.locals
            .kill_all_local_variables(obarray, kill_permanent);
    }

    pub fn get_buffer_local(&self, name: &str) -> Option<&Value> {
        self.locals.raw_value_ref(name)
    }

    pub fn get_buffer_local_binding(&self, name: &str) -> Option<RuntimeBindingValue> {
        if !self.locals.has_local(name) {
            return None;
        }
        if name == "buffer-undo-list" {
            return Some(RuntimeBindingValue::Bound(self.get_undo_list()));
        }
        if name == "buffer-file-name" {
            return Some(match &self.file_name {
                Some(file_name) => RuntimeBindingValue::Bound(Value::string(file_name)),
                None => RuntimeBindingValue::Bound(Value::NIL),
            });
        }
        if name == "buffer-auto-save-file-name" {
            return Some(match &self.auto_save_file_name {
                Some(file_name) => RuntimeBindingValue::Bound(Value::string(file_name)),
                None => RuntimeBindingValue::Bound(Value::NIL),
            });
        }
        if name == "enable-multibyte-characters" {
            return Some(RuntimeBindingValue::Bound(Value::bool_val(self.multibyte)));
        }
        self.locals.raw_binding(name)
    }

    pub fn has_buffer_local(&self, name: &str) -> bool {
        self.locals.has_local(name)
    }

    pub fn local_map(&self) -> Value {
        self.locals.local_map()
    }

    pub fn set_local_map(&mut self, keymap: Value) {
        self.locals.set_local_map(keymap);
    }

    pub fn buffer_local_value(&self, name: &str) -> Option<Value> {
        match self.get_buffer_local_binding(name) {
            Some(RuntimeBindingValue::Bound(value)) => Some(value),
            Some(RuntimeBindingValue::Void) | None => None,
        }
    }

    pub fn ordered_buffer_local_bindings(&self) -> Vec<(String, RuntimeBindingValue)> {
        self.locals
            .ordered_runtime_bindings()
            .into_iter()
            .map(|(name, binding)| {
                if name == "buffer-undo-list" {
                    (name, RuntimeBindingValue::Bound(self.get_undo_list()))
                } else {
                    (name, binding)
                }
            })
            .collect()
    }

    pub fn ordered_buffer_local_names(&self) -> Vec<String> {
        self.locals.ordered_binding_names()
    }

    pub fn bound_buffer_local_values_mut(&mut self) -> impl Iterator<Item = &mut Value> {
        self.locals.bound_values_mut()
    }
}

impl Buffer {
    pub fn buffer_local_bound_p(&self, name: &str) -> bool {
        matches!(
            self.get_buffer_local_binding(name),
            Some(RuntimeBindingValue::Bound(_))
        )
    }

    pub fn buffer_local_void_p(&self, name: &str) -> bool {
        matches!(
            self.get_buffer_local_binding(name),
            Some(RuntimeBindingValue::Void)
        )
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
    labeled_restrictions: HashMap<BufferId, Vec<LabeledRestriction>>,
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
            labeled_restrictions: HashMap::new(),
            dead_buffer_last_names: HashMap::new(),
        };
        let scratch = mgr.create_buffer("*scratch*");
        mgr.current = Some(scratch);
        mgr
    }

    /// Allocate a new buffer with the given name and return its id.
    pub fn create_buffer(&mut self, name: &str) -> BufferId {
        self.create_buffer_with_hook_inhibition(name, false)
    }

    /// Allocate a new buffer with the given name and hook-inhibition state.
    pub fn create_buffer_with_hook_inhibition(
        &mut self,
        name: &str,
        inhibit_buffer_hooks: bool,
    ) -> BufferId {
        let id = BufferId(self.next_id);
        self.next_id += 1;
        let mut buf = Buffer::new(id, name.to_string());
        buf.inhibit_buffer_hooks = inhibit_buffer_hooks;
        if let Some(default_directory) = self
            .current
            .and_then(|current| self.buffers.get(&current))
            .and_then(|current| current.buffer_local_value("default-directory"))
        {
            buf.set_buffer_local("default-directory", default_directory);
        }
        // GNU buffer.c:667 — buffers whose names start with a space have
        // undo recording disabled by default.
        if name.starts_with(' ') {
            buf.set_buffer_local("buffer-undo-list", crate::emacs_core::value::Value::T);
        }
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
        self.create_indirect_buffer_with_hook_inhibition(base_id, name, clone, false)
    }

    pub fn create_indirect_buffer_with_hook_inhibition(
        &mut self,
        base_id: BufferId,
        name: &str,
        clone: bool,
        inhibit_buffer_hooks: bool,
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
            let mut fresh = Buffer::new(id, name.to_string());
            if let Some(default_directory) = self
                .current
                .and_then(|current| self.buffers.get(&current))
                .and_then(|current| current.buffer_local_value("default-directory"))
            {
                fresh.set_buffer_local("default-directory", default_directory);
            }
            fresh
        };

        indirect.base_buffer = Some(root_id);
        indirect.inhibit_buffer_hooks = inhibit_buffer_hooks;
        indirect.text = shared_text;
        indirect.undo_state = root.undo_state.clone();
        indirect.narrow_to_byte_region(root.begv, root.zv);
        indirect.goto_byte(root.pt);
        indirect.multibyte = root.multibyte;
        indirect.modified = root.modified;
        indirect.modified_tick = root.modified_tick;
        indirect.chars_modified_tick = root.chars_modified_tick;
        indirect.save_modified_tick = root.save_modified_tick;
        indirect.autosave_modified_tick = root.autosave_modified_tick;
        indirect.file_name = None;
        if !clone {
            indirect.overlays = OverlayList::new();
            indirect.mark = None;
        }

        self.buffers.insert(id, indirect);
        let _ = self.ensure_buffer_state_markers(root_id);
        let _ = self.ensure_buffer_state_markers(id);
        let _ = self.sync_shared_undo_binding_cache(root_id);
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

    pub fn buffer_hooks_inhibited(&self, id: BufferId) -> bool {
        self.buffers
            .get(&id)
            .is_some_and(|buffer| buffer.inhibit_buffer_hooks)
    }

    fn buffer_has_state_markers(&self, id: BufferId) -> bool {
        self.buffers
            .get(&id)
            .and_then(|buffer| buffer.state_markers)
            .is_some()
    }

    fn ensure_buffer_state_markers(&mut self, buffer_id: BufferId) -> Option<()> {
        if self.buffer_has_state_markers(buffer_id) {
            return Some(());
        }
        let (pt, begv, zv) = {
            let buffer = self.buffers.get(&buffer_id)?;
            (buffer.pt, buffer.begv, buffer.zv)
        };
        let pt_marker = self.create_marker(buffer_id, pt, InsertionType::Before);
        let begv_marker = self.create_marker(buffer_id, begv, InsertionType::Before);
        let zv_marker = self.create_marker(buffer_id, zv, InsertionType::After);
        self.buffers.get_mut(&buffer_id)?.state_markers = Some(BufferStateMarkers {
            pt_marker,
            begv_marker,
            zv_marker,
        });
        Some(())
    }

    fn record_buffer_state_markers(&mut self, buffer_id: BufferId) -> Option<()> {
        let markers = self.buffers.get(&buffer_id)?.state_markers?;
        let (pt, begv, zv) = {
            let buffer = self.buffers.get(&buffer_id)?;
            (buffer.pt, buffer.begv, buffer.zv)
        };
        self.register_marker_id(buffer_id, markers.pt_marker, pt, InsertionType::Before)?;
        self.register_marker_id(buffer_id, markers.begv_marker, begv, InsertionType::Before)?;
        self.register_marker_id(buffer_id, markers.zv_marker, zv, InsertionType::After)?;
        Some(())
    }

    fn fetch_buffer_state_markers(&mut self, buffer_id: BufferId) -> Option<()> {
        let markers = self.buffers.get(&buffer_id)?.state_markers?;
        let pt = self.marker_position(buffer_id, markers.pt_marker)?;
        let pt_char = self.marker_char_position(buffer_id, markers.pt_marker)?;
        let begv = self.marker_position(buffer_id, markers.begv_marker)?;
        let begv_char = self.marker_char_position(buffer_id, markers.begv_marker)?;
        let zv = self.marker_position(buffer_id, markers.zv_marker)?;
        let zv_char = self.marker_char_position(buffer_id, markers.zv_marker)?;
        let buffer = self.buffers.get_mut(&buffer_id)?;
        buffer.pt = pt;
        buffer.pt_char = pt_char;
        buffer.begv = begv;
        buffer.begv_char = begv_char;
        buffer.zv = zv;
        buffer.zv_char = zv_char;
        Some(())
    }

    /// Switch the current buffer and run buffer-manager-owned transition work.
    ///
    /// This is the closest NeoVM equivalent of GNU Emacs's
    /// `set_buffer_internal_1/2` boundary inside the buffer subsystem.
    pub fn switch_current(&mut self, id: BufferId) -> bool {
        if !self.buffers.contains_key(&id) {
            return false;
        }
        if self.current == Some(id) {
            if let Some(root_id) = self.shared_text_root_id(id) {
                let _ = self.sync_shared_undo_binding_cache(root_id);
            }
            return true;
        }

        let old_id = self.current;
        self.current = Some(id);

        if let Some(old_id) = old_id {
            if let Some(root_id) = self.shared_text_root_id(old_id) {
                let _ = self.sync_shared_undo_binding_cache(root_id);
            }
            let _ = self.record_buffer_state_markers(old_id);
        }
        if let Some(root_id) = self.shared_text_root_id(id) {
            let _ = self.sync_shared_undo_binding_cache(root_id);
        }
        let _ = self.fetch_buffer_state_markers(id);
        true
    }

    /// Backwards-compatible alias while call sites migrate to `switch_current`.
    pub fn set_current(&mut self, id: BufferId) {
        let _ = self.switch_current(id);
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
        self.kill_buffer_collect(id).is_some()
    }

    pub fn kill_buffer_collect(&mut self, id: BufferId) -> Option<Vec<BufferId>> {
        let killed_ids = self.collect_killed_buffer_ids(id)?;
        let killed_set: HashSet<BufferId> = killed_ids.iter().copied().collect();
        let kill_root = self.buffers.get(&id)?.base_buffer.is_none();

        for killed_id in &killed_ids {
            self.replace_labeled_restrictions(*killed_id, None);
        }

        with_tagged_heap(|heap| heap.clear_markers_for_buffers(&killed_set));
        if kill_root {
            self.buffers.get(&id)?.text.clear_markers();
        } else {
            self.buffers
                .get(&id)?
                .text
                .remove_markers_for_buffers(&killed_set);
        }

        for killed_id in &killed_ids {
            let buf = self.buffers.remove(killed_id)?;
            self.dead_buffer_last_names.insert(*killed_id, buf.name);
        }

        if self
            .current
            .is_some_and(|current| killed_set.contains(&current))
        {
            self.current = None;
        }

        Some(killed_ids)
    }

    /// Return the last known name for a dead buffer id, if available.
    pub fn dead_buffer_last_name(&self, id: BufferId) -> Option<&str> {
        self.dead_buffer_last_names.get(&id).map(|s| s.as_str())
    }

    /// List all live buffer ids in stable creation order.
    pub fn buffer_list(&self) -> Vec<BufferId> {
        let mut ids: Vec<BufferId> = self.buffers.keys().copied().collect();
        ids.sort_by_key(|id| id.0);
        ids
    }

    fn shared_text_root_id(&self, id: BufferId) -> Option<BufferId> {
        let buf = self.buffers.get(&id)?;
        Some(buf.base_buffer.unwrap_or(buf.id))
    }

    pub(crate) fn collect_killed_buffer_ids(&self, id: BufferId) -> Option<Vec<BufferId>> {
        let buf = self.buffers.get(&id)?;
        let mut killed_ids = vec![id];
        if buf.base_buffer.is_none() {
            let mut indirects = self
                .buffers
                .values()
                .filter_map(|buffer| (buffer.base_buffer == Some(id)).then_some(buffer.id))
                .collect::<Vec<_>>();
            indirects.sort_by_key(|buffer_id| buffer_id.0);
            killed_ids.extend(indirects);
        }
        Some(killed_ids)
    }

    fn full_buffer_bounds(&self, id: BufferId) -> Option<(usize, usize)> {
        let buf = self.buffers.get(&id)?;
        Some((0, buf.text.len()))
    }

    fn labeled_restriction_at(&self, id: BufferId, outermost: bool) -> Option<&LabeledRestriction> {
        let restrictions = self.labeled_restrictions.get(&id)?;
        if outermost {
            restrictions.first()
        } else {
            restrictions.last()
        }
    }

    fn labeled_restriction_bounds(&self, id: BufferId, outermost: bool) -> Option<(usize, usize)> {
        let restriction = self.labeled_restriction_at(id, outermost)?;
        let beg = self.marker_position(id, restriction.beg_marker)?;
        let end = self.marker_position(id, restriction.end_marker)?;
        Some((beg, end))
    }

    pub fn current_labeled_restriction_bounds(&self, id: BufferId) -> Option<(usize, usize)> {
        self.labeled_restriction_bounds(id, false)
    }

    pub fn current_labeled_restriction_char_bounds(&self, id: BufferId) -> Option<(usize, usize)> {
        let restriction = self.labeled_restriction_at(id, false)?;
        let beg = self.marker_char_position(id, restriction.beg_marker)?;
        let end = self.marker_char_position(id, restriction.end_marker)?;
        Some((beg, end))
    }

    pub fn current_labeled_restriction_matches_label(&self, id: BufferId, label: &Value) -> bool {
        let Some(restriction) = self.labeled_restriction_at(id, false) else {
            return false;
        };
        match restriction.label {
            LabeledRestrictionLabel::User(current) => {
                crate::emacs_core::value::eq_value(&current, label)
            }
            LabeledRestrictionLabel::Outermost => false,
        }
    }

    fn clone_marker_in_buffer(&mut self, buffer_id: BufferId, marker_id: u64) -> Option<u64> {
        let (pos, insertion_type) = {
            let buf = self.buffers.get(&buffer_id)?;
            let marker = buf.marker_entry(marker_id)?;
            (marker.byte_pos, marker.insertion_type)
        };
        Some(self.create_marker(buffer_id, pos, insertion_type))
    }

    fn clone_labeled_restrictions(
        &mut self,
        buffer_id: BufferId,
    ) -> Option<Option<Vec<LabeledRestriction>>> {
        let restrictions = self.labeled_restrictions.get(&buffer_id)?.clone();
        let mut cloned = Vec::with_capacity(restrictions.len());
        for restriction in restrictions {
            let beg_marker = self.clone_marker_in_buffer(buffer_id, restriction.beg_marker)?;
            let end_marker = self.clone_marker_in_buffer(buffer_id, restriction.end_marker)?;
            cloned.push(LabeledRestriction {
                label: restriction.label,
                beg_marker,
                end_marker,
            });
        }
        Some(Some(cloned))
    }

    fn replace_labeled_restrictions(
        &mut self,
        buffer_id: BufferId,
        restrictions: Option<Vec<LabeledRestriction>>,
    ) {
        let mut live_marker_ids = std::collections::HashSet::new();
        if let Some(ref restrictions) = restrictions {
            for restriction in restrictions {
                live_marker_ids.insert(restriction.beg_marker);
                live_marker_ids.insert(restriction.end_marker);
            }
        }

        if let Some(old) = self.labeled_restrictions.remove(&buffer_id) {
            for restriction in old {
                if !live_marker_ids.contains(&restriction.beg_marker) {
                    self.remove_marker(restriction.beg_marker);
                }
                if !live_marker_ids.contains(&restriction.end_marker) {
                    self.remove_marker(restriction.end_marker);
                }
            }
        }

        if self.buffers.contains_key(&buffer_id) {
            if let Some(restrictions) = restrictions.filter(|restrictions| !restrictions.is_empty())
            {
                self.labeled_restrictions.insert(buffer_id, restrictions);
            }
        }
    }

    pub fn clear_buffer_labeled_restrictions(&mut self, buffer_id: BufferId) -> Option<()> {
        self.buffers.get(&buffer_id)?;
        self.replace_labeled_restrictions(buffer_id, None);
        Some(())
    }

    fn push_labeled_restriction_for_current_bounds(
        &mut self,
        buffer_id: BufferId,
        label: LabeledRestrictionLabel,
    ) -> Option<()> {
        let (begv, zv) = {
            let buf = self.buffers.get(&buffer_id)?;
            (buf.begv, buf.zv)
        };
        let beg_marker = self.create_marker(buffer_id, begv, InsertionType::Before);
        let end_marker = self.create_marker(buffer_id, zv, InsertionType::After);
        self.labeled_restrictions
            .entry(buffer_id)
            .or_default()
            .push(LabeledRestriction {
                label,
                beg_marker,
                end_marker,
            });
        Some(())
    }

    fn pop_labeled_restriction(&mut self, buffer_id: BufferId) -> Option<LabeledRestriction> {
        let restrictions = self.labeled_restrictions.get_mut(&buffer_id)?;
        let restriction = restrictions.pop()?;
        let remove_entry = restrictions.is_empty();
        if remove_entry {
            self.labeled_restrictions.remove(&buffer_id);
        }
        self.remove_marker(restriction.beg_marker);
        self.remove_marker(restriction.end_marker);
        Some(restriction)
    }

    fn widen_buffer_fully(&mut self, id: BufferId) -> Option<()> {
        let (begv, zv) = self.full_buffer_bounds(id)?;
        self.restore_buffer_restriction(id, begv, zv)
    }

    fn buffers_sharing_root_ids(&self, root_id: BufferId) -> Vec<BufferId> {
        self.buffers
            .values()
            .filter_map(|buf| (buf.base_buffer.unwrap_or(buf.id) == root_id).then_some(buf.id))
            .collect()
    }

    pub(crate) fn modified_state_root_id(&self, id: BufferId) -> Option<BufferId> {
        self.shared_text_root_id(id)
    }

    fn sync_shared_modification_state(&mut self, root_id: BufferId) -> Option<()> {
        // GNU shares MODIFF/SAVE_MODIFF across indirect buffers via the
        // underlying text, but BUF_AUTOSAVE_MODIFF stays per-buffer.
        let (modified, modified_tick, chars_modified_tick, save_modified_tick) = {
            let root = self.buffers.get(&root_id)?;
            (
                root.modified,
                root.modified_tick,
                root.chars_modified_tick,
                root.save_modified_tick,
            )
        };

        for shared_id in self.buffers_sharing_root_ids(root_id) {
            let buf = self.buffers.get_mut(&shared_id)?;
            buf.modified = modified;
            buf.modified_tick = modified_tick;
            buf.chars_modified_tick = chars_modified_tick;
            buf.save_modified_tick = save_modified_tick;
        }
        Some(())
    }

    fn adjust_shared_insert_metadata(
        buf: &mut Buffer,
        insert_pos: usize,
        insert_char_pos: usize,
        byte_len: usize,
        char_len: usize,
        update_state_fields: bool,
        overlay_before_markers: bool,
    ) {
        buf.apply_byte_insert_side_effects(
            insert_pos,
            insert_char_pos,
            byte_len,
            char_len,
            update_state_fields,
            true,
            false,
            false,
            false,
            overlay_before_markers,
        );
    }

    fn adjust_shared_delete_metadata(
        buf: &mut Buffer,
        start: usize,
        end: usize,
        start_char: usize,
        end_char: usize,
        update_state_fields: bool,
    ) {
        buf.apply_byte_delete_side_effects(
            start,
            end,
            start_char,
            end_char,
            update_state_fields,
            true,
            false,
            false,
        );
    }

    fn adjust_shared_same_len_edit_metadata(
        buf: &mut Buffer,
        changed_chars: usize,
        preserve_modified_state: bool,
    ) {
        buf.apply_same_len_edit_side_effects(changed_chars, preserve_modified_state);
    }

    fn sync_shared_undo_binding_cache(&mut self, root_id: BufferId) -> Option<()> {
        let shared_ids = self.buffers_sharing_root_ids(root_id);
        let authoritative = self.buffers.get(&root_id)?.get_undo_list();
        for shared_id in shared_ids {
            self.buffers.get_mut(&shared_id)?.locals.set_raw_binding(
                "buffer-undo-list",
                RuntimeBindingValue::Bound(authoritative),
            );
        }
        Some(())
    }

    fn refresh_shared_buffer_state_cache(
        &mut self,
        buffer_id: BufferId,
        update_state_fields: bool,
    ) -> Option<()> {
        if !update_state_fields && self.buffer_has_state_markers(buffer_id) {
            self.fetch_buffer_state_markers(buffer_id)?;
        }
        Some(())
    }

    /// Centralized structural text mutations.
    ///
    /// Indirect buffers will eventually share a single text object.  When that
    /// happens, sibling buffers must be updated from one place instead of every
    /// ad hoc `buf.insert` / `buf.delete_region` call site in the tree.
    pub fn goto_buffer_byte(&mut self, id: BufferId, pos: usize) -> Option<usize> {
        {
            let buf = self.buffers.get_mut(&id)?;
            buf.goto_byte(pos);
        }
        let point = self.buffers.get(&id)?.point_byte();
        let _ = self.record_buffer_state_markers(id);
        Some(point)
    }

    pub fn insert_into_buffer(&mut self, id: BufferId, text: &str) -> Option<()> {
        let byte_len = text.len();
        if byte_len == 0 {
            return Some(());
        }
        let char_len = text.chars().count();

        let root_id = self.shared_text_root_id(id)?;
        let shared_ids = self.buffers_sharing_root_ids(root_id);
        let source = self.buffers.get(&id)?;
        let insert_pos = source.pt;
        let insert_char_pos = source.pt_char;

        self.buffers.get_mut(&id)?.insert(text);

        for sibling_id in shared_ids {
            if sibling_id == id {
                continue;
            }
            let update_state_fields =
                self.current == Some(sibling_id) || !self.buffer_has_state_markers(sibling_id);
            let sibling = self.buffers.get_mut(&sibling_id)?;
            Self::adjust_shared_insert_metadata(
                sibling,
                insert_pos,
                insert_char_pos,
                byte_len,
                char_len,
                update_state_fields,
                false,
            );
            self.refresh_shared_buffer_state_cache(sibling_id, update_state_fields)?;
        }
        self.sync_shared_undo_binding_cache(root_id)?;
        Some(())
    }

    pub fn insert_into_buffer_before_markers(&mut self, id: BufferId, text: &str) -> Option<()> {
        let byte_len = text.len();
        if byte_len == 0 {
            return Some(());
        }
        let char_len = text.chars().count();
        let root_id = self.shared_text_root_id(id)?;
        let shared_ids = self.buffers_sharing_root_ids(root_id);
        let source = self.buffers.get(&id)?;
        let insert_pos = source.pt;
        let insert_char_pos = source.pt_char;

        self.buffers.get_mut(&id)?.insert_before_markers(text);

        for sibling_id in shared_ids {
            if sibling_id == id {
                continue;
            }
            let update_state_fields =
                self.current == Some(sibling_id) || !self.buffer_has_state_markers(sibling_id);
            let sibling = self.buffers.get_mut(&sibling_id)?;
            Self::adjust_shared_insert_metadata(
                sibling,
                insert_pos,
                insert_char_pos,
                byte_len,
                char_len,
                update_state_fields,
                true,
            );
            self.refresh_shared_buffer_state_cache(sibling_id, update_state_fields)?;
        }
        self.sync_shared_undo_binding_cache(root_id)?;
        Some(())
    }

    pub fn delete_buffer_region(&mut self, id: BufferId, start: usize, end: usize) -> Option<()> {
        if start >= end {
            return Some(());
        }

        let root_id = self.shared_text_root_id(id)?;
        let shared_ids = self.buffers_sharing_root_ids(root_id);
        let source = self.buffers.get(&id)?;
        let start_char = source.text.byte_to_char(start);
        let end_char = source.text.byte_to_char(end);
        self.buffers.get_mut(&id)?.delete_region(start, end);

        for sibling_id in shared_ids {
            if sibling_id == id {
                continue;
            }
            let update_state_fields =
                self.current == Some(sibling_id) || !self.buffer_has_state_markers(sibling_id);
            let sibling = self.buffers.get_mut(&sibling_id)?;
            Self::adjust_shared_delete_metadata(
                sibling,
                start,
                end,
                start_char,
                end_char,
                update_state_fields,
            );
            self.refresh_shared_buffer_state_cache(sibling_id, update_state_fields)?;
        }
        self.sync_shared_undo_binding_cache(root_id)?;
        Some(())
    }

    pub fn subst_char_in_buffer_region(
        &mut self,
        id: BufferId,
        start: usize,
        end: usize,
        from: char,
        to: char,
        noundo: bool,
    ) -> Option<bool> {
        if start >= end || from == to {
            return Some(false);
        }

        let root_id = self.shared_text_root_id(id)?;
        let shared_ids = self.buffers_sharing_root_ids(root_id);
        let changed_chars = {
            let source = self.buffers.get(&id)?;
            source.text.byte_to_char(end) - source.text.byte_to_char(start)
        };
        let changed = self
            .buffers
            .get_mut(&id)?
            .subst_char_in_region(start, end, from, to, noundo);
        if !changed {
            return Some(false);
        }

        for sibling_id in shared_ids {
            if sibling_id == id {
                continue;
            }
            let sibling = self.buffers.get_mut(&sibling_id)?;
            Self::adjust_shared_same_len_edit_metadata(sibling, changed_chars, noundo);
        }
        if !noundo {
            self.sync_shared_undo_binding_cache(root_id)?;
        }
        Some(true)
    }

    pub fn delete_all_buffer_overlays(&mut self, id: BufferId) -> Option<()> {
        let buf = self.buffers.get_mut(&id)?;
        let ids = buf
            .overlays
            .overlays_in(buf.point_min_byte(), buf.point_max_byte());
        for ov_id in ids {
            buf.overlays.delete_overlay(ov_id);
        }
        Some(())
    }

    pub fn delete_buffer_overlay(&mut self, id: BufferId, overlay_id: Value) -> Option<()> {
        self.buffers
            .get_mut(&id)?
            .overlays
            .delete_overlay(overlay_id);
        Some(())
    }

    pub fn put_buffer_overlay_property(
        &mut self,
        id: BufferId,
        overlay_id: Value,
        name: Value,
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
        self.buffers.get_mut(&id)?.narrow_to_byte_region(start, end);
        let _ = self.record_buffer_state_markers(id);
        Some(())
    }

    pub fn widen_buffer(&mut self, id: BufferId) -> Option<()> {
        self.buffers.get(&id)?;
        let Some(restriction) = self.labeled_restriction_at(id, false).copied() else {
            return self.widen_buffer_fully(id);
        };
        let Some((begv, zv)) = self.labeled_restriction_bounds(id, false) else {
            self.replace_labeled_restrictions(id, None);
            return self.widen_buffer_fully(id);
        };
        self.restore_buffer_restriction(id, begv, zv)?;
        if matches!(restriction.label, LabeledRestrictionLabel::Outermost) {
            let _ = self.pop_labeled_restriction(id);
        }
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
            buf.goto_byte(0);
        }
        if !text.is_empty() {
            self.insert_into_buffer(id, text)?;
            self.goto_buffer_byte(id, 0)?;
        }
        Some(())
    }

    pub fn clear_buffer_local_properties(
        &mut self,
        id: BufferId,
        obarray: &crate::emacs_core::symbol::Obarray,
        kill_permanent: bool,
    ) -> Option<()> {
        let buf = self.buffers.get_mut(&id)?;
        buf.kill_all_local_variables(obarray, kill_permanent);
        Some(())
    }

    pub fn put_buffer_text_property(
        &mut self,
        id: BufferId,
        start: usize,
        end: usize,
        name: &str,
        value: Value,
    ) -> Option<bool> {
        let buf = self.buffers.get_mut(&id)?;
        // Record old value for undo before changing.
        if !buf.undo_state.in_progress() && !undo::undo_list_is_disabled(&buf.get_undo_list()) {
            let old_val = buf
                .text
                .text_props_get_property(start, name)
                .unwrap_or(Value::NIL);
            let mut ul = buf.get_undo_list();
            undo::undo_list_record_property_change(
                &mut ul,
                Value::symbol(name),
                old_val,
                start,
                end,
            );
            buf.set_undo_list(ul);
        }
        Some(buf.text.text_props_put_property(start, end, name, value))
    }

    pub fn append_buffer_text_properties(
        &mut self,
        id: BufferId,
        table: &TextPropertyTable,
        byte_offset: usize,
    ) -> Option<()> {
        self.buffers
            .get_mut(&id)?
            .text
            .text_props_append_shifted(table, byte_offset);
        Some(())
    }

    pub fn remove_buffer_text_property(
        &mut self,
        id: BufferId,
        start: usize,
        end: usize,
        name: &str,
    ) -> Option<bool> {
        let buf = self.buffers.get_mut(&id)?;
        // Record old value for undo before removing.
        if !buf.undo_state.in_progress() && !undo::undo_list_is_disabled(&buf.get_undo_list()) {
            let old_val = buf
                .text
                .text_props_get_property(start, name)
                .unwrap_or(Value::NIL);
            // Only record if property actually exists.
            if !old_val.is_nil() {
                let mut ul = buf.get_undo_list();
                undo::undo_list_record_property_change(
                    &mut ul,
                    Value::symbol(name),
                    old_val,
                    start,
                    end,
                );
                buf.set_undo_list(ul);
            }
        }
        Some(buf.text.text_props_remove_property(start, end, name))
    }

    pub fn clear_buffer_text_properties(
        &mut self,
        id: BufferId,
        start: usize,
        end: usize,
    ) -> Option<()> {
        self.buffers
            .get_mut(&id)?
            .text
            .text_props_remove_all(start, end);
        Some(())
    }

    pub fn set_buffer_multibyte_flag(&mut self, id: BufferId, flag: bool) -> Option<()> {
        let buf = self.buffers.get_mut(&id)?;
        buf.multibyte = flag;
        buf.set_buffer_local(
            "enable-multibyte-characters",
            if flag {
                crate::emacs_core::value::Value::T
            } else {
                crate::emacs_core::value::Value::NIL
            },
        );
        Some(())
    }

    pub fn set_buffer_modified_flag(&mut self, id: BufferId, flag: bool) -> Option<()> {
        let root_id = self.modified_state_root_id(id)?;
        self.buffers.get_mut(&root_id)?.set_modified(flag);
        self.sync_shared_modification_state(root_id)
    }

    pub fn restore_buffer_modified_state(&mut self, id: BufferId, flag: Value) -> Option<Value> {
        let root_id = self.modified_state_root_id(id)?;
        let out = self.buffers.get_mut(&root_id)?.restore_modified_state(flag);
        self.sync_shared_modification_state(root_id)?;
        Some(out)
    }

    pub fn set_buffer_auto_saved(&mut self, id: BufferId) -> Option<()> {
        self.buffers.get_mut(&id)?.mark_auto_saved();
        Some(())
    }

    pub fn set_buffer_modified_tick(&mut self, id: BufferId, tick: i64) -> Option<()> {
        let root_id = self.modified_state_root_id(id)?;
        let buf = self.buffers.get_mut(&root_id)?;
        buf.modified_tick = tick;
        buf.sync_modified_flag();
        self.sync_shared_modification_state(root_id)
    }

    pub fn set_buffer_file_name(&mut self, id: BufferId, file_name: Option<String>) -> Option<()> {
        let buf = self.buffers.get_mut(&id)?;
        buf.file_name = file_name.clone();
        match file_name {
            Some(file_name) => {
                buf.locals.set_raw_binding(
                    "buffer-file-name",
                    RuntimeBindingValue::Bound(Value::string(&file_name)),
                );
                buf.locals.set_raw_binding(
                    "buffer-file-truename",
                    RuntimeBindingValue::Bound(Value::string(&file_name)),
                );
            }
            None => {
                buf.locals
                    .set_raw_binding("buffer-file-name", RuntimeBindingValue::Bound(Value::NIL));
                buf.locals.set_raw_binding(
                    "buffer-file-truename",
                    RuntimeBindingValue::Bound(Value::NIL),
                );
            }
        }
        Some(())
    }

    pub fn set_buffer_name(&mut self, id: BufferId, name: String) -> Option<()> {
        self.buffers.get_mut(&id)?.name = name;
        Some(())
    }

    pub fn set_buffer_mark(&mut self, id: BufferId, pos: usize) -> Option<()> {
        self.buffers.get_mut(&id)?.set_mark_byte(pos);
        Some(())
    }

    pub fn clear_buffer_mark(&mut self, id: BufferId) -> Option<()> {
        let buf = self.buffers.get_mut(&id)?;
        buf.mark = None;
        buf.mark_char = None;
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

    pub fn buffer_local_map(&self, id: BufferId) -> Option<Value> {
        Some(self.buffers.get(&id)?.local_map())
    }

    pub fn current_local_map(&self) -> Value {
        self.current
            .and_then(|id| self.buffer_local_map(id))
            .unwrap_or(Value::NIL)
    }

    pub fn set_buffer_local_map(&mut self, id: BufferId, keymap: Value) -> Option<()> {
        self.buffers.get_mut(&id)?.set_local_map(keymap);
        Some(())
    }

    pub fn set_current_local_map(&mut self, keymap: Value) -> Option<()> {
        let id = self.current?;
        self.set_buffer_local_map(id, keymap)
    }

    pub fn set_buffer_local_void_property(&mut self, id: BufferId, name: &str) -> Option<()> {
        self.buffers.get_mut(&id)?.set_buffer_local_void(name);
        Some(())
    }

    pub fn remove_buffer_local_property(
        &mut self,
        id: BufferId,
        name: &str,
    ) -> Option<Option<RuntimeBindingValue>> {
        let buf = self.buffers.get_mut(&id)?;
        Some(buf.kill_buffer_local(name))
    }

    pub fn add_undo_boundary(&mut self, id: BufferId) -> Option<()> {
        let root_id = self.shared_text_root_id(id)?;
        let buf = self.buffers.get_mut(&id)?;
        let mut ul = buf.get_undo_list();
        undo::undo_list_boundary(&mut ul);
        // Periodically truncate the undo list to avoid unbounded growth.
        // Default limits match GNU Emacs: undo-limit=160000, undo-strong-limit=240000.
        ul = undo::truncate_undo_list(ul, 160_000, 240_000);
        buf.set_undo_list(ul);
        self.sync_shared_undo_binding_cache(root_id)?;
        Some(())
    }

    pub fn restore_buffer_restriction(
        &mut self,
        id: BufferId,
        begv: usize,
        zv: usize,
    ) -> Option<()> {
        self.buffers.get_mut(&id)?.narrow_to_byte_region(begv, zv);
        let _ = self.record_buffer_state_markers(id);
        Some(())
    }

    pub fn save_current_restriction_state(&mut self) -> Option<SavedRestrictionState> {
        let buffer_id = self.current_buffer_id()?;
        let (begv, zv, len) = {
            let buffer = self.get(buffer_id)?;
            (buffer.begv, buffer.zv, buffer.text.len())
        };
        let restriction = if begv == 0 && zv == len {
            SavedRestrictionKind::None
        } else {
            let beg_marker = self.create_marker(buffer_id, begv, InsertionType::Before);
            let end_marker = self.create_marker(buffer_id, zv, InsertionType::After);
            SavedRestrictionKind::Markers {
                beg_marker,
                end_marker,
            }
        };
        let labeled_restrictions = self.clone_labeled_restrictions(buffer_id).unwrap_or(None);
        Some(SavedRestrictionState {
            buffer_id,
            restriction,
            labeled_restrictions,
        })
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub fn reset_outermost_restrictions(&mut self) -> OutermostRestrictionResetState {
        let mut affected_buffers: Vec<BufferId> =
            self.labeled_restrictions.keys().copied().collect();
        affected_buffers.sort_by_key(|buffer_id| buffer_id.0);

        let mut retained_buffers = Vec::with_capacity(affected_buffers.len());
        for buffer_id in affected_buffers {
            let Some((begv, zv)) = self.labeled_restriction_bounds(buffer_id, true) else {
                self.replace_labeled_restrictions(buffer_id, None);
                continue;
            };
            if self
                .restore_buffer_restriction(buffer_id, begv, zv)
                .is_some()
            {
                retained_buffers.push(buffer_id);
            } else {
                self.replace_labeled_restrictions(buffer_id, None);
            }
        }

        OutermostRestrictionResetState {
            affected_buffers: retained_buffers,
        }
    }

    #[tracing::instrument(level = "trace", skip(self, state))]
    pub fn restore_outermost_restrictions(&mut self, state: OutermostRestrictionResetState) {
        for buffer_id in state.affected_buffers {
            if let Some((begv, zv)) = self.current_labeled_restriction_bounds(buffer_id) {
                let _ = self.restore_buffer_restriction(buffer_id, begv, zv);
            } else {
                self.replace_labeled_restrictions(buffer_id, None);
            }
        }
    }

    pub fn restore_saved_restriction_state(&mut self, saved: SavedRestrictionState) {
        let buffer_id = saved.buffer_id;
        if self.buffers.get(&buffer_id).is_none() {
            self.replace_labeled_restrictions(buffer_id, None);
            return;
        }
        self.replace_labeled_restrictions(buffer_id, saved.labeled_restrictions);
        match saved.restriction {
            SavedRestrictionKind::None => {
                let _ = self.widen_buffer_fully(buffer_id);
            }
            SavedRestrictionKind::Markers {
                beg_marker,
                end_marker,
            } => {
                let beg = self.marker_position(buffer_id, beg_marker);
                let end = self.marker_position(buffer_id, end_marker);
                if let (Some(begv), Some(zv), Some(len)) = (
                    beg,
                    end,
                    self.buffers.get(&buffer_id).map(|buffer| buffer.text.len()),
                ) {
                    let mut restored_begv = begv.min(len);
                    let mut restored_zv = zv.min(len);
                    if restored_begv > restored_zv {
                        std::mem::swap(&mut restored_begv, &mut restored_zv);
                    }
                    let _ = self.restore_buffer_restriction(buffer_id, restored_begv, restored_zv);
                }
                self.remove_marker(beg_marker);
                self.remove_marker(end_marker);
            }
        }
    }

    pub fn internal_labeled_narrow_to_region(
        &mut self,
        buffer_id: BufferId,
        start: usize,
        end: usize,
        label: Value,
    ) -> Option<()> {
        self.buffers.get(&buffer_id)?;
        if self.labeled_restriction_at(buffer_id, false).is_none() {
            self.push_labeled_restriction_for_current_bounds(
                buffer_id,
                LabeledRestrictionLabel::Outermost,
            )?;
        }
        self.restore_buffer_restriction(buffer_id, start, end)?;
        self.push_labeled_restriction_for_current_bounds(
            buffer_id,
            LabeledRestrictionLabel::User(label),
        )?;
        Some(())
    }

    pub fn internal_labeled_widen(&mut self, buffer_id: BufferId, label: &Value) -> Option<()> {
        self.buffers.get(&buffer_id)?;
        if self.current_labeled_restriction_matches_label(buffer_id, label) {
            let _ = self.pop_labeled_restriction(buffer_id);
        }
        self.widen_buffer(buffer_id)
    }

    pub fn configure_buffer_undo_list(&mut self, id: BufferId, value: Value) -> Option<()> {
        let root_id = self.shared_text_root_id(id)?;
        {
            let buf = self.buffers.get_mut(&id)?;
            match value.kind() {
                ValueKind::T => {
                    buf.set_buffer_local("buffer-undo-list", Value::T);
                }
                ValueKind::Nil => {
                    buf.set_buffer_local("buffer-undo-list", Value::NIL);
                    buf.undo_state.set_recorded_first_change(false);
                }
                other => {
                    buf.set_buffer_local("buffer-undo-list", value);
                }
            }
        }
        self.sync_shared_undo_binding_cache(root_id)?;
        Some(())
    }

    pub fn undo_buffer(&mut self, id: BufferId, mut count: i64) -> Option<UndoExecutionResult> {
        let (had_any_records, had_boundary, previous_undoing, groups) = {
            let buffer = self.buffers.get_mut(&id)?;
            let ul = buffer.get_undo_list();

            let had_any_records = !undo::undo_list_is_empty(&ul);
            let had_boundary = undo::undo_list_contains_boundary(&ul);
            let had_trailing_boundary = undo::undo_list_has_trailing_boundary(&ul);

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

            let previous_undoing = buffer.undo_state.in_progress();
            buffer.undo_state.set_in_progress(true);
            let groups_to_undo = if had_trailing_boundary {
                count as usize
            } else {
                (count as usize).saturating_add(1)
            };

            let mut current_ul = ul;
            let mut groups = Vec::new();
            for _ in 0..groups_to_undo {
                let group = undo::undo_list_pop_group(&mut current_ul);
                if group.is_empty() {
                    break;
                }
                groups.push(group);
            }
            buffer.set_undo_list(current_ul);

            (had_any_records, had_boundary, previous_undoing, groups)
        };

        let mut applied_any = false;
        for group in groups {
            applied_any = true;
            for entry in group {
                if let Some(pt1) = entry.as_fixnum() {
                    // Cursor position (1-indexed)
                    let pos = (pt1 - 1).max(0) as usize;
                    let clamped = self
                        .buffers
                        .get(&id)
                        .map(|buffer| pos.min(buffer.text.len()))?;
                    self.goto_buffer_byte(id, clamped)?;
                } else if entry.is_cons() {
                    let car = entry.cons_car();
                    let cdr = entry.cons_cdr();
                    match (car.kind(), cdr.kind()) {
                        (ValueKind::Fixnum(beg1), ValueKind::Fixnum(end1)) => {
                            // Insert record: (BEG . END) — to undo, delete [beg, end)
                            let beg = (beg1 - 1).max(0) as usize;
                            let end = (end1 - 1).max(0) as usize;
                            let clamped_end = self
                                .buffers
                                .get(&id)
                                .map(|buffer| end.min(buffer.text.len()))?;
                            self.delete_buffer_region(id, beg.min(clamped_end), clamped_end)?;
                        }
                        (ValueKind::String, ValueKind::Fixnum(pos1)) => {
                            // Delete record: (TEXT . POS) — to undo, re-insert text
                            let text = car.as_str_owned().unwrap_or_default();
                            let pos = (pos1.abs() - 1).max(0) as usize;
                            let clamped = self
                                .buffers
                                .get(&id)
                                .map(|buffer| pos.min(buffer.text.len()))?;
                            self.goto_buffer_byte(id, clamped)?;
                            self.insert_into_buffer(id, &text)?;
                        }
                        (ValueKind::T, ValueKind::Fixnum(_)) => {
                            // First-change sentinel (t . MODTIME) — skip
                        }
                        _ => {
                            // Other cons entries (e.g. property changes) — skip
                        }
                    }
                }
                // nil entries (boundaries within a group) are skipped
            }
        }

        self.buffers
            .get_mut(&id)?
            .undo_state
            .set_in_progress(previous_undoing);
        let root_id = self.shared_text_root_id(id)?;
        self.sync_shared_undo_binding_cache(root_id)?;
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
        self.generate_new_buffer_name_ignoring(base, None)
    }

    /// Generate a unique buffer name, allowing `ignore` to be reused even if
    /// a live buffer already owns that name.
    pub fn generate_new_buffer_name_ignoring(&self, base: &str, ignore: Option<&str>) -> String {
        if ignore == Some(base) || self.find_buffer_by_name(base).is_none() {
            return base.to_string();
        }
        let mut n = 2u64;
        loop {
            let candidate = format!("{}<{}>", base, n);
            if ignore == Some(candidate.as_str()) || self.find_buffer_by_name(&candidate).is_none()
            {
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
        buf.register_marker(marker_id, pos, insertion_type);
        Some(())
    }

    /// Query the current byte position of a marker.
    pub fn marker_position(&self, buffer_id: BufferId, marker_id: u64) -> Option<usize> {
        self.buffers
            .get(&buffer_id)
            .and_then(|buf| buf.marker_entry(marker_id).map(|marker| marker.byte_pos))
    }

    /// Query the current character position of a marker.
    pub fn marker_char_position(&self, buffer_id: BufferId, marker_id: u64) -> Option<usize> {
        self.buffers
            .get(&buffer_id)
            .and_then(|buf| buf.marker_entry(marker_id).map(|marker| marker.char_pos))
    }

    /// Remove a marker registration from any live buffer.
    pub fn remove_marker(&mut self, marker_id: u64) {
        for buf in self.buffers.values_mut() {
            buf.remove_marker_entry(marker_id);
        }
    }

    /// Update the insertion type of a registered marker across all buffers.
    pub fn update_marker_insertion_type(&mut self, marker_id: u64, ins_type: InsertionType) {
        for buf in self.buffers.values_mut() {
            if buf.marker_entry(marker_id).is_some() {
                buf.update_marker_insertion_type(marker_id, ins_type);
                return;
            }
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
        mut buffers: HashMap<BufferId, Buffer>,
        current: Option<BufferId>,
        next_id: u64,
        next_marker_id: u64,
    ) -> Self {
        let indirect_buffers: Vec<(BufferId, BufferId)> = buffers
            .iter()
            .filter_map(|(id, buffer)| buffer.base_buffer.map(|base_id| (*id, base_id)))
            .collect();
        for (buffer_id, base_id) in indirect_buffers {
            let Some(root) = buffers.get(&base_id).cloned() else {
                continue;
            };
            let Some(buffer) = buffers.get_mut(&buffer_id) else {
                continue;
            };
            buffer.text = root.text.shared_clone();
            buffer.undo_state = root.undo_state.clone();
        }

        let mut manager = Self {
            buffers,
            current,
            next_id,
            next_marker_id,
            labeled_restrictions: HashMap::new(),
            dead_buffer_last_names: HashMap::new(),
        };
        let root_ids: Vec<BufferId> = manager
            .buffers
            .values()
            .map(|buffer| buffer.base_buffer.unwrap_or(buffer.id))
            .collect();
        for root_id in root_ids {
            let _ = manager.sync_shared_undo_binding_cache(root_id);
        }
        manager
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
            buffer.locals.trace_roots(roots);
            buffer.text.trace_text_prop_roots(roots);
            buffer.undo_state.trace_roots(roots);
            buffer.overlays.trace_roots(roots);
        }
        for restrictions in self.labeled_restrictions.values() {
            for restriction in restrictions {
                if let LabeledRestrictionLabel::User(label) = restriction.label {
                    roots.push(label);
                }
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
        crate::test_utils::init_test_tracing();
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
        crate::test_utils::init_test_tracing();
        let a = BufferId(1);
        let b = BufferId(1);
        let c = BufferId(2);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn create_indirect_buffer_shares_root_text_and_updates_siblings() {
        crate::test_utils::init_test_tracing();
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
        crate::test_utils::init_test_tracing();
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
        crate::test_utils::init_test_tracing();
        let mut mgr = BufferManager::new();
        let base_id = mgr.current_buffer_id().expect("scratch buffer");
        let indirect_id = mgr
            .create_indirect_buffer(base_id, "*indirect-undo*", false)
            .expect("indirect buffer");

        let _ = mgr.insert_into_buffer(base_id, "abc");
        {
            let undo_val = mgr
                .get(indirect_id)
                .and_then(|buf| buf.buffer_local_value("buffer-undo-list"));
            assert!(
                undo_val.is_some() && !undo_val.unwrap().is_nil(),
                "indirect buffer should observe the base buffer's undo history"
            );
        }

        let result = mgr.undo_buffer(indirect_id, 1).expect("undo result");
        assert!(result.applied_any);
        assert_eq!(mgr.get(base_id).unwrap().buffer_string(), "");
        assert_eq!(mgr.get(indirect_id).unwrap().buffer_string(), "");
    }

    #[test]
    fn from_dump_restores_indirect_buffer_shared_text_state() {
        crate::test_utils::init_test_tracing();
        let mut mgr = BufferManager::new();
        let base_id = mgr.current_buffer_id().expect("scratch buffer");
        let _ = mgr.insert_into_buffer(base_id, "abcdef");
        let indirect_id = mgr
            .create_indirect_buffer(base_id, "*indirect-restored*", false)
            .expect("indirect buffer");
        let _ = mgr.put_buffer_text_property(base_id, 1, 4, "face", Value::symbol("bold"));
        let _ = mgr.insert_into_buffer(base_id, "z");

        let mut dumped = mgr.dump_buffers().clone();
        let independent_indirect = dumped.get(&indirect_id).expect("indirect buffer").clone();
        let indirect = dumped.get_mut(&indirect_id).expect("indirect buffer");
        indirect.text = BufferText::from_dump(independent_indirect.text.dump_text());
        indirect
            .text
            .text_props_replace(independent_indirect.text.text_props_snapshot());
        indirect.undo_state =
            SharedUndoState::from_parts(independent_indirect.get_undo_list(), false, false);

        let restored = BufferManager::from_dump(
            dumped,
            mgr.dump_current(),
            mgr.dump_next_id(),
            mgr.dump_next_marker_id(),
        );

        let base = restored.get(base_id).expect("base buffer");
        let indirect = restored.get(indirect_id).expect("indirect buffer");
        assert!(base.text.shares_storage_with(&indirect.text));
        assert!(base.undo_state.shares_with(&indirect.undo_state));
        assert_eq!(
            indirect.text.text_props_get_property(1, "face"),
            Some(Value::symbol("bold"))
        );
    }

    #[test]
    fn indirect_buffers_preserve_narrowing_across_shared_edits() {
        crate::test_utils::init_test_tracing();
        let mut mgr = BufferManager::new();
        let base_id = mgr.current_buffer_id().expect("scratch buffer");
        let _ = mgr.insert_into_buffer(base_id, "abcdef");
        let indirect_id = mgr
            .create_indirect_buffer(base_id, "*indirect-narrow*", false)
            .expect("indirect buffer");

        let _ = mgr.narrow_buffer_to_region(indirect_id, 2, 6);
        let _ = mgr.goto_buffer_byte(indirect_id, 4);

        let _ = mgr.goto_buffer_byte(base_id, 0);
        let _ = mgr.insert_into_buffer(base_id, "zz");

        let indirect = mgr.get(indirect_id).expect("indirect buffer");
        assert_eq!(indirect.point_min(), 4);
        assert_eq!(indirect.point_max(), 8);
        assert_eq!(indirect.point(), 6);
        assert_eq!(indirect.buffer_string(), "cdef");

        let _ = mgr.delete_buffer_region(base_id, 0, 2);

        let indirect = mgr.get(indirect_id).expect("indirect buffer");
        assert_eq!(indirect.point_min(), 2);
        assert_eq!(indirect.point_max(), 6);
        assert_eq!(indirect.point(), 4);
        assert_eq!(indirect.buffer_string(), "cdef");
    }

    // -----------------------------------------------------------------------
    // Point movement
    // -----------------------------------------------------------------------

    #[test]
    fn goto_char_clamps_to_accessible_region() {
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("hello");
        buf.goto_char(3);
        assert_eq!(buf.point(), 3);

        // Past end — clamped to zv.
        buf.goto_char(999);
        assert_eq!(buf.point(), buf.point_max());

        // Before start — clamped to begv.
        buf.goto_char(0);
        buf.narrow_to_byte_region(2, buf.point_max_byte());
        buf.goto_char(0);
        assert_eq!(buf.point(), 2);
    }

    #[test]
    fn point_char_converts_byte_to_char_pos() {
        crate::test_utils::init_test_tracing();
        // "cafe\u{0301}" — 'e' + combining acute = 5 bytes, 5 chars in UTF-8
        let mut buf = buf_with_text("hello");
        buf.goto_char(3);
        assert_eq!(buf.point_char(), 3);
    }

    #[test]
    fn byte_position_aliases_match_legacy_buffer_apis() {
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("hello world");
        buf.narrow_to_byte_region(2, 9);
        buf.goto_byte(7);
        buf.set_mark_byte(4);

        assert_eq!(buf.point_byte(), buf.point());
        assert_eq!(buf.point_min_byte(), buf.point_min());
        assert_eq!(buf.point_max_byte(), buf.point_max());
        assert_eq!(buf.mark_byte(), buf.mark());
    }

    #[test]
    fn cached_char_positions_track_multibyte_edits_and_narrowing() {
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("ééz");
        assert_eq!(buf.point_max_char(), 3);

        buf.goto_byte('é'.len_utf8());
        assert_eq!(buf.point_char(), 1);

        buf.insert("ß");
        assert_eq!(buf.point_byte(), 4);
        assert_eq!(buf.point_char(), 2);
        assert_eq!(buf.point_max_char(), 4);

        buf.narrow_to_byte_region('é'.len_utf8(), buf.point_max_byte());
        assert_eq!(buf.point_min_char(), 1);
        assert_eq!(buf.point_max_char(), 4);

        buf.delete_region(2, 4);
        assert_eq!(buf.point_byte(), 2);
        assert_eq!(buf.point_char(), 1);
        assert_eq!(buf.point_max_char(), 3);
        assert_eq!(buf.buffer_string(), "éz");
    }

    #[test]
    fn char_position_conversions_clamp_to_buffer_and_accessible_bounds() {
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("ééz");
        assert_eq!(buf.total_chars(), 3);
        assert_eq!(buf.char_to_byte_clamped(99), "ééz".len());
        assert_eq!(buf.lisp_pos_to_byte(99), "ééz".len());

        buf.narrow_to_byte_region('é'.len_utf8(), "ééz".len());
        assert_eq!(buf.point_min_char(), 1);
        assert_eq!(buf.point_max_char(), 3);
        assert_eq!(buf.lisp_pos_to_accessible_byte(1), 'é'.len_utf8());
        assert_eq!(buf.lisp_pos_to_accessible_byte(99), "ééz".len());
    }

    // -----------------------------------------------------------------------
    // Insertion
    // -----------------------------------------------------------------------

    #[test]
    fn insert_at_point_advances_point() {
        crate::test_utils::init_test_tracing();
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
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("helo");
        buf.goto_char(3);
        buf.insert("l");
        assert_eq!(buf.buffer_string(), "hello");
        assert_eq!(buf.point(), 4);
    }

    #[test]
    fn insert_adjusts_mark() {
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("ab");
        buf.set_mark(1);
        buf.goto_char(0);
        buf.insert("X");
        // Mark was at 1, insert at 0 pushes it to 2.
        assert_eq!(buf.mark(), Some(2));
        assert_eq!(buf.mark_char(), Some(2));
    }

    #[test]
    fn insert_empty_string_is_noop() {
        crate::test_utils::init_test_tracing();
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
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("hello world");
        buf.goto_char(11); // at end
        buf.delete_region(5, 11);
        assert_eq!(buf.buffer_string(), "hello");
        assert_eq!(buf.point(), 5); // was past deleted range
    }

    #[test]
    fn delete_region_adjusts_point_inside() {
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("abcdef");
        buf.goto_char(3); // in middle of deleted range
        buf.delete_region(1, 5);
        assert_eq!(buf.point(), 1); // collapsed to start of deletion
        assert_eq!(buf.buffer_string(), "af");
    }

    #[test]
    fn delete_region_adjusts_point_at_end_boundary() {
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("abcdef");
        buf.goto_char(5);
        buf.delete_region(1, 5);
        assert_eq!(buf.point(), 1);
        assert_eq!(buf.point_char(), 1);
    }

    #[test]
    fn delete_region_adjusts_mark() {
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("abcdef");
        buf.set_mark(4);
        buf.delete_region(1, 3);
        // mark was at 4, past deleted range end (3), so shifts by 2
        assert_eq!(buf.mark(), Some(2));
        assert_eq!(buf.mark_char(), Some(2));
    }

    #[test]
    fn delete_region_moves_marker_at_end_to_start() {
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("0123456789ABCDEF");
        buf.register_marker(1, 12, InsertionType::Before);
        buf.delete_region(5, 12);
        let marker = buf.marker_entry(1).expect("marker");
        assert_eq!(marker.byte_pos, 5);
        assert_eq!(marker.char_pos, 5);
    }

    #[test]
    fn mark_char_tracks_multibyte_edits() {
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("ééz");
        buf.set_mark_byte('é'.len_utf8());
        buf.goto_byte('é'.len_utf8());
        buf.insert("ß");
        assert_eq!(buf.mark(), Some(2));
        assert_eq!(buf.mark_char(), Some(1));

        buf.delete_region(0, 2);
        assert_eq!(buf.mark(), Some(0));
        assert_eq!(buf.mark_char(), Some(0));
    }

    #[test]
    fn delete_region_adjusts_zv() {
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("abcdef");
        assert_eq!(buf.zv, 6);
        buf.delete_region(2, 4);
        assert_eq!(buf.zv, 4);
    }

    #[test]
    fn delete_empty_range_is_noop() {
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("hello");
        buf.delete_region(2, 2);
        assert_eq!(buf.buffer_string(), "hello");
    }

    // -----------------------------------------------------------------------
    // Substring / buffer_string
    // -----------------------------------------------------------------------

    #[test]
    fn buffer_substring_range() {
        crate::test_utils::init_test_tracing();
        let buf = buf_with_text("hello world");
        assert_eq!(buf.buffer_substring(6, 11), "world");
    }

    #[test]
    fn buffer_string_returns_accessible() {
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("hello world");
        buf.narrow_to_region(6, 11);
        assert_eq!(buf.buffer_string(), "world");
    }

    // -----------------------------------------------------------------------
    // char_after / char_before
    // -----------------------------------------------------------------------

    #[test]
    fn char_after_basic() {
        crate::test_utils::init_test_tracing();
        let buf = buf_with_text("hello");
        assert_eq!(buf.char_after(0), Some('h'));
        assert_eq!(buf.char_after(4), Some('o'));
        assert_eq!(buf.char_after(5), None);
    }

    #[test]
    fn char_before_basic() {
        crate::test_utils::init_test_tracing();
        let buf = buf_with_text("hello");
        assert_eq!(buf.char_before(0), None);
        assert_eq!(buf.char_before(1), Some('h'));
        assert_eq!(buf.char_before(5), Some('o'));
    }

    #[test]
    fn char_after_multibyte() {
        crate::test_utils::init_test_tracing();
        // Each Chinese character is 3 bytes in UTF-8.
        let buf = buf_with_text("\u{4f60}\u{597d}"); // "nihao" in Chinese
        assert_eq!(buf.char_after(0), Some('\u{4f60}'));
        assert_eq!(buf.char_after(3), Some('\u{597d}'));
    }

    #[test]
    fn char_before_multibyte() {
        crate::test_utils::init_test_tracing();
        let buf = buf_with_text("\u{4f60}\u{597d}");
        assert_eq!(buf.char_before(3), Some('\u{4f60}'));
        assert_eq!(buf.char_before(6), Some('\u{597d}'));
    }

    // -----------------------------------------------------------------------
    // Narrowing
    // -----------------------------------------------------------------------

    #[test]
    fn narrow_and_widen() {
        crate::test_utils::init_test_tracing();
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
        crate::test_utils::init_test_tracing();
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
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("ab");
        buf.register_marker(1, 1, InsertionType::After);
        buf.goto_char(1);
        buf.insert("XY");
        // Marker was at 1 with After => advances to 3.
        let marker = buf.marker_entry(1).expect("marker");
        assert_eq!(marker.byte_pos, 3);
        assert_eq!(marker.char_pos, 3);
    }

    #[test]
    fn marker_stays_on_insertion_before() {
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("ab");
        buf.register_marker(1, 1, InsertionType::Before);
        buf.goto_char(1);
        buf.insert("XY");
        // Marker was at 1 with Before => stays at 1.
        let marker = buf.marker_entry(1).expect("marker");
        assert_eq!(marker.byte_pos, 1);
        assert_eq!(marker.char_pos, 1);
    }

    #[test]
    fn marker_adjusts_on_deletion() {
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("abcdef");
        buf.register_marker(1, 4, InsertionType::After);
        buf.delete_region(1, 3);
        // Marker was at 4 (past deleted range [1,3)), shifts by 2 => 2.
        let marker = buf.marker_entry(1).expect("marker");
        assert_eq!(marker.byte_pos, 2);
        assert_eq!(marker.char_pos, 2);
    }

    #[test]
    fn marker_inside_deleted_range_collapses() {
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("abcdef");
        buf.register_marker(1, 2, InsertionType::After);
        buf.delete_region(1, 5);
        // Marker at 2 inside [1,5) => collapses to 1.
        let marker = buf.marker_entry(1).expect("marker");
        assert_eq!(marker.byte_pos, 1);
        assert_eq!(marker.char_pos, 1);
    }

    #[test]
    fn marker_char_pos_tracks_multibyte_edits() {
        crate::test_utils::init_test_tracing();
        let mut buf = buf_with_text("ééz");
        buf.register_marker(1, 'é'.len_utf8(), InsertionType::After);
        buf.goto_byte('é'.len_utf8());
        buf.insert("ß");
        let marker = buf.marker_entry(1).expect("marker");
        assert_eq!(marker.byte_pos, 4);
        assert_eq!(marker.char_pos, 2);

        buf.delete_region(2, 4);
        let marker = buf.marker_entry(1).expect("marker");
        assert_eq!(marker.byte_pos, 2);
        assert_eq!(marker.char_pos, 1);
    }

    // -----------------------------------------------------------------------
    // Buffer-local variables
    // -----------------------------------------------------------------------

    #[test]
    fn buffer_local_get_set() {
        crate::test_utils::init_test_tracing();
        let mut buf = Buffer::new(BufferId(1), "test".into());
        assert!(buf.get_buffer_local("tab-width").is_none());

        buf.set_buffer_local("tab-width", Value::fixnum(4));
        let val = buf.get_buffer_local("tab-width").unwrap();
        assert!(val.is_fixnum());

        buf.set_buffer_local("tab-width", Value::fixnum(8));
        let val = buf.get_buffer_local("tab-width").unwrap();
        assert!(val.is_fixnum());
    }

    #[test]
    fn buffer_local_multiple_vars() {
        crate::test_utils::init_test_tracing();
        let mut buf = Buffer::new(BufferId(1), "test".into());
        buf.set_buffer_local("fill-column", Value::fixnum(80));
        buf.set_buffer_local("major-mode", Value::symbol("text-mode"));

        assert!(buf.get_buffer_local("fill-column").is_some());
        assert!(buf.get_buffer_local("major-mode").is_some());
        assert!(buf.get_buffer_local("nonexistent").is_none());
    }

    #[test]
    fn buffer_local_defaults_include_builtin_per_buffer_vars() {
        crate::test_utils::init_test_tracing();
        let buf = Buffer::new(BufferId(1), "test".into());

        assert_eq!(
            buf.buffer_local_value("major-mode"),
            Some(Value::symbol("fundamental-mode"))
        );
        assert_eq!(
            buf.buffer_local_value("mode-name"),
            Some(Value::string("Fundamental"))
        );
        assert_eq!(buf.buffer_local_value("buffer-file-name"), Some(Value::NIL));
        assert_eq!(
            buf.buffer_local_value("buffer-auto-save-file-name"),
            Some(Value::NIL)
        );
        assert_eq!(
            buf.buffer_local_value("buffer-display-count"),
            Some(Value::fixnum(0))
        );
        assert_eq!(
            buf.buffer_local_value("buffer-display-time"),
            Some(Value::NIL)
        );
        assert_eq!(
            buf.buffer_local_value("buffer-invisibility-spec"),
            Some(Value::T)
        );
        assert_eq!(buf.buffer_local_value("buffer-undo-list"), Some(Value::NIL));
    }

    #[test]
    fn buffer_file_name_variable_tracks_slot_backed_state() {
        crate::test_utils::init_test_tracing();
        let mut buf = Buffer::new(BufferId(1), "test".into());
        assert_eq!(buf.buffer_local_value("buffer-file-name"), Some(Value::NIL));

        buf.set_buffer_local("buffer-file-name", Value::string("/tmp/demo.txt"));
        assert_eq!(buf.file_name.as_deref(), Some("/tmp/demo.txt"));
        assert_eq!(
            buf.buffer_local_value("buffer-file-name"),
            Some(Value::string("/tmp/demo.txt"))
        );

        buf.set_buffer_local("buffer-file-name", Value::NIL);
        assert_eq!(buf.file_name, None);
        assert_eq!(buf.buffer_local_value("buffer-file-name"), Some(Value::NIL));
    }

    #[test]
    fn buffer_auto_save_file_name_variable_tracks_slot_backed_state() {
        crate::test_utils::init_test_tracing();
        let mut buf = Buffer::new(BufferId(1), "test".into());
        assert_eq!(
            buf.buffer_local_value("buffer-auto-save-file-name"),
            Some(Value::NIL)
        );

        buf.set_buffer_local(
            "buffer-auto-save-file-name",
            Value::string("/tmp/#demo.txt#"),
        );
        assert_eq!(buf.auto_save_file_name.as_deref(), Some("/tmp/#demo.txt#"));
        assert_eq!(
            buf.buffer_local_value("buffer-auto-save-file-name"),
            Some(Value::string("/tmp/#demo.txt#"))
        );

        buf.set_buffer_local("buffer-auto-save-file-name", Value::NIL);
        assert_eq!(buf.auto_save_file_name, None);
        assert_eq!(
            buf.buffer_local_value("buffer-auto-save-file-name"),
            Some(Value::NIL)
        );
    }

    // -----------------------------------------------------------------------
    // Modified flag
    // -----------------------------------------------------------------------

    #[test]
    fn modified_flag() {
        crate::test_utils::init_test_tracing();
        let mut buf = Buffer::new(BufferId(1), "test".into());
        assert!(!buf.is_modified());
        buf.insert("x");
        assert!(buf.is_modified());
        buf.set_modified(false);
        assert!(!buf.is_modified());
    }

    #[test]
    fn modified_state_tracks_autosaved_semantics() {
        crate::test_utils::init_test_tracing();
        let mut buf = Buffer::new(BufferId(1), "test".into());
        assert_eq!(buf.modified_state_value(), Value::NIL);
        assert!(!buf.recent_auto_save_p());
        assert_eq!(buf.modified_tick, 1);
        assert_eq!(buf.chars_modified_tick, 1);

        assert_eq!(buf.restore_modified_state(Value::T), Value::T);
        assert_eq!(buf.modified_state_value(), Value::T);
        assert_eq!(buf.modified_tick, 2);
        assert_eq!(buf.chars_modified_tick, 1);
        assert!(!buf.recent_auto_save_p());

        assert_eq!(
            buf.restore_modified_state(Value::symbol("autosaved")),
            Value::symbol("autosaved")
        );
        assert_eq!(buf.modified_state_value(), Value::symbol("autosaved"));
        assert_eq!(buf.modified_tick, 2);
        assert_eq!(buf.chars_modified_tick, 1);
        assert!(buf.recent_auto_save_p());

        assert_eq!(buf.restore_modified_state(Value::NIL), Value::NIL);
        assert_eq!(buf.modified_state_value(), Value::NIL);
        assert_eq!(buf.modified_tick, 2);
        assert_eq!(buf.chars_modified_tick, 1);
        assert!(!buf.recent_auto_save_p());
    }

    #[test]
    fn modification_ticks_track_content_changes() {
        crate::test_utils::init_test_tracing();
        let mut buf = Buffer::new(BufferId(1), "test".into());
        assert_eq!(buf.modified_tick, 1);
        assert_eq!(buf.chars_modified_tick, 1);

        buf.insert("abcdef");
        assert_eq!(buf.modified_tick, 4);
        assert_eq!(buf.chars_modified_tick, 4);

        buf.set_modified(false);
        assert_eq!(buf.modified_tick, 4);
        assert_eq!(buf.chars_modified_tick, 4);
        assert_eq!(buf.modified_state_value(), Value::NIL);

        buf.delete_region(0, 6);
        assert_eq!(buf.modified_tick, 7);
        assert_eq!(buf.chars_modified_tick, 7);
        assert_eq!(buf.modified_state_value(), Value::T);
    }

    #[test]
    fn chars_modified_tick_rejoins_modiff_after_non_char_modification() {
        crate::test_utils::init_test_tracing();
        let mut buf = Buffer::new(BufferId(1), "test".into());
        assert_eq!(buf.restore_modified_state(Value::T), Value::T);
        assert_eq!(buf.modified_tick, 2);
        assert_eq!(buf.chars_modified_tick, 1);

        buf.insert("x");
        assert_eq!(buf.modified_tick, 3);
        assert_eq!(buf.chars_modified_tick, 3);
        assert_eq!(buf.modified_state_value(), Value::T);
    }

    // -----------------------------------------------------------------------
    // BufferManager — creation, lookup, kill
    // -----------------------------------------------------------------------

    #[test]
    fn manager_starts_with_scratch() {
        crate::test_utils::init_test_tracing();
        let mgr = BufferManager::new();
        let scratch = mgr.find_buffer_by_name("*scratch*");
        assert!(scratch.is_some());
        assert!(mgr.current_buffer().is_some());
        assert_eq!(mgr.current_buffer().unwrap().name, "*scratch*");
    }

    #[test]
    fn manager_create_and_lookup() {
        crate::test_utils::init_test_tracing();
        let mut mgr = BufferManager::new();
        let id = mgr.create_buffer("foo.el");
        assert!(mgr.get(id).is_some());
        assert_eq!(mgr.get(id).unwrap().name, "foo.el");
        assert_eq!(mgr.find_buffer_by_name("foo.el"), Some(id));
        assert_eq!(mgr.find_buffer_by_name("bar.el"), None);
    }

    #[test]
    fn manager_set_current() {
        crate::test_utils::init_test_tracing();
        let mut mgr = BufferManager::new();
        let a = mgr.create_buffer("a");
        let b = mgr.create_buffer("b");
        mgr.set_current(a);
        assert_eq!(mgr.current_buffer().unwrap().name, "a");
        mgr.set_current(b);
        assert_eq!(mgr.current_buffer().unwrap().name, "b");
    }

    #[test]
    fn switch_current_refreshes_indirect_undo_binding_cache() {
        crate::test_utils::init_test_tracing();
        let mut mgr = BufferManager::new();
        let base_id = mgr.current_buffer_id().expect("scratch buffer");
        let indirect_id = mgr
            .create_indirect_buffer(base_id, "*switch-current-indirect*", false)
            .expect("indirect buffer");
        let _ = mgr.insert_into_buffer(base_id, "abc");

        let stale = mgr
            .get(indirect_id)
            .expect("indirect buffer")
            .get_undo_list();
        mgr.get_mut(indirect_id)
            .expect("indirect buffer")
            .locals
            .set_raw_binding("buffer-undo-list", RuntimeBindingValue::Bound(Value::NIL));
        assert_eq!(
            mgr.get(indirect_id)
                .expect("indirect buffer")
                .get_buffer_local("buffer-undo-list"),
            Some(&Value::NIL)
        );

        assert!(mgr.switch_current(indirect_id));
        assert_eq!(
            mgr.get(indirect_id)
                .expect("indirect buffer")
                .get_buffer_local("buffer-undo-list"),
            Some(&stale)
        );
    }

    #[test]
    fn manager_kill_buffer() {
        crate::test_utils::init_test_tracing();
        let mut mgr = BufferManager::new();
        let id = mgr.create_buffer("doomed");
        assert!(mgr.kill_buffer(id));
        assert!(mgr.get(id).is_none());
        assert!(!mgr.kill_buffer(id)); // already dead
    }

    #[test]
    fn manager_kill_current_clears_current() {
        crate::test_utils::init_test_tracing();
        let mut mgr = BufferManager::new();
        let scratch = mgr.find_buffer_by_name("*scratch*").unwrap();
        mgr.set_current(scratch);
        mgr.kill_buffer(scratch);
        assert!(mgr.current_buffer().is_none());
    }

    #[test]
    fn manager_buffer_list() {
        crate::test_utils::init_test_tracing();
        let mut mgr = BufferManager::new();
        let scratch = mgr.find_buffer_by_name("*scratch*").expect("scratch");
        let a = mgr.create_buffer("a");
        let b = mgr.create_buffer("b");
        assert_eq!(mgr.buffer_list(), vec![scratch, a, b]);
    }

    #[test]
    fn manager_generate_new_buffer_name_unique() {
        crate::test_utils::init_test_tracing();
        let mgr = BufferManager::new();
        // "*scratch*" is taken, "foo" is not.
        assert_eq!(mgr.generate_new_buffer_name("foo"), "foo");
        assert_eq!(mgr.generate_new_buffer_name("*scratch*"), "*scratch*<2>");
    }

    #[test]
    fn manager_generate_new_buffer_name_increments() {
        crate::test_utils::init_test_tracing();
        let mut mgr = BufferManager::new();
        mgr.create_buffer("buf");
        assert_eq!(mgr.generate_new_buffer_name("buf"), "buf<2>");
        mgr.create_buffer("buf<2>");
        assert_eq!(mgr.generate_new_buffer_name("buf"), "buf<3>");
    }

    #[test]
    fn manager_generate_new_buffer_name_honors_ignore_candidate() {
        crate::test_utils::init_test_tracing();
        let mut mgr = BufferManager::new();
        mgr.create_buffer("buf");
        mgr.create_buffer("buf<2>");
        assert_eq!(
            mgr.generate_new_buffer_name_ignoring("buf", Some("buf<2>")),
            "buf<2>"
        );
        assert_eq!(
            mgr.generate_new_buffer_name_ignoring("buf", Some("buf<3>")),
            "buf<3>"
        );
    }

    // -----------------------------------------------------------------------
    // BufferManager — markers
    // -----------------------------------------------------------------------

    #[test]
    fn manager_create_and_query_marker() {
        crate::test_utils::init_test_tracing();
        let mut mgr = BufferManager::new();
        let id = mgr.create_buffer("m");
        // Insert some text so there is room for a marker.
        mgr.get_mut(id).unwrap().text = BufferText::from_str("abcdef");
        mgr.get_mut(id).unwrap().widen();

        let mid = mgr.create_marker(id, 3, InsertionType::After);
        assert_eq!(mgr.marker_position(id, mid), Some(3));
        assert_eq!(mgr.marker_char_position(id, mid), Some(3));
    }

    #[test]
    fn manager_marker_clamped_to_buffer_len() {
        crate::test_utils::init_test_tracing();
        let mut mgr = BufferManager::new();
        let id = mgr.create_buffer("m");
        // Buffer is empty (len = 0), marker at 100 should be clamped.
        let mid = mgr.create_marker(id, 100, InsertionType::Before);
        assert_eq!(mgr.marker_position(id, mid), Some(0));
        assert_eq!(mgr.marker_char_position(id, mid), Some(0));
    }

    #[test]
    fn manager_marker_nonexistent_buffer() {
        crate::test_utils::init_test_tracing();
        let mgr = BufferManager::new();
        let pos = mgr.marker_position(BufferId(9999), 1);
        assert_eq!(pos, None);
    }

    #[test]
    fn manager_labeled_widen_uses_innermost_and_without_restriction_reaches_full_buffer() {
        crate::test_utils::init_test_tracing();
        let mut mgr = BufferManager::new();
        let id = mgr.create_buffer("labeled");
        mgr.set_current(id);
        mgr.get_mut(id).unwrap().insert("abcdef");

        let _ = mgr.internal_labeled_narrow_to_region(id, 1, 4, Value::symbol("tag"));
        let buf = mgr.get(id).unwrap();
        assert_eq!(buf.point_min(), 1);
        assert_eq!(buf.point_max(), 4);

        let _ = mgr.widen_buffer(id);
        let buf = mgr.get(id).unwrap();
        assert_eq!(buf.point_min(), 1);
        assert_eq!(buf.point_max(), 4);

        let _ = mgr.internal_labeled_widen(id, &Value::symbol("tag"));
        let buf = mgr.get(id).unwrap();
        assert_eq!(buf.point_min(), 0);
        assert_eq!(buf.point_max(), 6);
    }

    #[test]
    fn manager_save_restriction_state_restores_labeled_stack() {
        crate::test_utils::init_test_tracing();
        let mut mgr = BufferManager::new();
        let id = mgr.create_buffer("saved-labeled");
        mgr.set_current(id);
        mgr.get_mut(id).unwrap().insert("abcdefgh");
        let _ = mgr.internal_labeled_narrow_to_region(id, 1, 5, Value::symbol("tag"));

        let saved = mgr
            .save_current_restriction_state()
            .expect("restriction state should save");
        let _ = mgr.internal_labeled_widen(id, &Value::symbol("tag"));
        let _ = mgr.narrow_buffer_to_region(id, 2, 3);
        mgr.restore_saved_restriction_state(saved);

        let buf = mgr.get(id).unwrap();
        assert_eq!(buf.point_min(), 1);
        assert_eq!(buf.point_max(), 5);

        let _ = mgr.widen_buffer(id);
        let buf = mgr.get(id).unwrap();
        assert_eq!(buf.point_min(), 1);
        assert_eq!(buf.point_max(), 5);
    }

    #[test]
    fn manager_reset_outermost_restrictions_restores_current_innermost_after_mutation() {
        crate::test_utils::init_test_tracing();
        let mut mgr = BufferManager::new();
        let id = mgr.create_buffer("redisplay-labeled");
        mgr.set_current(id);
        mgr.get_mut(id).unwrap().insert("abcdef");

        let _ = mgr.internal_labeled_narrow_to_region(id, 1, 5, Value::symbol("outer"));
        let _ = mgr.internal_labeled_narrow_to_region(id, 2, 4, Value::symbol("inner"));

        let buf = mgr.get(id).unwrap();
        assert_eq!(buf.point_min(), 2);
        assert_eq!(buf.point_max(), 4);

        let saved = mgr.reset_outermost_restrictions();
        let buf = mgr.get(id).unwrap();
        assert_eq!(buf.point_min(), 0);
        assert_eq!(buf.point_max(), 6);

        let _ = mgr.internal_labeled_widen(id, &Value::symbol("inner"));
        let buf = mgr.get(id).unwrap();
        assert_eq!(buf.point_min(), 1);
        assert_eq!(buf.point_max(), 5);

        mgr.restore_outermost_restrictions(saved);
        let buf = mgr.get(id).unwrap();
        assert_eq!(buf.point_min(), 1);
        assert_eq!(buf.point_max(), 5);
    }

    // -----------------------------------------------------------------------
    // BufferManager — current_buffer_mut
    // -----------------------------------------------------------------------

    #[test]
    fn manager_current_buffer_mut_insert() {
        crate::test_utils::init_test_tracing();
        let mut mgr = BufferManager::new();
        let current = mgr.current_buffer_id().unwrap();
        mgr.insert_into_buffer(current, "hello");
        assert_eq!(mgr.current_buffer().unwrap().buffer_string(), "hello");
    }

    #[test]
    fn manager_replace_buffer_contents_resets_narrowing_and_point() {
        crate::test_utils::init_test_tracing();
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
        crate::test_utils::init_test_tracing();
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
