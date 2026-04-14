//! Structural buffer edit pipeline.
//!
//! This module is the first source-ownership extraction toward a GNU
//! `insdel.c`-style boundary. It rehomes the existing `Buffer` edit core
//! without changing behavior.

use super::{Buffer, BufferId, BufferManager};
use crate::buffer::undo;
use crate::heap_types::LispString;

#[inline]
fn emacs_char_count(bytes: &[u8], multibyte: bool) -> usize {
    if multibyte {
        crate::emacs_core::emacs_char::chars_in_multibyte(bytes)
    } else {
        bytes.len()
    }
}

#[inline]
fn lisp_string_from_buffer_bytes(bytes: Vec<u8>, multibyte: bool) -> LispString {
    if multibyte {
        LispString::from_emacs_bytes(bytes)
    } else {
        LispString::from_unibyte(bytes)
    }
}

impl Buffer {
    fn insert_bytes_internal(&mut self, bytes: &[u8], char_len: usize, before_markers: bool) {
        let insert_pos = self.pt_byte;
        let insert_char_pos = self.pt;
        if bytes.is_empty() {
            return;
        }
        let byte_len = bytes.len();

        if !self.undo_state.in_progress() {
            self.undo_prepare_change(insert_pos, self.pt_byte);
            let mut ul = self.get_undo_list();
            if !undo::undo_list_is_disabled(&ul) {
                undo::undo_list_record_insert(&mut ul, insert_pos, byte_len, self.pt_byte);
                self.set_undo_list(ul);
            }
        }

        self.text.insert_emacs_bytes(insert_pos, bytes);
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
            if self.pt_byte > insert_pos || (advance_point_at_insert && self.pt_byte == insert_pos)
            {
                self.pt_byte += byte_len;
                self.pt += char_len;
            }
            if shift_begv && self.begv_byte > insert_pos {
                self.begv_byte += byte_len;
                self.begv += char_len;
            }
            if self.zv_byte >= insert_pos {
                self.zv_byte += byte_len;
                self.zv += char_len;
            }
        }
        if let Some(mark_byte) = self.mark_byte
            && mark_byte > insert_pos
        {
            self.mark_byte = Some(mark_byte + byte_len);
            self.mark = self.mark.map(|mark_char| mark_char + char_len);
        }
        if adjust_shared_markers {
            self.text
                .adjust_markers_for_insert(insert_pos, byte_len, char_len);
        }
        debug_assert_eq!(
            self.text.emacs_byte_to_char(insert_pos),
            insert_char_pos,
            "insert-side-effect char position drifted from the source edit site"
        );
        if adjust_shared_text_props {
            self.text.adjust_text_props_for_insert(insert_pos, byte_len);
        }
        self.overlays
            .adjust_for_insert(insert_pos, byte_len, overlay_before_markers);
        self.record_char_modification(char_len);
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
            if self.pt_byte >= end {
                self.pt_byte -= byte_len;
                self.pt -= char_len;
            } else if self.pt_byte > start {
                self.pt_byte = start;
                self.pt = start_char;
            }

            if shift_begv {
                if self.begv_byte >= end {
                    self.begv_byte -= byte_len;
                    self.begv -= char_len;
                } else if self.begv_byte > start {
                    self.begv_byte = start;
                    self.begv = start_char;
                }
            }

