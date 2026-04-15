use super::{Frame, FrameManager, Window, WindowDisplayState, WindowId};
use crate::buffer::BufferId;
use crate::emacs_core::value::{Value, next_float_id};

#[derive(Clone, Copy, Debug, Default)]
pub struct WindowBufferDisplayDefaults {
    pub margins: Option<(usize, usize)>,
    pub fringes: Option<(Option<i32>, Option<i32>, bool)>,
    pub scroll_bars: Option<(Option<i32>, Value, Option<i32>, Value)>,
}

fn symbol_name(value: &Value) -> Option<&str> {
    value.as_symbol_name()
}

fn frame_line_height(frame: &Frame) -> i64 {
    frame.char_height.max(1.0).round() as i64
}

fn canon_y_from_pixel_y(frame: &Frame, y: i64) -> Value {
    let unit = frame_line_height(frame).max(1);
    if y % unit != 0 {
        Value::make_float(y as f64 / unit as f64)
    } else {
        Value::fixnum(y / unit)
    }
}

fn frame_default_left_fringe_width(frame: &Frame) -> i32 {
    frame
        .parameter("left-fringe")
        .and_then(|v| v.as_int())
        .and_then(|value| i32::try_from(value).ok())
        .unwrap_or(8)
}

fn frame_default_right_fringe_width(frame: &Frame) -> i32 {
    frame
        .parameter("right-fringe")
        .and_then(|v| v.as_int())
        .and_then(|value| i32::try_from(value).ok())
        .unwrap_or(8)
}

fn frame_vertical_scroll_bar_side(frame: &Frame) -> Option<Value> {
    let raw = frame.parameter("vertical-scroll-bars").unwrap_or_else(|| {
        if frame.effective_window_system().is_some() {
            Value::symbol("right")
        } else {
            Value::NIL
        }
    });
    match symbol_name(&raw) {
        Some("left") => Some(Value::symbol("left")),
        Some("right") => Some(Value::symbol("right")),
        _ if raw.is_nil() => None,
        _ if raw.is_truthy() => Some(Value::symbol("right")),
        _ => None,
    }
}

fn frame_has_horizontal_scroll_bar(frame: &Frame) -> bool {
    let raw = frame
        .parameter("horizontal-scroll-bars")
        .unwrap_or(Value::NIL);
    matches!(symbol_name(&raw), Some("bottom")) || raw.is_truthy()
}

fn frame_config_scroll_bar_width(frame: &Frame) -> i32 {
    frame
        .parameter("scroll-bar-width")
        .and_then(|v| v.as_int())
        .and_then(|value| i32::try_from(value).ok())
        .filter(|value| *value > 0)
        .unwrap_or_else(|| frame.char_width.max(1.0).round() as i32)
}

fn frame_config_scroll_bar_height(frame: &Frame) -> i32 {
    frame
        .parameter("scroll-bar-height")
        .and_then(|v| v.as_int())
        .and_then(|value| i32::try_from(value).ok())
        .filter(|value| *value > 0)
        .unwrap_or_else(|| frame.char_height.max(1.0).round() as i32)
}

fn effective_vertical_scroll_bar_type(
    frame: &Frame,
    display: &WindowDisplayState,
) -> Option<Value> {
    match symbol_name(&display.vertical_scroll_bar_type) {
        Some("left") => Some(Value::symbol("left")),
        Some("right") => Some(Value::symbol("right")),
        _ if display.vertical_scroll_bar_type.is_nil() => None,
        _ if display.vertical_scroll_bar_type.is_truthy() => frame_vertical_scroll_bar_side(frame),
        _ => None,
    }
}

