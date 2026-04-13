use super::RenderApp;
use crate::core::frame_glyphs::{DisplaySlotId, FrameGlyph, PhysCursor, WindowCursorVisual};
use std::collections::HashMap;

impl RenderApp {
    pub(super) fn prepare_frame_state_for_render(&mut self) {
        self.update_fps_state();

        #[cfg(feature = "neo-term")]
        self.update_terminals();

        self.process_webkit_frames();
        self.process_video_frames();
        self.process_pending_images();
        self.refresh_faces_from_frames();
        self.apply_extra_spacing_if_needed();
    }

    fn update_fps_state(&mut self) {
        if self.fps.enabled {
            self.fps.render_start = std::time::Instant::now();
            self.fps.frame_count += 1;
            let elapsed = self.fps.last_instant.elapsed();
            if elapsed.as_secs_f32() >= 1.0 {
                self.fps.display_value = self.fps.frame_count as f32 / elapsed.as_secs_f32();
                self.fps.frame_count = 0;
                self.fps.last_instant = std::time::Instant::now();
            }
        }
    }

    fn refresh_faces_from_frames(&mut self) {
        let old_face_ids: std::collections::HashSet<u32> = self.faces.keys().copied().collect();
        if let Some(ref frame) = self.current_frame {
            self.faces = frame.faces.clone();
        }
        for entry in self.child_frames.frames.values() {
            for (face_id, face) in &entry.frame.faces {
                self.faces.entry(*face_id).or_insert_with(|| face.clone());
            }
        }
        let has_new_faces = self.faces.keys().any(|id| !old_face_ids.contains(id));
        if has_new_faces {
            if let Some(ref mut atlas) = self.glyph_atlas {
                tracing::info!(
                    "New face_ids detected (old={}, new={}), clearing glyph cache",
                    old_face_ids.len(),
                    self.faces.len()
                );
                atlas.clear();
            }
        }
    }

    fn apply_extra_spacing_if_needed(&mut self) {
        if self.extra_line_spacing != 0.0 || self.extra_letter_spacing != 0.0 {
            if let Some(ref mut frame) = self.current_frame {
                Self::apply_extra_spacing(
                    &mut frame.glyphs,
                    &mut frame.window_cursors,
                    &mut frame.phys_cursor,
                    self.extra_line_spacing,
                    self.extra_letter_spacing,
                );
            }
        }
    }

