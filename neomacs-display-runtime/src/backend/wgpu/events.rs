//! Event types for winit → Emacs communication.

/// Input event kinds matching Emacs event types.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    KeyPress = 1,
    KeyRelease = 2,
    MousePress = 3,
    MouseRelease = 4,
    MouseMove = 5,
    Scroll = 6,
    Resize = 7,
    CloseRequest = 8,
    FocusIn = 9,
    FocusOut = 10,
    ImageDimensionsReady = 11,
    TerminalExited = 12,
    MenuSelection = 13,
    FileDrop = 14,
    TerminalTitleChanged = 15,
    ToolBarClick = 16,
    MenuBarClick = 17,
}

/// Modifier flags matching Emacs.
pub const NEOMACS_SHIFT_MASK: u32 = 1 << 0;
pub const NEOMACS_CTRL_MASK: u32 = 1 << 1;
pub const NEOMACS_META_MASK: u32 = 1 << 2;
pub const NEOMACS_SUPER_MASK: u32 = 1 << 3;

/// Event kind constants for FFI.
pub const NEOMACS_EVENT_KEY_PRESS: u32 = EventKind::KeyPress as u32;
pub const NEOMACS_EVENT_KEY_RELEASE: u32 = EventKind::KeyRelease as u32;
pub const NEOMACS_EVENT_BUTTON_PRESS: u32 = EventKind::MousePress as u32;
pub const NEOMACS_EVENT_BUTTON_RELEASE: u32 = EventKind::MouseRelease as u32;
pub const NEOMACS_EVENT_MOUSE_MOVE: u32 = EventKind::MouseMove as u32;
pub const NEOMACS_EVENT_SCROLL: u32 = EventKind::Scroll as u32;
pub const NEOMACS_EVENT_RESIZE: u32 = EventKind::Resize as u32;
pub const NEOMACS_EVENT_CLOSE: u32 = EventKind::CloseRequest as u32;
pub const NEOMACS_EVENT_FOCUS_IN: u32 = EventKind::FocusIn as u32;
pub const NEOMACS_EVENT_FOCUS_OUT: u32 = EventKind::FocusOut as u32;
pub const NEOMACS_EVENT_IMAGE_DIMENSIONS_READY: u32 = EventKind::ImageDimensionsReady as u32;
pub const NEOMACS_EVENT_TERMINAL_EXITED: u32 = EventKind::TerminalExited as u32;
pub const NEOMACS_EVENT_MENU_SELECTION: u32 = EventKind::MenuSelection as u32;
pub const NEOMACS_EVENT_FILE_DROP: u32 = EventKind::FileDrop as u32;
pub const NEOMACS_EVENT_TERMINAL_TITLE_CHANGED: u32 = EventKind::TerminalTitleChanged as u32;
pub const NEOMACS_EVENT_TOOL_BAR_CLICK: u32 = EventKind::ToolBarClick as u32;
pub const NEOMACS_EVENT_MENU_BAR_CLICK: u32 = EventKind::MenuBarClick as u32;

/// Input event structure passed to C.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct NeomacsInputEvent {
    pub kind: u32,
    pub window_id: u32,
    pub timestamp: u64,
    pub x: i32,
    pub y: i32,
    pub keycode: u32,
    pub keysym: u32,
    pub modifiers: u32,
    pub button: u32,
    pub scroll_delta_x: f32,
    pub scroll_delta_y: f32,
    pub pixel_precise: u32,
    pub width: u32,
    pub height: u32,
    /// Target frame pointer for child frame mouse event routing (0 = parent frame)
    pub target_frame_id: u64,
    /// WebKit view ID hit by render-thread glyph search (0 = none)
    pub webkit_id: u32,
    /// Coordinates relative to the WebKit view (valid when webkit_id != 0)
    pub webkit_rel_x: i32,
    pub webkit_rel_y: i32,
}