            if self.zv_byte >= end {
                self.zv_byte -= byte_len;
                self.zv -= char_len;
            } else if self.zv_byte > start {
                self.zv_byte = start;
                self.zv = start_char;
            }
        }

        if let Some(mark_byte) = self.mark_byte {
            if mark_byte >= end {
                self.mark_byte = Some(mark_byte - byte_len);
                self.mark = self.mark.map(|mark_char| mark_char - char_len);
            } else if mark_byte > start {
                self.mark_byte = Some(start);
                self.mark = Some(start_char);
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
    }

    fn apply_same_len_edit_side_effects(
        &mut self,
        changed_chars: usize,
        preserve_modified_state: bool,
    ) {
        let old_state = self.modified_state_value();
        self.record_char_modification(changed_chars);
        if preserve_modified_state && old_state.is_nil() {
            self.text.set_save_modified_tick(self.text.modified_tick());
        }
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
        self.text
            .record_char_modification(Self::modification_tick_delta(changed_chars));
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
        if text.is_empty() {
            return;
        }
        let bytes = crate::emacs_core::string_escape::storage_string_to_buffer_bytes(
            text,
            self.get_multibyte(),
        );
        let char_len = emacs_char_count(&bytes, self.get_multibyte());
        self.insert_bytes_internal(&bytes, char_len, before_markers);
    }

    pub fn insert(&mut self, text: &str) {
        self.insert_internal(text, false);
    }

    pub fn insert_before_markers(&mut self, text: &str) {
        self.insert_internal(text, true);
    }

    pub fn insert_lisp_string(&mut self, text: &LispString) {
        debug_assert_eq!(
            text.is_multibyte(),
            self.get_multibyte(),
            "insert_lisp_string: string multibyte flag must match target buffer",
        );
        self.insert_bytes_internal(text.as_bytes(), text.schars(), false);
    }

    pub fn insert_lisp_string_before_markers(&mut self, text: &LispString) {
        debug_assert_eq!(
            text.is_multibyte(),
            self.get_multibyte(),
            "insert_lisp_string_before_markers: string multibyte flag must match target buffer",
        );
        self.insert_bytes_internal(text.as_bytes(), text.schars(), true);
    }

    /// Delete the byte range `[start, end)`.
    ///
    /// Adjusts point, mark, markers, and the narrowing boundary.
    pub fn delete_region(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let start_char = self.text.emacs_byte_to_char(start);
        let end_char = self.text.emacs_byte_to_char(end);
        // Record undo: save the deleted text for restoration.
        let mut deleted_bytes = Vec::new();
        self.text
            .copy_emacs_bytes_to(start, end, &mut deleted_bytes);
        let deleted_text = lisp_string_from_buffer_bytes(deleted_bytes, self.get_multibyte());
        if !self.undo_state.in_progress() {
            self.undo_prepare_change(start, self.pt_byte);
            let mut ul = self.get_undo_list();
            if !undo::undo_list_is_disabled(&ul) {
                undo::undo_list_record_delete(&mut ul, start, deleted_text, self.pt_byte);
                self.set_undo_list(ul);
            }
        }

        self.text.delete_range(start, end);
        self.apply_byte_delete_side_effects(
            start, end, start_char, end_char, true, false, true, true,
        );
    }

    /// Replace every occurrence of `from_code` with `to_storage` in the byte range
    /// `[start, end)`.
    ///
    /// The replacement is performed in place, so callers must ensure the
    /// matched storage units have the same storage-byte length.
    pub fn subst_char_in_region(
        &mut self,
        start: usize,
        end: usize,
        from_code: u32,
        to_storage: &str,
        noundo: bool,
    ) -> bool {
        if start >= end {
            return false;
        }
        let changed_chars = self.text.emacs_byte_to_char(end) - self.text.emacs_byte_to_char(start);
        let original = self.text.text_range(start, end);
        let Some(replacement) =
            crate::emacs_core::string_escape::replace_storage_char_code_same_len(
                &original, from_code, to_storage,
            )
        else {
            return false;
        };
        let replacement_bytes = crate::emacs_core::string_escape::storage_string_to_buffer_bytes(
            &replacement,
            self.get_multibyte(),
        );

        if !noundo && !self.undo_state.in_progress() {
            self.undo_prepare_change(start, self.pt_byte);
            let mut ul = self.get_undo_list();
            if !undo::undo_list_is_disabled(&ul) {
                let deleted_bytes =
                    crate::emacs_core::string_escape::storage_string_to_buffer_bytes(
                        &original,
                        self.get_multibyte(),
                    );
                let deleted = lisp_string_from_buffer_bytes(deleted_bytes, self.get_multibyte());
                undo::undo_list_record_delete(&mut ul, start, deleted, self.pt_byte);
                undo::undo_list_record_insert(
                    &mut ul,
                    start,
                    replacement_bytes.len(),
                    self.pt_byte,
                );
                self.set_undo_list(ul);
            }
        }

        self.text
            .replace_same_len_emacs_bytes(start, end, &replacement_bytes);
        self.apply_same_len_edit_side_effects(changed_chars, noundo);
        true
    }
}

