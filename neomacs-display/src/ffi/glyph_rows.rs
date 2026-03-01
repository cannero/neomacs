//! Glyph Row and Face Management FFI functions
//!
//! Row operations (begin/end row, add char/stretch/image glyphs) and face registration.

use super::*;

// ============================================================================
// Glyph Row Management
// ============================================================================

/// Begin a new glyph row for the current window
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_begin_row(
    handle: *mut NeomacsDisplay,
    y: c_int,  // Frame-absolute Y coordinate
    x: c_int,  // Starting X position for this glyph string
    height: c_int,
    ascent: c_int,
    mode_line: c_int,
    header_line: c_int,
) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;

    // Track current row Y (frame-absolute) and X for glyph additions
    display.current_row_y = y;
    display.current_row_x = x;  // Set starting X for this glyph string
    display.current_row_height = height;  // Store for hybrid path
    display.current_row_ascent = ascent;  // Store for hybrid path
    // Mode-line and header-line are overlays that render on top
    display.current_row_is_overlay = mode_line != 0 || header_line != 0;

    // Hybrid path: we don't need window tracking - just use frame-absolute coords
}

/// Add a character glyph to the current row
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_add_char_glyph(
    handle: *mut NeomacsDisplay,
    charcode: u32,
    _face_id: u32,
    pixel_width: c_int,
    _ascent: c_int,
    _descent: c_int,
) {
    if handle.is_null() {
        return;
    }

    // Catch panics to prevent aborting across FFI boundary
    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let display = &mut *handle;
        let current_y = display.current_row_y;  // Frame-absolute Y
        let current_x = display.current_row_x;
        let c = char::from_u32(charcode).unwrap_or('\u{FFFD}');

        // Debug: log first char of each row to trace Y coordinates
        static mut LAST_DEBUG_Y: i32 = -1;
        if current_y != LAST_DEBUG_Y && current_x < 20 {
            let is_overlay = display.current_row_is_overlay;
        tracing::debug!("add_char_glyph: y={} x={} char='{}' overlay={}",
                current_y, current_x, c, is_overlay);
            LAST_DEBUG_Y = current_y;
        }

        display.frame_glyphs.add_char(
            c,
            current_x as f32,
            current_y as f32,
            pixel_width as f32,
            display.current_row_height as f32,
            display.current_row_ascent as f32,
            display.current_row_is_overlay,
        );
        display.current_row_x += pixel_width;
    }));

    if let Err(e) = result {
        error!("PANIC in neomacs_display_add_char_glyph: {:?}", e);
    }
}

/// Add a stretch (whitespace) glyph to the current row
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_add_stretch_glyph(
    handle: *mut NeomacsDisplay,
    pixel_width: c_int,
    height: c_int,
    face_id: u32,
) {
    if handle.is_null() {
        return;
    }

    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let display = &mut *handle;
        let current_y = display.current_row_y;  // Frame-absolute Y
        let current_x = display.current_row_x;

        // Get the background color from the current face
        let bg_color = display.frame_glyphs.get_current_bg()
            .unwrap_or(display.frame_glyphs.background);

        display.frame_glyphs.add_stretch(
            current_x as f32,
            current_y as f32,
            pixel_width as f32,
            height as f32,
            bg_color,
            face_id,
            display.current_row_is_overlay,
        );
        display.current_row_x += pixel_width;
    }));

    if let Err(e) = result {
        error!("PANIC in neomacs_display_add_stretch_glyph: {:?}", e);
    }
}

/// Add an image glyph to the current row
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_add_image_glyph(
    handle: *mut NeomacsDisplay,
    image_id: u32,
    pixel_width: c_int,
    pixel_height: c_int,
) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;
    let current_y = display.current_row_y;  // Frame-absolute Y
    let current_x = display.current_row_x;

    tracing::info!("add_image_glyph: id={}, pos=({},{}) size={}x{}",
               image_id, current_x, current_y, pixel_width, pixel_height);
    display.frame_glyphs.add_image(
        image_id,
        current_x as f32,
        current_y as f32,
        pixel_width as f32,
        pixel_height as f32,
    );
    display.current_row_x += pixel_width;
}

/// End the current row
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_end_row(handle: *mut NeomacsDisplay) {
    // Currently a no-op, but could be used for row finalization
    let _ = handle;
}