impl Default for NeomacsInputEvent {
    fn default() -> Self {
        Self {
            kind: 0,
            window_id: 0,
            timestamp: 0,
            x: 0,
            y: 0,
            keycode: 0,
            keysym: 0,
            modifiers: 0,
            button: 0,
            scroll_delta_x: 0.0,
            scroll_delta_y: 0.0,
            pixel_precise: 0,
            width: 0,
            height: 0,
            target_frame_id: 0,
            webkit_id: 0,
            webkit_rel_x: 0,
            webkit_rel_y: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- EventKind discriminant values ----

    #[test]
    fn event_kind_discriminants() {
        assert_eq!(EventKind::KeyPress as u32, 1);
        assert_eq!(EventKind::KeyRelease as u32, 2);
        assert_eq!(EventKind::MousePress as u32, 3);
        assert_eq!(EventKind::MouseRelease as u32, 4);
        assert_eq!(EventKind::MouseMove as u32, 5);
        assert_eq!(EventKind::Scroll as u32, 6);
        assert_eq!(EventKind::Resize as u32, 7);
        assert_eq!(EventKind::CloseRequest as u32, 8);
        assert_eq!(EventKind::FocusIn as u32, 9);
        assert_eq!(EventKind::FocusOut as u32, 10);
        assert_eq!(EventKind::ImageDimensionsReady as u32, 11);
        assert_eq!(EventKind::TerminalExited as u32, 12);
        assert_eq!(EventKind::MenuSelection as u32, 13);
        assert_eq!(EventKind::FileDrop as u32, 14);
        assert_eq!(EventKind::TerminalTitleChanged as u32, 15);
    }

    // ---- FFI event kind constants match enum ----

    #[test]
    fn ffi_event_constants_match_enum() {
        assert_eq!(NEOMACS_EVENT_KEY_PRESS, EventKind::KeyPress as u32);
        assert_eq!(NEOMACS_EVENT_KEY_RELEASE, EventKind::KeyRelease as u32);
        assert_eq!(NEOMACS_EVENT_BUTTON_PRESS, EventKind::MousePress as u32);
        assert_eq!(NEOMACS_EVENT_BUTTON_RELEASE, EventKind::MouseRelease as u32);
        assert_eq!(NEOMACS_EVENT_MOUSE_MOVE, EventKind::MouseMove as u32);
        assert_eq!(NEOMACS_EVENT_SCROLL, EventKind::Scroll as u32);
        assert_eq!(NEOMACS_EVENT_RESIZE, EventKind::Resize as u32);
        assert_eq!(NEOMACS_EVENT_CLOSE, EventKind::CloseRequest as u32);
        assert_eq!(NEOMACS_EVENT_FOCUS_IN, EventKind::FocusIn as u32);
        assert_eq!(NEOMACS_EVENT_FOCUS_OUT, EventKind::FocusOut as u32);
        assert_eq!(
            NEOMACS_EVENT_IMAGE_DIMENSIONS_READY,
            EventKind::ImageDimensionsReady as u32
        );
        assert_eq!(
            NEOMACS_EVENT_TERMINAL_EXITED,
            EventKind::TerminalExited as u32
        );
        assert_eq!(
            NEOMACS_EVENT_MENU_SELECTION,
            EventKind::MenuSelection as u32
        );
        assert_eq!(NEOMACS_EVENT_FILE_DROP, EventKind::FileDrop as u32);
        assert_eq!(
            NEOMACS_EVENT_TERMINAL_TITLE_CHANGED,
            EventKind::TerminalTitleChanged as u32
        );
    }

    // ---- Modifier mask constants ----

    #[test]
    fn modifier_mask_values() {
        assert_eq!(NEOMACS_SHIFT_MASK, 1);
        assert_eq!(NEOMACS_CTRL_MASK, 2);
        assert_eq!(NEOMACS_META_MASK, 4);
        assert_eq!(NEOMACS_SUPER_MASK, 8);
    }

    #[test]
    fn modifier_masks_are_distinct_bits() {
        // Each mask should be a single distinct bit (no overlap).
        let masks = [
            NEOMACS_SHIFT_MASK,
            NEOMACS_CTRL_MASK,
            NEOMACS_META_MASK,
            NEOMACS_SUPER_MASK,
        ];
        for i in 0..masks.len() {
            assert!(
                masks[i].is_power_of_two(),
                "mask {} is not a power of two",
                masks[i]
            );
            for j in (i + 1)..masks.len() {
                assert_eq!(
                    masks[i] & masks[j],
                    0,
                    "masks {} and {} overlap",
                    masks[i],
                    masks[j]
                );
            }
        }
    }

    #[test]
    fn modifier_masks_can_be_combined() {
        let ctrl_meta = NEOMACS_CTRL_MASK | NEOMACS_META_MASK;
        assert_eq!(ctrl_meta, 6);
        assert_ne!(ctrl_meta & NEOMACS_CTRL_MASK, 0);
        assert_ne!(ctrl_meta & NEOMACS_META_MASK, 0);
        assert_eq!(ctrl_meta & NEOMACS_SHIFT_MASK, 0);
        assert_eq!(ctrl_meta & NEOMACS_SUPER_MASK, 0);
    }

    // ---- NeomacsInputEvent default ----

    #[test]
    fn input_event_default_all_zeroed() {
        let evt = NeomacsInputEvent::default();
        assert_eq!(evt.kind, 0);
        assert_eq!(evt.window_id, 0);
        assert_eq!(evt.timestamp, 0);
        assert_eq!(evt.x, 0);
        assert_eq!(evt.y, 0);
        assert_eq!(evt.keycode, 0);
        assert_eq!(evt.keysym, 0);
        assert_eq!(evt.modifiers, 0);
        assert_eq!(evt.button, 0);
        assert_eq!(evt.scroll_delta_x, 0.0);
        assert_eq!(evt.scroll_delta_y, 0.0);
        assert_eq!(evt.pixel_precise, 0);
        assert_eq!(evt.width, 0);
        assert_eq!(evt.height, 0);
        assert_eq!(evt.target_frame_id, 0);
    }

    // ---- NeomacsInputEvent field mutation ----

    #[test]
    fn input_event_field_mutation() {
        let mut evt = NeomacsInputEvent::default();
        evt.kind = NEOMACS_EVENT_KEY_PRESS;
        evt.window_id = 42;
        evt.timestamp = 123456789;
        evt.x = -100;
        evt.y = 200;
        evt.keycode = 65; // 'A'
        evt.keysym = 0x61; // 'a'
        evt.modifiers = NEOMACS_CTRL_MASK | NEOMACS_SHIFT_MASK;
        evt.button = 1;
        evt.scroll_delta_x = 1.5;
        evt.scroll_delta_y = -2.5;
        evt.pixel_precise = 1;
        evt.width = 1920;
        evt.height = 1080;
        evt.target_frame_id = 0xDEAD_BEEF;

        assert_eq!(evt.kind, 1);
        assert_eq!(evt.window_id, 42);
        assert_eq!(evt.timestamp, 123456789);
        assert_eq!(evt.x, -100);
        assert_eq!(evt.y, 200);
        assert_eq!(evt.keycode, 65);
        assert_eq!(evt.keysym, 0x61);
        assert_eq!(evt.modifiers, 3); // SHIFT | CTRL = 1 | 2 = 3
        assert_eq!(evt.button, 1);
        assert_eq!(evt.scroll_delta_x, 1.5);
        assert_eq!(evt.scroll_delta_y, -2.5);
        assert_eq!(evt.pixel_precise, 1);
        assert_eq!(evt.width, 1920);
        assert_eq!(evt.height, 1080);
        assert_eq!(evt.target_frame_id, 0xDEAD_BEEF);
    }

    // ---- EventKind traits ----

    #[test]
    fn event_kind_clone_and_copy() {
        let a = EventKind::Scroll;
        let b = a; // Copy
        let c = a.clone(); // Clone
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    #[test]
    fn event_kind_equality() {
        assert_eq!(EventKind::KeyPress, EventKind::KeyPress);
        assert_ne!(EventKind::KeyPress, EventKind::KeyRelease);
    }

    #[test]
    fn event_kind_debug_format() {
        let dbg = format!("{:?}", EventKind::CloseRequest);
        assert_eq!(dbg, "CloseRequest");
    }

    // ---- NeomacsInputEvent clone ----

    #[test]
    fn input_event_clone() {
        let mut evt = NeomacsInputEvent::default();
        evt.kind = NEOMACS_EVENT_SCROLL;
        evt.scroll_delta_y = -3.0;
        evt.modifiers = NEOMACS_META_MASK;

        let cloned = evt.clone();
        assert_eq!(cloned.kind, evt.kind);
        assert_eq!(cloned.scroll_delta_y, evt.scroll_delta_y);
        assert_eq!(cloned.modifiers, evt.modifiers);
    }

    // ---- FFI layout: repr(C) struct size sanity ----

    #[test]
    fn input_event_struct_size_is_reasonable() {
        // The struct has 18 fields of various sizes. Ensure it's at least
        // as large as the sum of field sizes and not absurdly large.
        let size = std::mem::size_of::<NeomacsInputEvent>();
        // Minimum: 4+4+8+4+4+4+4+4+4+4+4+4+4+4+8+4+4+4 = 80 bytes
        assert!(size >= 80, "struct too small: {}", size);
        // Should not exceed a generous upper bound (padding included)
        assert!(size <= 136, "struct unexpectedly large: {}", size);
    }

    #[test]
    fn event_kind_repr_u32_size() {
        assert_eq!(std::mem::size_of::<EventKind>(), std::mem::size_of::<u32>());
    }
}
