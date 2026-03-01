//! Shared types and utilities for visual effect modules.
//!
//! Contains the EffectCtx struct and helper functions used by
//! cursor_effects, window_effects, and pattern_effects.

use super::super::vertex::RectVertex;
use crate::core::types::{Color, AnimatedCursor};
use crate::core::frame_glyphs::{CursorStyle, FrameGlyph, FrameGlyphBuffer};
use crate::effect_config::EffectsConfig;

/// Shared context for effect vertex computation.
/// Holds immutable references to data needed by most effects.
pub(super) struct EffectCtx<'a> {
    pub effects: &'a EffectsConfig,
    pub frame_glyphs: &'a FrameGlyphBuffer,
    pub animated_cursor: &'a Option<AnimatedCursor>,
    pub cursor_visible: bool,
    pub mouse_pos: (f32, f32),
    pub surface_width: u32,
    pub surface_height: u32,
    pub aurora_start: std::time::Instant,
    pub scale_factor: f32,
    /// Logical frame width (frame_glyphs.width or surface_width/scale_factor)
    pub logical_w: f32,
    /// Logical frame height
    pub logical_h: f32,
    /// Renderer physical width as float
    pub renderer_width: f32,
    /// Renderer physical height as float
    pub renderer_height: f32,
}

/// Push a rectangle (6 vertices = 2 triangles) into a vertex buffer.
/// Free function equivalent of WgpuRenderer::add_rect.
pub(super) fn push_rect(
    vertices: &mut Vec<RectVertex>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    color: &Color,
) {
    let color_arr = [color.r, color.g, color.b, color.a];
    let x0 = x;
    let y0 = y;
    let x1 = x + width;
    let y1 = y + height;

    vertices.push(RectVertex { position: [x0, y0], color: color_arr });
    vertices.push(RectVertex { position: [x1, y0], color: color_arr });
    vertices.push(RectVertex { position: [x0, y1], color: color_arr });
    vertices.push(RectVertex { position: [x1, y0], color: color_arr });
    vertices.push(RectVertex { position: [x1, y1], color: color_arr });
    vertices.push(RectVertex { position: [x0, y1], color: color_arr });
}

