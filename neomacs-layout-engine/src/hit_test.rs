//! Hit-test infrastructure: maps pixel coordinates to buffer char positions.
//!
//! Built during layout and queried from FFI for mouse interaction.

/// Per-row hit-test data: maps a Y range to a charpos range.
#[derive(Clone)]
pub(crate) struct HitRow {
    pub y_start: f32,
    pub y_end: f32,
    pub charpos_start: i64,
    pub charpos_end: i64,
}

/// Per-window hit-test data built during layout.
#[derive(Clone)]
pub(crate) struct WindowHitData {
    pub window_id: i64,
    pub content_x: f32,
    pub char_w: f32,
    pub rows: Vec<HitRow>,
}

/// Global hit-test data for all windows, updated each frame.
/// Safe to use without Mutex because layout and queries happen on the same (Emacs) thread.
pub(crate) static mut FRAME_HIT_DATA: Option<Vec<WindowHitData>> = None;

/// Core logic: compute charpos from pixel coordinates given window hit data.
/// Searches all windows for the one containing (px, py).
/// Returns charpos, or -1 if not found.
fn charpos_at_pixel_in(data: &[WindowHitData], px: f32, py: f32) -> i64 {
    for win in data {
        // Find row by Y
        for row in &win.rows {
            if py >= row.y_start && py < row.y_end {
                // Compute approximate column from X (guard zero char_w)
                let cw = if win.char_w > 0.0 { win.char_w } else { 8.0 };
                let col = ((px - win.content_x) / cw).max(0.0) as i64;
                let charpos = (row.charpos_start + col).min(row.charpos_end);
                return charpos;
            }
        }
    }
    -1
}

/// Core logic: compute charpos for a specific window at window-relative pixel coordinates.
fn window_charpos_in(data: &[WindowHitData], window_id: i64, wx: f32, wy: f32) -> i64 {
    for win in data {
        if win.window_id != window_id {
            continue;
        }
        for row in &win.rows {
            if wy >= row.y_start && wy < row.y_end {
                let cw = if win.char_w > 0.0 { win.char_w } else { 8.0 };
                let col = ((wx - win.content_x) / cw).max(0.0) as i64;
                return (row.charpos_start + col).min(row.charpos_end);
            }
        }
        // Past last row: return last charpos
        if let Some(last) = win.rows.last() {
            return last.charpos_end;
        }
        return -1;
    }
    -1
}

/// Query charpos at a given frame-relative pixel coordinate.
/// Searches all windows for the one containing (px, py).
/// Returns charpos, or -1 if not found.
pub fn hit_test_charpos_at_pixel(px: f32, py: f32) -> i64 {
    unsafe {
        match &*std::ptr::addr_of!(FRAME_HIT_DATA) {
            Some(data) => charpos_at_pixel_in(data, px, py),
            None => -1,
        }
    }
}