fn effective_horizontal_scroll_bar_enabled(
    frame: &Frame,
    display: &WindowDisplayState,
    is_minibuffer: bool,
) -> bool {
    match symbol_name(&display.horizontal_scroll_bar_type) {
        Some("bottom") => true,
        _ if display.horizontal_scroll_bar_type.is_nil() => false,
        _ if is_minibuffer => false,
        _ if display.horizontal_scroll_bar_type.is_truthy() => {
            frame_has_horizontal_scroll_bar(frame)
        }
        _ => false,
    }
}

fn vertical_scroll_bar_area_width(frame: &Frame, display: &WindowDisplayState) -> i64 {
    if effective_vertical_scroll_bar_type(frame, display).is_some() {
        i64::from(if display.scroll_bar_width >= 0 {
            display.scroll_bar_width
        } else {
            frame_config_scroll_bar_width(frame)
        })
    } else {
        0
    }
}

fn left_scroll_bar_area_width(frame: &Frame, display: &WindowDisplayState) -> i64 {
    if matches!(effective_vertical_scroll_bar_type(frame, display), Some(value) if symbol_name(&value) == Some("left"))
    {
        vertical_scroll_bar_area_width(frame, display)
    } else {
        0
    }
}

fn right_scroll_bar_area_width(frame: &Frame, display: &WindowDisplayState) -> i64 {
    if matches!(effective_vertical_scroll_bar_type(frame, display), Some(value) if symbol_name(&value) == Some("right"))
    {
        vertical_scroll_bar_area_width(frame, display)
    } else {
        0
    }
}

fn horizontal_scroll_bar_area_height(
    frame: &Frame,
    display: &WindowDisplayState,
    is_minibuffer: bool,
) -> i64 {
    if effective_horizontal_scroll_bar_enabled(frame, display, is_minibuffer) {
        i64::from(if display.scroll_bar_height >= 0 {
            display.scroll_bar_height
        } else {
            frame_config_scroll_bar_height(frame)
        })
    } else {
        0
    }
}

fn vertical_scroll_bar_cols(frame: &Frame, display: &WindowDisplayState) -> i64 {
    let width = vertical_scroll_bar_area_width(frame, display);
    if width == 0 {
        0
    } else {
        (width + frame.char_width.max(1.0).round() as i64 - 1)
            / frame.char_width.max(1.0).round() as i64
    }
}

fn horizontal_scroll_bar_lines(
    frame: &Frame,
    display: &WindowDisplayState,
    is_minibuffer: bool,
) -> i64 {
    let height = horizontal_scroll_bar_area_height(frame, display, is_minibuffer);
    if height == 0 {
        0
    } else {
        (height + frame.char_height.max(1.0).round() as i64 - 1)
            / frame.char_height.max(1.0).round() as i64
    }
}

impl FrameManager {
    fn window_display_state(
        &self,
        window_id: WindowId,
    ) -> Option<(&Frame, &WindowDisplayState, bool)> {
        self.frames.values().find_map(|frame| {
            frame.find_window(window_id).and_then(|window| {
                window
                    .display()
                    .map(|display| (frame, display, frame.minibuffer_window == Some(window_id)))
            })
        })
    }

    fn window_display_location(&self, window_id: WindowId) -> Option<(super::FrameId, bool)> {
        let frame_id = self.find_window_frame_id(window_id)?;
        let frame = self.get(frame_id)?;
        Some((frame_id, frame.minibuffer_window == Some(window_id)))
    }

    /// Return WINDOW-ID's Lisp-visible vertical scroll amount.
    pub fn window_vscroll(&self, window_id: WindowId, pixelwise: bool) -> Option<Value> {
        let frame_id = self.find_window_frame_id(window_id)?;
        let frame = self.get(frame_id)?;
        let Window::Leaf { vscroll, .. } = frame.find_window(window_id)? else {
            return None;
        };
        if frame.effective_window_system().is_none() {
            return Some(Value::fixnum(0));
        }
        let pixels = -i64::from(*vscroll);
        Some(if pixelwise {
            Value::fixnum(pixels)
        } else {
            canon_y_from_pixel_y(frame, pixels)
        })
    }