/// Find the active cursor position from animated cursor or frame glyphs.
/// Returns (x, y, width, height) of the active (non-hollow) cursor.
pub(super) fn find_cursor_pos(
    animated_cursor: &Option<AnimatedCursor>,
    frame_glyphs: &FrameGlyphBuffer,
) -> Option<(f32, f32, f32, f32)> {
    if let Some(anim) = animated_cursor {
        return Some((anim.x, anim.y, anim.width, anim.height));
    }
    for glyph in &frame_glyphs.glyphs {
        if let FrameGlyph::Cursor { x, y, width, height, style, .. } = glyph {
            if !style.is_hollow() {
                return Some((*x, *y, *width, *height));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_rect_single() {
        let mut vertices = Vec::new();
        let color = Color::new(1.0, 0.5, 0.25, 0.8);

        push_rect(&mut vertices, 10.0, 20.0, 30.0, 40.0, &color);

        // Should push exactly 6 vertices (2 triangles)
        assert_eq!(vertices.len(), 6);

        // Check positions form correct rectangle
        assert_eq!(vertices[0].position, [10.0, 20.0]); // top-left
        assert_eq!(vertices[1].position, [40.0, 20.0]); // top-right
        assert_eq!(vertices[2].position, [10.0, 60.0]); // bottom-left
        assert_eq!(vertices[3].position, [40.0, 20.0]); // top-right (triangle 2)
        assert_eq!(vertices[4].position, [40.0, 60.0]); // bottom-right
        assert_eq!(vertices[5].position, [10.0, 60.0]); // bottom-left (triangle 2)

        // Check color propagated to all vertices
        let expected_color = [1.0, 0.5, 0.25, 0.8];
        for vertex in &vertices {
            assert_eq!(vertex.color, expected_color);
        }
    }

    #[test]
    fn test_push_rect_multiple() {
        let mut vertices = Vec::new();
        let color1 = Color::new(1.0, 0.0, 0.0, 1.0);
        let color2 = Color::new(0.0, 1.0, 0.0, 1.0);

        push_rect(&mut vertices, 0.0, 0.0, 10.0, 10.0, &color1);
        push_rect(&mut vertices, 20.0, 20.0, 15.0, 15.0, &color2);

        // Should have 12 vertices (6 per rect)
        assert_eq!(vertices.len(), 12);

        // Check first rect has red color
        for i in 0..6 {
            assert_eq!(vertices[i].color, [1.0, 0.0, 0.0, 1.0]);
        }

        // Check second rect has green color
        for i in 6..12 {
            assert_eq!(vertices[i].color, [0.0, 1.0, 0.0, 1.0]);
        }
    }

    #[test]
    fn test_push_rect_zero_size() {
        let mut vertices = Vec::new();
        let color = Color::new(0.5, 0.5, 0.5, 1.0);

        push_rect(&mut vertices, 100.0, 100.0, 0.0, 0.0, &color);

        // Even zero-size rect pushes 6 vertices
        assert_eq!(vertices.len(), 6);

        // All vertices should be at the same position
        for vertex in &vertices {
            assert_eq!(vertex.position[0], 100.0);
            assert_eq!(vertex.position[1], 100.0);
        }
    }

    #[test]
    fn test_push_rect_negative_coords() {
        let mut vertices = Vec::new();
        let color = Color::new(0.0, 0.0, 1.0, 0.5);

        push_rect(&mut vertices, -50.0, -30.0, 20.0, 15.0, &color);

        assert_eq!(vertices.len(), 6);
        assert_eq!(vertices[0].position, [-50.0, -30.0]); // top-left
        assert_eq!(vertices[1].position, [-30.0, -30.0]); // top-right
        assert_eq!(vertices[2].position, [-50.0, -15.0]); // bottom-left
        assert_eq!(vertices[4].position, [-30.0, -15.0]); // bottom-right
    }

    #[test]
    fn test_find_cursor_pos_animated() {
        let animated = Some(AnimatedCursor {
            window_id: 1,
            x: 100.0,
            y: 200.0,
            width: 10.0,
            height: 20.0,
            corners: None,
            frame_id: 0,
        });
        let frame_glyphs = FrameGlyphBuffer::new();

        let result = find_cursor_pos(&animated, &frame_glyphs);

        assert_eq!(result, Some((100.0, 200.0, 10.0, 20.0)));
    }

    #[test]
    fn test_find_cursor_pos_from_glyphs() {
        let animated = None;
        let mut frame_glyphs = FrameGlyphBuffer::new();

        // Add a non-hollow cursor (style FilledBox)
        frame_glyphs.glyphs.push(FrameGlyph::Cursor {
            window_id: 1,
            x: 50.0,
            y: 60.0,
            width: 8.0,
            height: 16.0,
            style: CursorStyle::FilledBox,
            color: Color::new(1.0, 1.0, 1.0, 1.0),
        });

        let result = find_cursor_pos(&animated, &frame_glyphs);

        assert_eq!(result, Some((50.0, 60.0, 8.0, 16.0)));
    }

    #[test]
    fn test_find_cursor_pos_skips_hollow() {
        let animated = None;
        let mut frame_glyphs = FrameGlyphBuffer::new();

        // Add hollow cursor (Hollow = inactive window)
        frame_glyphs.glyphs.push(FrameGlyph::Cursor {
            window_id: 1,
            x: 10.0,
            y: 20.0,
            width: 8.0,
            height: 16.0,
            style: CursorStyle::Hollow,
            color: Color::new(1.0, 1.0, 1.0, 1.0),
        });

        // Add non-hollow cursor
        frame_glyphs.glyphs.push(FrameGlyph::Cursor {
            window_id: 2,
            x: 30.0,
            y: 40.0,
            width: 2.0,
            height: 16.0,
            style: CursorStyle::Bar(2.0), // bar cursor
            color: Color::new(1.0, 1.0, 1.0, 1.0),
        });

        let result = find_cursor_pos(&animated, &frame_glyphs);

        // Should return the non-hollow cursor, not the hollow one
        assert_eq!(result, Some((30.0, 40.0, 2.0, 16.0)));
    }

    #[test]
    fn test_find_cursor_pos_first_non_hollow() {
        let animated = None;
        let mut frame_glyphs = FrameGlyphBuffer::new();

        // Add first non-hollow cursor
        frame_glyphs.glyphs.push(FrameGlyph::Cursor {
            window_id: 1,
            x: 100.0,
            y: 200.0,
            width: 8.0,
            height: 16.0,
            style: CursorStyle::FilledBox,
            color: Color::new(1.0, 1.0, 1.0, 1.0),
        });

        // Add second non-hollow cursor
        frame_glyphs.glyphs.push(FrameGlyph::Cursor {
            window_id: 2,
            x: 300.0,
            y: 400.0,
            width: 8.0,
            height: 16.0,
            style: CursorStyle::Hbar(2.0), // hbar cursor
            color: Color::new(1.0, 1.0, 1.0, 1.0),
        });

        let result = find_cursor_pos(&animated, &frame_glyphs);

        // Should return the first non-hollow cursor
        assert_eq!(result, Some((100.0, 200.0, 8.0, 16.0)));
    }

    #[test]
    fn test_find_cursor_pos_none_found() {
        let animated = None;
        let mut frame_glyphs = FrameGlyphBuffer::new();

        // Add only hollow cursors
        frame_glyphs.glyphs.push(FrameGlyph::Cursor {
            window_id: 1,
            x: 10.0,
            y: 20.0,
            width: 8.0,
            height: 16.0,
            style: CursorStyle::Hollow,
            color: Color::new(1.0, 1.0, 1.0, 1.0),
        });

        let result = find_cursor_pos(&animated, &frame_glyphs);

        assert_eq!(result, None);
    }

    #[test]
    fn test_find_cursor_pos_empty() {
        let animated = None;
        let frame_glyphs = FrameGlyphBuffer::new();

        let result = find_cursor_pos(&animated, &frame_glyphs);

        assert_eq!(result, None);
    }
}