/// Query charpos for a specific window at window-relative pixel coordinates.
pub fn hit_test_window_charpos(window_id: i64, wx: f32, wy: f32) -> i64 {
    unsafe {
        match &*std::ptr::addr_of!(FRAME_HIT_DATA) {
            Some(data) => window_charpos_in(data, window_id, wx, wy),
            None => -1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_window(
        window_id: i64,
        content_x: f32,
        char_w: f32,
        rows: Vec<HitRow>,
    ) -> WindowHitData {
        WindowHitData {
            window_id,
            content_x,
            char_w,
            rows,
        }
    }

    fn make_row(y_start: f32, y_end: f32, charpos_start: i64, charpos_end: i64) -> HitRow {
        HitRow {
            y_start,
            y_end,
            charpos_start,
            charpos_end,
        }
    }

    // --- charpos_at_pixel_in tests ---

    #[test]
    fn charpos_at_pixel_returns_neg1_when_empty_windows() {
        assert_eq!(charpos_at_pixel_in(&[], 100.0, 100.0), -1);
    }

    #[test]
    fn charpos_at_pixel_returns_neg1_for_out_of_bounds_y() {
        let data = vec![make_window(1, 0.0, 10.0, vec![make_row(0.0, 20.0, 1, 80)])];
        // Y=30.0 is outside the only row (0.0..20.0)
        assert_eq!(charpos_at_pixel_in(&data, 5.0, 30.0), -1);
    }

    #[test]
    fn charpos_at_pixel_basic_hit() {
        let data = vec![make_window(1, 0.0, 10.0, vec![make_row(0.0, 20.0, 1, 80)])];
        // px=25.0, char_w=10.0, content_x=0.0 => col = floor(25/10) = 2
        // charpos = 1 + 2 = 3
        assert_eq!(charpos_at_pixel_in(&data, 25.0, 10.0), 3);
    }

    #[test]
    fn charpos_at_pixel_with_content_x_offset() {
        let data = vec![make_window(1, 50.0, 10.0, vec![make_row(0.0, 20.0, 1, 80)])];
        // px=75.0, content_x=50.0, char_w=10.0 => col = floor((75-50)/10) = 2
        // charpos = 1 + 2 = 3
        assert_eq!(charpos_at_pixel_in(&data, 75.0, 10.0), 3);
    }

    #[test]
    fn charpos_at_pixel_clamps_x_left_of_content() {
        let data = vec![make_window(
            1,
            50.0,
            10.0,
            vec![make_row(0.0, 20.0, 100, 180)],
        )];
        // px=10.0, content_x=50.0 => (10-50)/10 = -4.0, clamped to 0.0 => col=0
        // charpos = 100 + 0 = 100
        assert_eq!(charpos_at_pixel_in(&data, 10.0, 10.0), 100);
    }

    #[test]
    fn charpos_at_pixel_clamps_to_row_end() {
        let data = vec![make_window(1, 0.0, 10.0, vec![make_row(0.0, 20.0, 1, 5)])];
        // px=200.0, char_w=10.0 => col = 20, charpos = 1+20 = 21, but clamped to charpos_end=5
        assert_eq!(charpos_at_pixel_in(&data, 200.0, 10.0), 5);
    }

    #[test]
    fn charpos_at_pixel_multiple_rows() {
        let data = vec![make_window(
            1,
            0.0,
            10.0,
            vec![
                make_row(0.0, 20.0, 1, 80),
                make_row(20.0, 40.0, 81, 160),
                make_row(40.0, 60.0, 161, 240),
            ],
        )];
        // Hit first row (y=5.0)
        assert_eq!(charpos_at_pixel_in(&data, 0.0, 5.0), 1);
        // Hit second row (y=25.0), x=30 => col=3, charpos=81+3=84
        assert_eq!(charpos_at_pixel_in(&data, 30.0, 25.0), 84);
        // Hit third row (y=50.0), x=10 => col=1, charpos=161+1=162
        assert_eq!(charpos_at_pixel_in(&data, 10.0, 50.0), 162);
    }

    #[test]
    fn charpos_at_pixel_zero_char_w_uses_fallback() {
        let data = vec![make_window(1, 0.0, 0.0, vec![make_row(0.0, 20.0, 1, 80)])];
        // char_w=0.0 => fallback to 8.0
        // px=24.0, content_x=0.0 => col = floor(24/8) = 3
        // charpos = 1 + 3 = 4
        assert_eq!(charpos_at_pixel_in(&data, 24.0, 10.0), 4);
    }

    #[test]
    fn charpos_at_pixel_negative_char_w_uses_fallback() {
        let data = vec![make_window(1, 0.0, -5.0, vec![make_row(0.0, 20.0, 1, 80)])];
        // char_w=-5.0 => fallback to 8.0 (condition: char_w > 0.0 is false)
        // px=16.0 => col = floor(16/8) = 2, charpos = 1+2 = 3
        assert_eq!(charpos_at_pixel_in(&data, 16.0, 10.0), 3);
    }

    #[test]
    fn charpos_at_pixel_row_boundary_exclusive_end() {
        let data = vec![make_window(
            1,
            0.0,
            10.0,
            vec![make_row(0.0, 20.0, 1, 80), make_row(20.0, 40.0, 81, 160)],
        )];
        // y=20.0 is exactly at y_end of row 0 and y_start of row 1
        // Row 0: py >= 0.0 && py < 20.0 => false for py=20.0
        // Row 1: py >= 20.0 && py < 40.0 => true
        assert_eq!(charpos_at_pixel_in(&data, 0.0, 20.0), 81);
    }

    #[test]
    fn charpos_at_pixel_first_column() {
        let data = vec![make_window(1, 0.0, 10.0, vec![make_row(0.0, 20.0, 1, 80)])];
        // px=0.0, content_x=0.0 => col=0, charpos=1+0=1
        assert_eq!(charpos_at_pixel_in(&data, 0.0, 10.0), 1);
    }

    #[test]
    fn charpos_at_pixel_fractional_column() {
        let data = vec![make_window(1, 0.0, 10.0, vec![make_row(0.0, 20.0, 1, 80)])];
        // px=15.0, char_w=10.0 => col = floor(15/10) = floor(1.5) = 1
        // charpos = 1 + 1 = 2
        assert_eq!(charpos_at_pixel_in(&data, 15.0, 10.0), 2);
    }

    // --- window_charpos_in tests ---

    #[test]
    fn window_charpos_returns_neg1_for_unknown_window() {
        let data = vec![make_window(1, 0.0, 10.0, vec![make_row(0.0, 20.0, 1, 80)])];
        // Window ID 999 does not exist
        assert_eq!(window_charpos_in(&data, 999, 10.0, 10.0), -1);
    }

    #[test]
    fn window_charpos_returns_neg1_for_empty_data() {
        assert_eq!(window_charpos_in(&[], 1, 10.0, 10.0), -1);
    }

    #[test]
    fn window_charpos_basic_hit() {
        let data = vec![make_window(
            42,
            10.0,
            8.0,
            vec![make_row(0.0, 16.0, 100, 200)],
        )];
        // wx=26.0, content_x=10.0, char_w=8.0 => col = floor((26-10)/8) = 2
        // charpos = 100 + 2 = 102
        assert_eq!(window_charpos_in(&data, 42, 26.0, 8.0), 102);
    }

    #[test]
    fn window_charpos_past_last_row_returns_last_charpos_end() {
        let data = vec![make_window(
            1,
            0.0,
            10.0,
            vec![make_row(0.0, 20.0, 1, 80), make_row(20.0, 40.0, 81, 160)],
        )];
        // wy=100.0 is past all rows; should return last row's charpos_end
        assert_eq!(window_charpos_in(&data, 1, 0.0, 100.0), 160);
    }

    #[test]
    fn window_charpos_returns_neg1_for_window_with_no_rows() {
        let data = vec![make_window(1, 0.0, 10.0, vec![])];
        // Window has no rows, past-last-row branch: rows.last() is None => -1
        assert_eq!(window_charpos_in(&data, 1, 0.0, 10.0), -1);
    }

    #[test]
    fn window_charpos_zero_char_w_fallback() {
        let data = vec![make_window(7, 0.0, 0.0, vec![make_row(0.0, 20.0, 50, 130)])];
        // char_w=0.0 => fallback to 8.0
        // wx=40.0 => col = floor(40/8) = 5, charpos = 50+5 = 55
        assert_eq!(window_charpos_in(&data, 7, 40.0, 10.0), 55);
    }

    #[test]
    fn window_charpos_multiple_windows_selects_correct_one() {
        let data = vec![
            make_window(1, 0.0, 10.0, vec![make_row(0.0, 20.0, 1, 80)]),
            make_window(2, 0.0, 10.0, vec![make_row(0.0, 20.0, 500, 580)]),
            make_window(3, 0.0, 10.0, vec![make_row(0.0, 20.0, 1000, 1080)]),
        ];
        // Should find window 2 specifically
        assert_eq!(window_charpos_in(&data, 2, 0.0, 10.0), 500);
        // Should find window 3
        assert_eq!(window_charpos_in(&data, 3, 0.0, 10.0), 1000);
    }

    #[test]
    fn window_charpos_x_clamped_left() {
        let data = vec![make_window(
            1,
            50.0,
            10.0,
            vec![make_row(0.0, 20.0, 200, 280)],
        )];
        // wx=10.0, content_x=50.0 => (10-50)/10 = -4.0, clamped to 0 => col=0
        // charpos = 200 + 0 = 200
        assert_eq!(window_charpos_in(&data, 1, 10.0, 10.0), 200);
    }

    #[test]
    fn window_charpos_x_clamped_to_row_end() {
        let data = vec![make_window(
            1,
            0.0,
            10.0,
            vec![make_row(0.0, 20.0, 100, 105)],
        )];
        // wx=300.0 => col=30, charpos=100+30=130, clamped to charpos_end=105
        assert_eq!(window_charpos_in(&data, 1, 300.0, 10.0), 105);
    }

    #[test]
    fn window_charpos_y_between_rows_returns_last_charpos() {
        let data = vec![make_window(
            1,
            0.0,
            10.0,
            vec![
                make_row(0.0, 20.0, 1, 80),
                // Gap between rows 20..40 (no row covers 20..30)
                make_row(30.0, 50.0, 81, 160),
            ],
        )];
        // wy=25.0 is between rows (after row 0 ends at 20.0, before row 1 starts at 30.0)
        // Neither row matches, so falls through to past-last-row: last().charpos_end = 160
        assert_eq!(window_charpos_in(&data, 1, 0.0, 25.0), 160);
    }

    // --- Public API tests (verify wrappers return -1 with FRAME_HIT_DATA = None) ---
    // These test the None path of the public functions. They are safe because
    // they only read the global (which defaults to None).

    #[test]
    fn public_charpos_at_pixel_no_data_returns_neg1() {
        // FRAME_HIT_DATA starts as None (or may have been set by another test);
        // we explicitly set it to None to be safe.
        unsafe {
            *std::ptr::addr_of_mut!(FRAME_HIT_DATA) = None;
        }
        assert_eq!(hit_test_charpos_at_pixel(0.0, 0.0), -1);
    }

    #[test]
    fn public_window_charpos_no_data_returns_neg1() {
        unsafe {
            *std::ptr::addr_of_mut!(FRAME_HIT_DATA) = None;
        }
        assert_eq!(hit_test_window_charpos(1, 0.0, 0.0), -1);
    }
}
