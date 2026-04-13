//! Live window-output emission helpers for Rust redisplay.
//!
//! This layer bridges Rust layout/status-line emission to GNU-like live window
//! output state. It updates `WindowDisplayState` through explicit row begin /
//! progress / finish steps while simultaneously recording immutable row
//! snapshots for renderer handoff.

use super::display_status_line::StatusLineOutputProgress;
use neovm_core::emacs_core::Context;
use neovm_core::window::{
    DisplayPointSnapshot, DisplayRowSnapshot, WindowCursorPos, WindowCursorSnapshot,
    WindowDisplaySnapshot,
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct RowMetricsSnapshot {
    pub(crate) row: usize,
    pub(crate) pixel_y: f32,
    pub(crate) height: f32,
    pub(crate) ascent: f32,
}

pub(crate) struct WindowOutputEmitter {
    frame_id: neovm_core::window::FrameId,
    window_id: neovm_core::window::WindowId,
    text_x: f32,
    window_top: f32,
    points: Vec<DisplayPointSnapshot>,
    rows: Vec<DisplayRowSnapshot>,
    row_metrics: Vec<RowMetricsSnapshot>,
    current_row_first_display_pos: Option<usize>,
    current_row_last_display_pos: Option<usize>,
}

impl WindowOutputEmitter {
    pub(crate) fn new(
        frame_id: neovm_core::window::FrameId,
        window_id: neovm_core::window::WindowId,
        text_x: f32,
        window_top: f32,
    ) -> Self {
        Self {
            frame_id,
            window_id,
            text_x,
            window_top,
            points: Vec::new(),
            rows: Vec::new(),
            row_metrics: Vec::new(),
            current_row_first_display_pos: None,
            current_row_last_display_pos: None,
        }
    }

    pub(crate) fn display_point_len(&self) -> usize {
        self.points.len()
    }

    pub(crate) fn truncate_display_points(&mut self, len: usize) {
        self.points.truncate(len);
    }

    pub(crate) fn rows(&self) -> &[DisplayRowSnapshot] {
        &self.rows
    }

    pub(crate) fn row_metrics(&self) -> &[RowMetricsSnapshot] {
        &self.row_metrics
    }

    pub(crate) fn current_row_display_positions(&self) -> (Option<usize>, Option<usize>) {
        (
            self.current_row_first_display_pos,
            self.current_row_last_display_pos,
        )
    }

    pub(crate) fn restore_current_row_display_positions(
        &mut self,
        first: Option<usize>,
        last: Option<usize>,
    ) {
        self.current_row_first_display_pos = first;
        self.current_row_last_display_pos = last;
    }

    pub(crate) fn note_display_buffer_pos(&mut self, buffer_pos: usize) {
        if self.current_row_first_display_pos.is_none() {
            self.current_row_first_display_pos = Some(buffer_pos);
        }
        self.current_row_last_display_pos = Some(buffer_pos);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn push_display_point(
        &mut self,
        buffer_pos: i64,
        glyph_x: f32,
        glyph_y: f32,
        width: f32,
        height: f32,
        row: i64,
        col: usize,
    ) {
        if buffer_pos < 1 {
            return;
        }
        let buffer_pos = buffer_pos as usize;
        self.note_display_buffer_pos(buffer_pos);
        self.points.push(DisplayPointSnapshot {
            buffer_pos,
            x: (glyph_x - self.text_x).round() as i64,
            y: (glyph_y - self.window_top).round() as i64,
            width: width.max(0.0).round() as i64,
            height: height.max(1.0).round() as i64,
            row,
            col: col as i64,
        });
    }

    pub(crate) fn begin_row(&self, evaluator: &mut Context, row: i64, col: i64, y: i64, x: i64) {
        if let Some(frame) = evaluator.frame_manager_mut().get_mut(self.frame_id) {
            frame.begin_window_output_row(self.window_id, row, col, y, x);
        }
    }

    pub(crate) fn begin_update(&self, evaluator: &mut Context) {
        if let Some(frame) = evaluator.frame_manager_mut().get_mut(self.frame_id) {
            frame.begin_window_output_update(self.window_id);
        }
    }

    pub(crate) fn advance_progress(
        &self,
        evaluator: &mut Context,
        row: i64,
        col: i64,
        y: i64,
        x: i64,
    ) {
        if let Some(frame) = evaluator.frame_manager_mut().get_mut(self.frame_id) {
            frame.advance_window_output_progress(self.window_id, row, col, y, x);
        }
    }

    pub(crate) fn push_text_row(
        &mut self,
        evaluator: &mut Context,
        row: i64,
        row_y_start: f32,
        row_height: f32,
        row_ascent: f32,
        row_end_x: f32,
        row_end_col: usize,
    ) {
        self.rows.push(DisplayRowSnapshot {
            row,
            y: (row_y_start - self.window_top).round() as i64,
            height: row_height.max(1.0).round() as i64,
            end_x: (row_end_x - self.text_x).round() as i64,
            end_col: row_end_col as i64,
            start_buffer_pos: self.current_row_first_display_pos.take(),
            end_buffer_pos: self.current_row_last_display_pos.take(),
        });
        self.row_metrics.push(RowMetricsSnapshot {
            row: row.max(0) as usize,
            pixel_y: row_y_start,
            height: row_height.max(1.0),
            ascent: row_ascent.max(0.0).min(row_height.max(1.0)),
        });
        if let Some(row) = self.rows.last()
            && let Some(frame) = evaluator.frame_manager_mut().get_mut(self.frame_id)
        {
            frame.finish_window_output_row(self.window_id, row);
        }
    }

    pub(crate) fn push_chrome_row(&mut self, evaluator: &mut Context, row: DisplayRowSnapshot) {
        if let Some(frame) = evaluator.frame_manager_mut().get_mut(self.frame_id) {
            frame.finish_window_output_row(self.window_id, &row);
        }
        self.rows.push(row);
    }

    pub(crate) fn push_chrome_row_progress(
        &mut self,
        evaluator: &mut Context,
        row: i64,
        progress: StatusLineOutputProgress,
    ) {
        self.push_chrome_row(
            evaluator,
            DisplayRowSnapshot {
                row,
                y: (progress.y - self.window_top).round() as i64,
                height: progress.height.round() as i64,
                end_x: (progress.end_x - self.text_x).round() as i64,
                end_col: progress.end_col,
                start_buffer_pos: None,
                end_buffer_pos: None,
            },
        );
    }

    pub(crate) fn finish_snapshot(
        mut self,
        evaluator: &mut Context,
        logical_cursor: Option<WindowCursorPos>,
        phys_cursor: Option<WindowCursorSnapshot>,
        text_area_left_offset: i64,
        mode_line_height: i64,
        header_line_height: i64,
        tab_line_height: i64,
    ) -> WindowDisplaySnapshot {
        self.rows.sort_by_key(|row| row.row);
        let snapshot = WindowDisplaySnapshot {
            window_id: self.window_id,
            text_area_left_offset,
            mode_line_height,
            header_line_height,
            tab_line_height,
            logical_cursor,
            phys_cursor: phys_cursor.clone(),
            points: self.points,
            rows: self.rows,
        };
        if let Some(frame) = evaluator.frame_manager_mut().get_mut(self.frame_id) {
            frame.finalize_window_output_update(
                self.window_id,
                logical_cursor,
                phys_cursor,
                Some(&snapshot),
            );
        }
        snapshot
    }
}