    fn apply_extra_spacing(
        glyphs: &mut [FrameGlyph],
        window_cursors: &mut [WindowCursorVisual],
        phys_cursor: &mut Option<PhysCursor>,
        line_spacing: f32,
        letter_spacing: f32,
    ) {
        let mut last_y: f32 = f32::NEG_INFINITY;
        let mut row_index: i32 = -1;
        let mut char_in_row: i32 = 0;
        let mut last_window_y: f32 = f32::NEG_INFINITY;
        let mut slot_positions: HashMap<DisplaySlotId, (f32, f32)> = HashMap::new();

        for glyph in glyphs.iter_mut() {
            match glyph {
                FrameGlyph::Char {
                    x,
                    y,
                    row_role,
                    slot_id,
                    ..
                } => {
                    if row_role.is_chrome() {
                        continue;
                    }
                    if *y < last_window_y - 1.0 {
                        row_index = -1;
                        last_y = f32::NEG_INFINITY;
                    }
                    last_window_y = *y;

                    if (*y - last_y).abs() > 0.5 {
                        row_index += 1;
                        char_in_row = 0;
                        last_y = *y;
                    } else {
                        char_in_row += 1;
                    }
                    *y += row_index as f32 * line_spacing;
                    *x += char_in_row as f32 * letter_spacing;
                    slot_positions.insert(*slot_id, (*x, *y));
                }
                FrameGlyph::Stretch {
                    x,
                    y,
                    row_role,
                    slot_id,
                    ..
                } => {
                    if row_role.is_chrome() {
                        continue;
                    }
                    if *y < last_window_y - 1.0 {
                        row_index = -1;
                        last_y = f32::NEG_INFINITY;
                    }
                    last_window_y = *y;

                    if (*y - last_y).abs() > 0.5 {
                        row_index += 1;
                        char_in_row = 0;
                        last_y = *y;
                    } else {
                        char_in_row += 1;
                    }
                    *y += row_index as f32 * line_spacing;
                    *x += char_in_row as f32 * letter_spacing;
                    slot_positions.insert(*slot_id, (*x, *y));
                }
                FrameGlyph::Image {
                    x,
                    y,
                    row_role,
                    slot_id,
                    ..
                }
                | FrameGlyph::Video {
                    x,
                    y,
                    row_role,
                    slot_id,
                    ..
                }
                | FrameGlyph::WebKit {
                    x,
                    y,
                    row_role,
                    slot_id,
                    ..
                } => {
                    if row_role.is_chrome() {
                        continue;
                    }
                    let Some(slot_id) = *slot_id else {
                        continue;
                    };
                    if *y < last_window_y - 1.0 {
                        row_index = -1;
                        last_y = f32::NEG_INFINITY;
                    }
                    last_window_y = *y;

                    if (*y - last_y).abs() > 0.5 {
                        row_index += 1;
                        char_in_row = 0;
                        last_y = *y;
                    } else {
                        char_in_row += 1;
                    }
                    *y += row_index as f32 * line_spacing;
                    *x += char_in_row as f32 * letter_spacing;
                    slot_positions.insert(slot_id, (*x, *y));
                }
                _ => {}
            }
        }

        for cursor in window_cursors.iter_mut() {
            if let Some((x, y)) = slot_positions.get(&cursor.slot_id).copied() {
                cursor.x = x;
                cursor.y = y;
            }
        }

        if let Some(cursor) = phys_cursor.as_mut()
            && let Some((x, y)) = slot_positions.get(&cursor.slot_id).copied()
        {
            cursor.x = x;
            cursor.y = y;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::frame_glyphs::{CursorStyle, FrameGlyphBuffer, GlyphRowRole};
    use neomacs_display_protocol::types::Color;

    #[test]
    fn apply_extra_spacing_remaps_cursor_by_slot_id() {
        let mut frame = FrameGlyphBuffer::with_size(80.0, 32.0);
        frame.set_draw_context(1, GlyphRowRole::Text, None);
        frame.add_char('a', 0.0, 0.0, 8.0, 16.0, 12.0, false);
        frame.add_char('b', 8.0, 0.0, 8.0, 16.0, 12.0, false);
        let target_slot = frame.glyphs[1].slot_id().expect("slot id");

        frame.add_cursor(1, 2.0, 0.0, 2.0, 16.0, CursorStyle::Bar(2.0), Color::WHITE);
        frame.window_cursors[0].slot_id = target_slot;

        frame.set_phys_cursor(PhysCursor {
            window_id: 1,
            charpos: 1,
            row: 0,
            col: 1,
            slot_id: target_slot,
            x: 2.0,
            y: 0.0,
            width: 2.0,
            height: 16.0,
            ascent: 12.0,
            style: CursorStyle::Bar(2.0),
            color: Color::WHITE,
            cursor_fg: Color::BLACK,
        });

        RenderApp::apply_extra_spacing(
            &mut frame.glyphs,
            &mut frame.window_cursors,
            &mut frame.phys_cursor,
            0.0,
            1.0,
        );

        match &frame.glyphs[1] {
            FrameGlyph::Char { x, .. } => assert_eq!(*x, 9.0),
            other => panic!("expected char glyph, got {:?}", other),
        }
        assert_eq!(frame.window_cursors[0].x, 9.0);
        assert_eq!(frame.window_cursors[0].y, 0.0);
        let cursor = frame.phys_cursor.as_ref().expect("phys cursor");
        assert_eq!(cursor.x, 9.0);
        assert_eq!(cursor.y, 0.0);
    }
}