    /// Set WINDOW-ID's vertical scroll amount and return the Lisp-visible value.
    pub fn set_window_vscroll(
        &mut self,
        window_id: WindowId,
        vscroll: f64,
        pixelwise: bool,
        preserve_vscroll_p: bool,
    ) -> Option<Value> {
        let frame_id = self.find_window_frame_id(window_id)?;
        let line_height = self
            .get(frame_id)
            .map(frame_line_height)
            .unwrap_or(1)
            .max(1);
        if self
            .get(frame_id)
            .is_none_or(|frame| frame.effective_window_system().is_none())
        {
            return Some(Value::fixnum(0));
        }

        let next_pixels = if pixelwise {
            vscroll.trunc() as i64
        } else {
            (vscroll * line_height as f64).trunc() as i64
        };
        let next_raw = std::cmp::min(-next_pixels, 0).clamp(i64::from(i32::MIN), 0) as i32;

        let frame = self.get_mut(frame_id)?;
        if let Some(Window::Leaf {
            vscroll,
            preserve_vscroll_p: preserve,
            ..
        }) = frame.find_window_mut(window_id)
        {
            *vscroll = next_raw;
            *preserve = preserve_vscroll_p;
            frame.display_snapshots.remove(&window_id);
        }

        let frame = self.get(frame_id)?;
        if frame.effective_window_system().is_none() {
            return Some(Value::fixnum(0));
        }
        let Window::Leaf { vscroll, .. } = frame.find_window(window_id)? else {
            return None;
        };
        let pixels = -i64::from(*vscroll);
        Some(if pixelwise {
            Value::fixnum(pixels)
        } else {
            canon_y_from_pixel_y(frame, pixels)
        })
    }

    /// Return window display table object for WINDOW-ID, or nil when unset.
    pub fn window_display_table(&self, window_id: WindowId) -> Value {
        self.window_display_state(window_id)
            .map(|(_, display, _)| display.display_table)
            .unwrap_or(Value::NIL)
    }

    /// Set window display table object for WINDOW-ID.
    pub fn set_window_display_table(&mut self, window_id: WindowId, table: Value) {
        if let Some((frame_id, _)) = self.window_display_location(window_id)
            && let Some(display) = self
                .get_mut(frame_id)
                .and_then(|frame| frame.find_window_mut(window_id))
                .and_then(Window::display_mut)
        {
            display.display_table = table;
        }
    }

    /// Return window cursor-type object for WINDOW-ID.
    ///
    /// GNU Emacs defaults to `t` when no explicit per-window cursor-type is set.
    pub fn window_cursor_type(&self, window_id: WindowId) -> Value {
        self.window_display_state(window_id)
            .map(|(_, display, _)| display.cursor_type)
            .unwrap_or(Value::T)
    }

    /// Set window cursor-type object for WINDOW-ID.
    pub fn set_window_cursor_type(&mut self, window_id: WindowId, cursor_type: Value) {
        if let Some((frame_id, _)) = self.window_display_location(window_id)
            && let Some(display) = self
                .get_mut(frame_id)
                .and_then(|frame| frame.find_window_mut(window_id))
                .and_then(Window::display_mut)
        {
            display.cursor_type = cursor_type;
        }
    }

    /// Return whether WINDOW-ID's cursor is logically visible.
    pub fn window_cursor_visible(&self, window_id: WindowId) -> bool {
        self.window_display_state(window_id)
            .map(|(_, display, _)| !display.cursor_off_p)
            .unwrap_or(true)
    }

    /// Set whether WINDOW-ID's cursor is logically visible.
    ///
    /// This mirrors GNU's `w->cursor_off_p`: redisplay may continue to own a
    /// physical cursor geometry while Lisp has requested that it stay hidden.
    pub fn set_window_cursor_visible(&mut self, window_id: WindowId, visible: bool) {
        if let Some((frame_id, _)) = self.window_display_location(window_id)
            && let Some(display) = self
                .get_mut(frame_id)
                .and_then(|frame| frame.find_window_mut(window_id))
                .and_then(Window::display_mut)
        {
            display.cursor_off_p = !visible;
        }
    }

