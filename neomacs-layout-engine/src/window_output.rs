//! Live window-output emission helpers for Rust redisplay.
//!
//! This layer bridges Rust layout/status-line emission to GNU-like live window
//! output state. It updates `WindowDisplayState` through explicit row begin /
//! progress / finish steps while simultaneously recording immutable row
//! snapshots for renderer handoff.

use neovm_core::emacs_core::Context;
use neovm_core::window::{
    DisplayRowSnapshot, WindowCursorPos, WindowCursorSnapshot, WindowDisplaySnapshot,
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
        }
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
        &self,
        evaluator: &mut Context,
        rows: &mut Vec<DisplayRowSnapshot>,
        row_metrics: &mut Vec<RowMetricsSnapshot>,
        row: i64,
        row_y_start: f32,
        row_height: f32,
        row_ascent: f32,
        row_end_x: f32,
        row_end_col: usize,
        row_first_display_pos: &mut Option<usize>,
        row_last_display_pos: &mut Option<usize>,
    ) {
        rows.push(DisplayRowSnapshot {
            row,
            y: (row_y_start - self.window_top).round() as i64,
            height: row_height.max(1.0).round() as i64,
            end_x: (row_end_x - self.text_x).round() as i64,
            end_col: row_end_col as i64,
            start_buffer_pos: row_first_display_pos.take(),
            end_buffer_pos: row_last_display_pos.take(),
        });
        row_metrics.push(RowMetricsSnapshot {
            row: row.max(0) as usize,
            pixel_y: row_y_start,
            height: row_height.max(1.0),
            ascent: row_ascent.max(0.0).min(row_height.max(1.0)),
        });
        if let Some(row) = rows.last()
            && let Some(frame) = evaluator.frame_manager_mut().get_mut(self.frame_id)
        {
            frame.finish_window_output_row(self.window_id, row);
        }
    }

    pub(crate) fn push_chrome_row(
        &self,
        evaluator: &mut Context,
        chrome_rows: &mut Vec<DisplayRowSnapshot>,
        row: DisplayRowSnapshot,
    ) {
        if let Some(frame) = evaluator.frame_manager_mut().get_mut(self.frame_id) {
            frame.finish_window_output_row(self.window_id, &row);
        }
        chrome_rows.push(row);
    }

    pub(crate) fn finalize_snapshot(
        &self,
        evaluator: &mut Context,
        logical_cursor: Option<WindowCursorPos>,
        phys_cursor: Option<WindowCursorSnapshot>,
        snapshot: &WindowDisplaySnapshot,
    ) {
        if let Some(frame) = evaluator.frame_manager_mut().get_mut(self.frame_id) {
            frame.install_logical_cursor(self.window_id, logical_cursor);
            frame.apply_physical_cursor_snapshot(self.window_id, phys_cursor);
            frame.fallback_output_cursor_from_snapshot(snapshot);
            frame.finish_window_output_update(self.window_id);
        }
    }
}
