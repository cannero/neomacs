//! Scene Management FFI functions
//!
//! Resize, begin_frame, add_window, window_info, cursor, border, scroll bar.

use super::*;

// ============================================================================
// Scene Management
// ============================================================================

/// Resize the display
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_resize(
    handle: *mut NeomacsDisplay,
    width: c_int,
    height: c_int,
) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;
    let new_width = width as f32;
    let new_height = height as f32;

    // Only clear glyphs if size actually changed
    let size_changed = (display.frame_glyphs.width - new_width).abs() > 1.0
        || (display.frame_glyphs.height - new_height).abs() > 1.0;

    if size_changed {
        let old_w = display.frame_glyphs.width;
        let old_h = display.frame_glyphs.height;
        tracing::info!(
            "neomacs_display_resize: {}x{} -> {}x{}",
            old_w,
            old_h,
            width,
            height
        );
        display.scene = Scene::new(new_width, new_height);
        display.frame_glyphs.width = new_width;
        display.frame_glyphs.height = new_height;
        display.frame_glyphs.clear_all();
    }

    if let Some(backend) = display.get_backend() {
        backend.resize(width as u32, height as u32);
    }
}

/// Begin building a new frame
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_begin_frame(handle: *mut NeomacsDisplay) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;
    display.frame_counter += 1;
    display.in_frame = true;

    let frame_num = display.frame_counter;
    let (bg_r, bg_g, bg_b) = (
        display.scene.background.r,
        display.scene.background.g,
        display.scene.background.b,
    );
    debug!(
        "begin_frame: frame={} scene_bg=({:.3},{:.3},{:.3})",
        frame_num, bg_r, bg_g, bg_b
    );

    // Rust layout is authoritative: clear everything and rebuild from scratch.
    display.frame_glyphs.width = display.scene.width;
    display.frame_glyphs.height = display.scene.height;
    display.frame_glyphs.background = display.scene.background;
    display.begin_transition_hint_frame();
    display.frame_glyphs.clear_all();
}

/// Set frame identity for child frame support.
/// Called after begin_frame_window, before glyphs are added.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_set_frame_identity(
    handle: *mut NeomacsDisplay,
    frame_id: u64,
    parent_id: u64,
    parent_x: f32,
    parent_y: f32,
    z_order: c_int,
    border_width: f32,
    border_color: u32,
    no_accept_focus: c_int,
    background_alpha: f32,
) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;
    display.frame_glyphs.set_frame_identity(
        frame_id,
        parent_id,
        parent_x,
        parent_y,
        z_order,
        border_width,
        Color::from_pixel(border_color),
        no_accept_focus != 0,
        background_alpha,
    );
}

/// Add a window to the current frame
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_add_window(
    handle: *mut NeomacsDisplay,
    window_id: c_int,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    bg_color: u32,
    selected: c_int,
) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;
    let current_frame = display.frame_counter;

    // Track current window for subsequent glyph operations
    display.current_window_id = window_id;
    display.current_window_x = x;
    display.current_window_width = width;

    let color = Color::from_pixel(bg_color);
    debug!(
        "neomacs_display_add_window: id={} at ({},{}) size {}x{} bg=0x{:06x}->({:.3},{:.3},{:.3})",
        window_id, x, y, width, height, bg_color, color.r, color.g, color.b
    );
    display
        .frame_glyphs
        .add_background(x, y, width, height, color);

    // Scene graph path (used for winit windows and legacy rendering)...
    // Find existing window by ID or create new one
    let window_idx = display
        .get_target_scene()
        .windows
        .iter()
        .position(|w| w.window_id == window_id);

    if let Some(idx) = window_idx {
        // Update existing window
        let window = &mut display.get_target_scene().windows[idx];
        window.bounds = Rect::new(x, y, width, height);
        window.background = Color::from_pixel(bg_color);
        window.selected = selected != 0;
        window.last_frame_touched = current_frame;
    } else {
        // Create new window
        let window = WindowScene {
            window_id,
            bounds: Rect::new(x, y, width, height),
            background: Color::from_pixel(bg_color),
            cursor: None,
            scroll_offset: 0.0,
            selected: selected != 0,
            mode_line_height: 0,
            header_line_height: 0,
            last_frame_touched: current_frame,
        };
        display.get_target_scene().windows.push(window);
    }
}

