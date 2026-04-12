use super::RenderApp;
use crate::core::frame_glyphs::{FrameGlyph, PhysCursor};

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
                    &mut frame.phys_cursor,
                    self.extra_line_spacing,
                    self.extra_letter_spacing,
                );
            }
        }
    }

    fn apply_extra_spacing(
        glyphs: &mut [FrameGlyph],
        phys_cursor: &mut Option<PhysCursor>,
        line_spacing: f32,
        letter_spacing: f32,
    ) {
        let mut last_y: f32 = f32::NEG_INFINITY;
        let mut row_index: i32 = -1;
        let mut char_in_row: i32 = 0;
        let mut last_window_y: f32 = f32::NEG_INFINITY;

        for glyph in glyphs.iter_mut() {
            match glyph {
                FrameGlyph::Char { x, y, row_role, .. } => {
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
                }
                FrameGlyph::Stretch { x, y, row_role, .. } => {
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
                }
                FrameGlyph::Cursor { y, x, .. } => {
                    if (*y - last_y).abs() < 0.5 {
                        let old_x = *x;
                        let old_y = *y;
                        let dy = row_index.max(0) as f32 * line_spacing;
                        let dx = char_in_row as f32 * letter_spacing;
                        *y += dy;
                        *x += dx;

                        if let Some(cursor) = phys_cursor.as_mut()
                            && (cursor.x - old_x).abs() < 0.5
                            && (cursor.y - old_y).abs() < 0.5
                        {
                            cursor.x += dx;
                            cursor.y += dy;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