// ============================================================================
// Face Management
// ============================================================================

/// Register or update a face
/// Colors are in 0xRRGGBB format
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_set_face(
    handle: *mut NeomacsDisplay,
    face_id: u32,
    foreground: u32,  // 0xRRGGBB
    background: u32,  // 0xRRGGBB
    font_family: *const c_char, // Font family name (e.g., "monospace", "Sans")
    font_weight: u16, // 400=normal, 700=bold
    is_italic: c_int,
    font_size: c_int, // Font size in pixels (from face->font->pixel_size)
    underline_style: c_int, // 0=none, 1=line, 2=wave, 3=double, 4=dotted, 5=dashed
    underline_color: u32,
    box_type: c_int,  // 0=none, 1=line, 2=raised3d, 3=sunken3d
    box_color: u32,
    box_line_width: c_int,
    box_corner_radius: c_int, // 0=sharp corners, >0=rounded
    strike_through: c_int, // 0=none, 1=enabled
    strike_through_color: u32, // 0xRRGGBB
    overline: c_int,  // 0=none, 1=enabled
    overline_color: u32, // 0xRRGGBB
    font_ascent: c_int,  // FONT_BASE(font) in pixels
    font_descent: c_int, // FONT_DESCENT(font) in pixels
    ul_position: c_int,  // font->underline_position
    ul_thickness: c_int, // font->underline_thickness
) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;

    // Convert font family from C string (defensively)
    let font_family_str = if font_family.is_null() {
        "monospace".to_string()
    } else {
        // Safety: need to verify the C string is valid
        match std::ffi::CStr::from_ptr(font_family).to_str() {
            Ok(s) if !s.is_empty() => s.to_string(),
            _ => "monospace".to_string(),
        }
    };

    trace!("set_face: id={}, fg=0x{:06x}, bg=0x{:06x}, family={}, weight={}", face_id, foreground, background, font_family_str, font_weight);

    // Convert colors from 0xRRGGBB sRGB to linear for GPU rendering.
    // The surface uses an sRGB format, so the GPU expects linear values
    // and automatically applies sRGB encoding on framebuffer write.
    let fg = Color {
        r: ((foreground >> 16) & 0xFF) as f32 / 255.0,
        g: ((foreground >> 8) & 0xFF) as f32 / 255.0,
        b: (foreground & 0xFF) as f32 / 255.0,
        a: 1.0,
    }.srgb_to_linear();

    let bg = Color {
        r: ((background >> 16) & 0xFF) as f32 / 255.0,
        g: ((background >> 8) & 0xFF) as f32 / 255.0,
        b: (background & 0xFF) as f32 / 255.0,
        a: 1.0,
    }.srgb_to_linear();

    // Build attributes
    let mut attrs = FaceAttributes::empty();
    if font_weight >= 700 {
        attrs |= FaceAttributes::BOLD;
    }
    if is_italic != 0 {
        attrs |= FaceAttributes::ITALIC;
    }
    if underline_style != 0 {
        attrs |= FaceAttributes::UNDERLINE;
    }
    if box_type != 0 {
        attrs |= FaceAttributes::BOX;
    }
    if strike_through != 0 {
        attrs |= FaceAttributes::STRIKE_THROUGH;
    }
    if overline != 0 {
        attrs |= FaceAttributes::OVERLINE;
    }

    // Underline style
    let ul_style = match underline_style {
        1 => UnderlineStyle::Line,
        2 => UnderlineStyle::Wave,
        3 => UnderlineStyle::Double,
        4 => UnderlineStyle::Dotted,
        5 => UnderlineStyle::Dashed,
        _ => UnderlineStyle::None,
    };

    // Box type
    let bx_type = match box_type {
        1 => BoxType::Line,
        2 => BoxType::Raised3D,
        3 => BoxType::Sunken3D,
        _ => BoxType::None,
    };

    // Underline color
    let ul_color = if underline_color != 0 {
        Some(Color {
            r: ((underline_color >> 16) & 0xFF) as f32 / 255.0,
            g: ((underline_color >> 8) & 0xFF) as f32 / 255.0,
            b: (underline_color & 0xFF) as f32 / 255.0,
            a: 1.0,
        }.srgb_to_linear())
    } else {
        None
    };

    // Box color
    let bx_color = if box_color != 0 {
        Some(Color {
            r: ((box_color >> 16) & 0xFF) as f32 / 255.0,
            g: ((box_color >> 8) & 0xFF) as f32 / 255.0,
            b: (box_color & 0xFF) as f32 / 255.0,
            a: 1.0,
        }.srgb_to_linear())
    } else {
        None
    };

    // Strike-through color
    let st_color = if strike_through != 0 && strike_through_color != 0 {
        Some(Color {
            r: ((strike_through_color >> 16) & 0xFF) as f32 / 255.0,
            g: ((strike_through_color >> 8) & 0xFF) as f32 / 255.0,
            b: (strike_through_color & 0xFF) as f32 / 255.0,
            a: 1.0,
        }.srgb_to_linear())
    } else {
        None
    };

    // Overline color
    let ol_color = if overline != 0 && overline_color != 0 {
        Some(Color {
            r: ((overline_color >> 16) & 0xFF) as f32 / 255.0,
            g: ((overline_color >> 8) & 0xFF) as f32 / 255.0,
            b: (overline_color & 0xFF) as f32 / 255.0,
            a: 1.0,
        }.srgb_to_linear())
    } else {
        None
    };

    let new_font_size = if font_size > 0 { font_size as f32 } else { 14.0 };

    // No text-scale clearing needed: with full-frame rebuild, the buffer is
    // always cleared at the start of each frame and rebuilt from scratch.

    let face = Face {
        id: face_id,
        foreground: fg,
        background: bg,
        underline_color: ul_color,
        overline_color: ol_color,
        strike_through_color: st_color,
        box_color: bx_color,
        font_family: font_family_str.clone(),
        font_size: new_font_size,
        font_weight,
        attributes: attrs,
        underline_style: ul_style,
        box_type: bx_type,
        box_line_width,
        box_corner_radius,
        box_border_style: 0,
        box_border_speed: 1.0,
        box_color2: None,
        font_file_path: None,
        font_ascent,
        font_descent,
        underline_position: if ul_position > 0 { ul_position } else { 1 },
        underline_thickness: if ul_thickness > 0 { ul_thickness } else { 1 },
    };

    // Store face for later lookup during rendering
    display.faces.insert(face_id, face.clone());

    // Also store in frame glyph buffer so render thread gets full face data
    display.frame_glyphs.faces.insert(face_id, face.clone());

    // Hybrid path: set current face attributes for frame glyph buffer
    if display.use_hybrid {
        let bg_opt = Some(bg);
        let ul_color_opt = if underline_color != 0 { ul_color } else { None };
        let st_color_opt = if strike_through != 0 { st_color } else { None };
        let ol_color_opt = if overline != 0 { ol_color } else { None };
        display.frame_glyphs.set_face_with_font(
            face_id,
            fg,
            bg_opt,
            &font_family_str,
            font_weight,
            is_italic != 0,
            if font_size > 0 { font_size as f32 } else { 14.0 },
            underline_style as u8,
            ul_color_opt,
            strike_through as u8,
            st_color_opt,
            overline as u8,
            ol_color_opt,
            false, // overstrike (legacy path doesn't use it)
        );
    }

    // Register face in the scene
    display.get_target_scene().set_face(face.clone());
}

/// Set the frame/scene background color
/// Color is in 0xRRGGBB format
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_set_background(
    handle: *mut NeomacsDisplay,
    color: u32,  // 0xRRGGBB
) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;
    let bg = Color {
        r: ((color >> 16) & 0xFF) as f32 / 255.0,
        g: ((color >> 8) & 0xFF) as f32 / 255.0,
        b: (color & 0xFF) as f32 / 255.0,
        a: 1.0,
    }.srgb_to_linear();

    let target_scene = display.get_target_scene();
    target_scene.background = bg;

    // Also set background for existing windows
    for window in &mut target_scene.windows {
        window.background = bg;
    }
}

/// Set the frame/scene background alpha (for transparent backgrounds).
/// alpha is 0.0 (fully transparent) to 1.0 (fully opaque).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_set_background_alpha(
    handle: *mut NeomacsDisplay,
    alpha: f32,
) {
    if handle.is_null() {
        return;
    }

    let display = &mut *handle;
    let target_scene = display.get_target_scene();
    target_scene.background.a = alpha;

    for window in &mut target_scene.windows {
        window.background.a = alpha;
    }
}