/// Add per-window metadata for animation detection
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_add_window_info(
    handle: *mut NeomacsDisplay,
    window_id: i64,
    buffer_id: u64,
    window_start: i64,
    window_end: i64,
    buffer_size: i64,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    mode_line_height: f32,
    header_line_height: f32,
    tab_line_height: f32,
    selected: c_int,
    is_minibuffer: c_int,
    char_height: f32,
    buffer_file_name: *const c_char,
    modified: c_int,
) {
    if handle.is_null() {
        return;
    }
    let file_name = if buffer_file_name.is_null() {
        String::new()
    } else {
        CStr::from_ptr(buffer_file_name)
            .to_string_lossy()
            .into_owned()
    };
    let display = &mut *handle;
    display.frame_glyphs.add_window_info(
        window_id,
        buffer_id,
        window_start,
        window_end,
        buffer_size,
        x,
        y,
        width,
        height,
        mode_line_height,
        header_line_height,
        tab_line_height,
        selected != 0,
        is_minibuffer != 0,
        char_height,
        file_name,
        modified != 0,
    );
    if let Some(curr) = display.frame_glyphs.window_infos.last().cloned() {
        display.record_transition_hint_window(&curr);
    }
}

/// Set cursor for a specific window
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_set_cursor(
    handle: *mut NeomacsDisplay,
    window_id: c_int,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    style: c_int,
    color: u32,
    visible: c_int,
) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;

    if visible != 0 {
        // Convert C-side style integer to CursorStyle enum.
        // For bar, use width as the bar width; for hbar, use height.
        let cursor_style = match style {
            0 => Some(crate::core::frame_glyphs::CursorStyle::FilledBox),
            1 => Some(crate::core::frame_glyphs::CursorStyle::Bar(width)),
            2 => Some(crate::core::frame_glyphs::CursorStyle::Hbar(height)),
            3 => Some(crate::core::frame_glyphs::CursorStyle::Hollow),
            _ => None,
        };
        if let Some(cs) = cursor_style {
            display.frame_glyphs.add_cursor(
                window_id,
                x,
                y,
                width,
                height,
                cs,
                Color::from_pixel(color),
            );
        }
    }

    // Legacy scene graph path...
    let cursor_visible = visible != 0;

    // Find the window by ID
    if let Some(window) = display
        .get_target_scene()
        .windows
        .iter_mut()
        .find(|w| w.window_id == window_id)
    {
        window.cursor = Some(CursorState {
            x,
            y,
            width,
            height,
            style: match style {
                0 => SceneCursorStyle::Box,
                1 => SceneCursorStyle::Bar,
                2 => SceneCursorStyle::Underline,
                3 => SceneCursorStyle::Hollow,
                _ => SceneCursorStyle::Box,
            },
            color: Color::from_pixel(color),
            visible: cursor_visible,
        });
    }
}

/// Set inverse video info for a filled box cursor
///
/// Called from C for style 0 (filled box) cursors. Provides the cursor
/// background color (the cursor rect) and cursor foreground color (for
/// redrawing the character under the cursor in inverse video).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_set_cursor_inverse(
    handle: *mut NeomacsDisplay,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    cursor_bg_rgba: u32,
    cursor_fg_rgba: u32,
) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;
    display.frame_glyphs.set_cursor_inverse(
        x,
        y,
        width,
        height,
        Color::from_pixel(cursor_bg_rgba),
        Color::from_pixel(cursor_fg_rgba),
    );
}

/// Draw a border rectangle (for window dividers)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_draw_border(
    handle: *mut NeomacsDisplay,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    color: u32,
) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;

    display.frame_glyphs.add_border(
        x as f32,
        y as f32,
        width as f32,
        height as f32,
        Color::from_pixel(color),
    );

    // Legacy path
    display.get_target_scene().add_border(
        x as f32,
        y as f32,
        width as f32,
        height as f32,
        Color::from_pixel(color),
    );
}

/// Add a GPU-rendered scroll bar
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_add_scroll_bar(
    handle: *mut NeomacsDisplay,
    horizontal: c_int,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    thumb_start: c_int,
    thumb_size: c_int,
    track_color: u32,
    thumb_color: u32,
) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;

    display.frame_glyphs.add_scroll_bar(
        horizontal != 0,
        x as f32,
        y as f32,
        width as f32,
        height as f32,
        thumb_start as f32,
        thumb_size as f32,
        Color::from_pixel(track_color),
        Color::from_pixel(thumb_color),
    );
}