    /// Return the effective fringe widths and raw flags for WINDOW-ID.
    pub fn window_fringes(&self, window_id: WindowId) -> Option<(i64, i64, bool, bool)> {
        let (frame, display, _) = self.window_display_state(window_id)?;
        if frame.effective_window_system().is_none() {
            return Some((0, 0, false, false));
        }
        Some((
            i64::from(if display.left_fringe_width >= 0 {
                display.left_fringe_width
            } else {
                frame_default_left_fringe_width(frame)
            }),
            i64::from(if display.right_fringe_width >= 0 {
                display.right_fringe_width
            } else {
                frame_default_right_fringe_width(frame)
            }),
            display.fringes_outside_margins,
            display.fringes_persistent,
        ))
    }

    /// Set raw window fringe settings. Returns true when the visible fringe state changed.
    pub fn set_window_fringes(
        &mut self,
        window_id: WindowId,
        left_width: Option<i32>,
        right_width: Option<i32>,
        outside_margins: bool,
        persistent: bool,
    ) -> bool {
        let previous = self.window_fringes(window_id);
        let Some((frame_id, _)) = self.window_display_location(window_id) else {
            return false;
        };
        if self
            .get(frame_id)
            .is_none_or(|frame| frame.effective_window_system().is_none())
        {
            return false;
        }
        if let Some(display) = self
            .get_mut(frame_id)
            .and_then(|frame| frame.find_window_mut(window_id))
            .and_then(Window::display_mut)
        {
            display.left_fringe_width = left_width.unwrap_or(-1);
            display.right_fringe_width = right_width.unwrap_or(-1);
            display.fringes_outside_margins = outside_margins;
            display.fringes_persistent = persistent;
        }
        self.window_fringes(window_id) != previous
    }

    /// Return raw window scroll-bar settings plus effective area metrics.
    pub fn window_scroll_bars(
        &self,
        window_id: WindowId,
    ) -> Option<(Value, i64, Value, Value, i64, Value, bool)> {
        let (frame, display, is_minibuffer) = self.window_display_state(window_id)?;
        Some((
            if display.scroll_bar_width >= 0 {
                Value::fixnum(i64::from(display.scroll_bar_width))
            } else {
                Value::NIL
            },
            vertical_scroll_bar_cols(frame, display),
            display.vertical_scroll_bar_type,
            if display.scroll_bar_height >= 0 {
                Value::fixnum(i64::from(display.scroll_bar_height))
            } else {
                Value::NIL
            },
            horizontal_scroll_bar_lines(frame, display, is_minibuffer),
            display.horizontal_scroll_bar_type,
            display.scroll_bars_persistent,
        ))
    }

    /// Return the effective vertical scroll-bar area width in pixels.
    pub fn window_scroll_bar_area_width(&self, window_id: WindowId) -> i64 {
        self.window_display_state(window_id)
            .map(|(frame, display, _)| vertical_scroll_bar_area_width(frame, display))
            .unwrap_or(0)
    }

    /// Return the effective left vertical scroll-bar area width in pixels.
    pub fn window_left_scroll_bar_area_width(&self, window_id: WindowId) -> i64 {
        self.window_display_state(window_id)
            .map(|(frame, display, _)| left_scroll_bar_area_width(frame, display))
            .unwrap_or(0)
    }

    /// Return the effective right vertical scroll-bar area width in pixels.
    pub fn window_right_scroll_bar_area_width(&self, window_id: WindowId) -> i64 {
        self.window_display_state(window_id)
            .map(|(frame, display, _)| right_scroll_bar_area_width(frame, display))
            .unwrap_or(0)
    }