/// Structural text mutation entry points for buffers and indirect-buffer
/// siblings. This is the closest Rust ownership boundary to GNU `insdel.c`.
impl BufferManager {
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

    pub fn insert_into_buffer(&mut self, id: BufferId, text: &str) -> Option<()> {
        if text.is_empty() {
            return Some(());
        }
        let byte_len = crate::emacs_core::string_escape::storage_byte_len(text);
        let char_len = crate::emacs_core::string_escape::storage_char_len(text);

        let root_id = self.shared_text_root_id(id)?;
        let shared_ids = self.buffers_sharing_root_ids(root_id);
        let source = self.buffers.get(&id)?;
        let insert_pos = source.pt_byte;
        let insert_char_pos = source.pt;

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
        Some(())
    }

    pub fn insert_lisp_string_into_buffer(
        &mut self,
        id: BufferId,
        text: &LispString,
    ) -> Option<()> {
        if text.is_empty() {
            return Some(());
        }
        let byte_len = text.sbytes();
        let char_len = text.schars();

        let root_id = self.shared_text_root_id(id)?;
        let shared_ids = self.buffers_sharing_root_ids(root_id);
        let source = self.buffers.get(&id)?;
        let insert_pos = source.pt_byte;
        let insert_char_pos = source.pt;

        self.buffers.get_mut(&id)?.insert_lisp_string(text);

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
        Some(())
    }

    pub fn insert_into_buffer_before_markers(&mut self, id: BufferId, text: &str) -> Option<()> {
        if text.is_empty() {
            return Some(());
        }
        let byte_len = crate::emacs_core::string_escape::storage_byte_len(text);
        let char_len = crate::emacs_core::string_escape::storage_char_len(text);
        let root_id = self.shared_text_root_id(id)?;
        let shared_ids = self.buffers_sharing_root_ids(root_id);
        let source = self.buffers.get(&id)?;
        let insert_pos = source.pt_byte;
        let insert_char_pos = source.pt;

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
        Some(())
    }

    pub fn insert_lisp_string_into_buffer_before_markers(
        &mut self,
        id: BufferId,
        text: &LispString,
    ) -> Option<()> {
        if text.is_empty() {
            return Some(());
        }
        let byte_len = text.sbytes();
        let char_len = text.schars();
        let root_id = self.shared_text_root_id(id)?;
        let shared_ids = self.buffers_sharing_root_ids(root_id);
        let source = self.buffers.get(&id)?;
        let insert_pos = source.pt_byte;
        let insert_char_pos = source.pt;

        self.buffers
            .get_mut(&id)?
            .insert_lisp_string_before_markers(text);

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
        Some(())
    }

    pub fn delete_buffer_region(&mut self, id: BufferId, start: usize, end: usize) -> Option<()> {
        if start >= end {
            return Some(());
        }

        let root_id = self.shared_text_root_id(id)?;
        let shared_ids = self.buffers_sharing_root_ids(root_id);
        let source = self.buffers.get(&id)?;
        let start_char = source.text.emacs_byte_to_char(start);
        let end_char = source.text.emacs_byte_to_char(end);
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
        Some(())
    }

    pub fn subst_char_in_buffer_region(
        &mut self,
        id: BufferId,
        start: usize,
        end: usize,
        from_code: u32,
        to_storage: &str,
        noundo: bool,
    ) -> Option<bool> {
        if start >= end {
            return Some(false);
        }

        let root_id = self.shared_text_root_id(id)?;
        let shared_ids = self.buffers_sharing_root_ids(root_id);
        let changed_chars = {
            let source = self.buffers.get(&id)?;
            source.text.emacs_byte_to_char(end) - source.text.emacs_byte_to_char(start)
        };
        let changed = self
            .buffers
            .get_mut(&id)?
            .subst_char_in_region(start, end, from_code, to_storage, noundo);
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
        Some(true)
    }
}
