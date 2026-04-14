//! Structural buffer edit pipeline.
//!
//! This module is the first source-ownership extraction toward a GNU
//! `insdel.c`-style boundary. It rehomes the existing `Buffer` edit core
//! without changing behavior.

use super::{buffer::Buffer, undo};

impl Buffer {
    pub(crate) fn apply_byte_insert_side_effects(
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

    pub(crate) fn apply_byte_delete_side_effects(
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

    pub(crate) fn apply_same_len_edit_side_effects(
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
        let _ = (beg, pt);
        self.undo_ensure_first_change();
    }

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
}