    /// Return the effective horizontal scroll-bar area height in pixels.
    pub fn window_scroll_bar_area_height(&self, window_id: WindowId) -> i64 {
        self.window_display_state(window_id)
            .map(|(frame, display, is_minibuffer)| {
                horizontal_scroll_bar_area_height(frame, display, is_minibuffer)
            })
            .unwrap_or(0)
    }

    /// Set raw window scroll-bar settings. Returns true when the visible scroll-bar state changed.
    pub fn set_window_scroll_bars(
        &mut self,
        window_id: WindowId,
        width: Option<i32>,
        vertical_type: Value,
        height: Option<i32>,
        horizontal_type: Value,
        persistent: bool,
    ) -> bool {
        let previous = self.window_scroll_bars(window_id);
        let Some((frame_id, is_minibuffer)) = self.window_display_location(window_id) else {
            return false;
        };
        if self
            .get(frame_id)
            .is_none_or(|frame| frame.effective_window_system().is_none())
        {
            return false;
        }
        let mut next_vertical_type = vertical_type;
        if width == Some(0) {
            next_vertical_type = Value::NIL;
        }
        let mut next_horizontal_type = horizontal_type;
        if height == Some(0)
            || (is_minibuffer && !matches!(symbol_name(&next_horizontal_type), Some("bottom")))
        {
            next_horizontal_type = Value::NIL;
        }
        if let Some(display) = self
            .get_mut(frame_id)
            .and_then(|frame| frame.find_window_mut(window_id))
            .and_then(Window::display_mut)
        {
            display.scroll_bar_width = width.unwrap_or(-1);
            display.vertical_scroll_bar_type = next_vertical_type;
            display.scroll_bar_height = height.unwrap_or(-1);
            display.horizontal_scroll_bar_type = next_horizontal_type;
            display.scroll_bars_persistent = persistent;
        }
        self.window_scroll_bars(window_id) != previous
    }

    /// Apply the window-owned state changes that GNU performs during
    /// `set_window_buffer`.
    pub fn apply_set_window_buffer_state(
        &mut self,
        window_id: WindowId,
        buffer_id: BufferId,
        window_start: usize,
        point: usize,
        preserve_display_state: bool,
        defaults: WindowBufferDisplayDefaults,
    ) {
        let Some(frame_id) = self.find_window_frame_id(window_id) else {
            return;
        };
        let (fringes_persistent, scroll_bars_persistent) = self
            .get(frame_id)
            .and_then(|frame| frame.find_window(window_id))
            .and_then(Window::display)
            .map(|display| (display.fringes_persistent, display.scroll_bars_persistent))
            .unwrap_or((false, false));

        if let Some(Window::Leaf {
            buffer_id: leaf_buffer_id,
            window_start: leaf_window_start,
            point: leaf_point,
            old_point,
            hscroll,
            vscroll,
            preserve_vscroll_p,
            margins,
            ..
        }) = self
            .get_mut(frame_id)
            .and_then(|frame| frame.find_window_mut(window_id))
        {
            *leaf_buffer_id = buffer_id;
            *leaf_window_start = window_start.max(1);
            *leaf_point = point.max(1);
            if !preserve_display_state {
                *old_point = point.max(1);
                *hscroll = 0;
                *vscroll = 0;
                *preserve_vscroll_p = false;
            }
            if let Some(next_margins) = defaults.margins {
                *margins = next_margins;
            }
        }

        if let Some((left_width, right_width, outside_margins)) = defaults.fringes
            && !fringes_persistent
        {
            let _ =
                self.set_window_fringes(window_id, left_width, right_width, outside_margins, false);
        }
        if let Some((width, vertical_type, height, horizontal_type)) = defaults.scroll_bars
            && !scroll_bars_persistent
        {
            let _ = self.set_window_scroll_bars(
                window_id,
                width,
                vertical_type,
                height,
                horizontal_type,
                false,
            );
        }
    }
}
