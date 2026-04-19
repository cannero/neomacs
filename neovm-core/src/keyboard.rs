//! Keyboard input and command loop.
//!
//! Implements the Emacs command loop:
//! - Key event representation
//! - Key sequence reading
//! - Command dispatch (keymap lookup → funcall)
//! - Interactive command argument parsing
//! - Minibuffer input
//! - Recursive edit support
//! - Pre/post-command hooks
//! - Prefix argument handling

use crate::emacs_core::intern::resolve_sym;
use crate::emacs_core::keyboard::pure::KEY_CHAR_META;
// decode_storage_char_codes import removed — now using emacs_char directly
use crate::emacs_core::value::{Value, ValueKind, VecLikeType};
use crate::heap_types::LispString;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Key events
// ---------------------------------------------------------------------------

/// Modifier flags for key events.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub ctrl: bool,
    pub meta: bool, // Alt
    pub shift: bool,
    pub super_: bool,
    pub hyper: bool,
}

impl Modifiers {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn ctrl() -> Self {
        Self {
            ctrl: true,
            ..Self::default()
        }
    }

    pub fn meta() -> Self {
        Self {
            meta: true,
            ..Self::default()
        }
    }

    pub fn ctrl_meta() -> Self {
        Self {
            ctrl: true,
            meta: true,
            ..Self::default()
        }
    }

    /// Convert to Emacs modifier bitmask.
    pub fn to_bits(&self) -> u32 {
        let mut bits = 0u32;
        if self.ctrl {
            bits |= 1 << 26;
        }
        if self.meta {
            bits |= 1 << 27;
        }
        if self.shift {
            bits |= 1 << 25;
        }
        if self.super_ {
            bits |= 1 << 23;
        }
        if self.hyper {
            bits |= 1 << 24;
        }
        bits
    }

    /// Parse from Emacs modifier bitmask.
    pub fn from_bits(bits: u32) -> Self {
        Self {
            ctrl: bits & (1 << 26) != 0,
            meta: bits & (1 << 27) != 0,
            shift: bits & (1 << 25) != 0,
            super_: bits & (1 << 23) != 0,
            hyper: bits & (1 << 24) != 0,
        }
    }

    /// Format as Emacs modifier prefix (e.g., "C-M-").
    pub fn prefix_string(&self) -> String {
        let mut s = String::new();
        if self.hyper {
            s.push_str("H-");
        }
        if self.super_ {
            s.push_str("s-");
        }
        if self.ctrl {
            s.push_str("C-");
        }
        if self.meta {
            s.push_str("M-");
        }
        if self.shift {
            s.push_str("S-");
        }
        s
    }

    pub fn is_empty(&self) -> bool {
        !self.ctrl && !self.meta && !self.shift && !self.super_ && !self.hyper
    }
}

/// A single key event (keystroke).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    /// The base key (character or named key).
    pub key: Key,
    /// Active modifiers.
    pub modifiers: Modifiers,
}

/// The base key of a keystroke.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// A character key (e.g., 'a', '1', space).
    Char(char),
    /// A named function key.
    Named(NamedKey),
}

/// Named (non-character) keys.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NamedKey {
    Return,
    Tab,
    Escape,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Left,
    Right,
    Up,
    Down,
    F(u8), // F1-F24
}

impl KeyEvent {
    pub fn char(c: char) -> Self {
        Self {
            key: Key::Char(c),
            modifiers: Modifiers::none(),
        }
    }

    pub fn char_with_mods(c: char, mods: Modifiers) -> Self {
        Self {
            key: Key::Char(c),
            modifiers: mods,
        }
    }

    pub fn named(key: NamedKey) -> Self {
        Self {
            key: Key::Named(key),
            modifiers: Modifiers::none(),
        }
    }

    pub fn named_with_mods(key: NamedKey, mods: Modifiers) -> Self {
        Self {
            key: Key::Named(key),
            modifiers: mods,
        }
    }

    /// Convert this host key event into the Lisp-visible Emacs event
    /// representation used by the command loop and keymap lookup.
    pub fn to_emacs_event_value(&self) -> Value {
        let event = crate::emacs_core::keymap::KeyEvent::from(self.clone());
        crate::emacs_core::keymap::key_event_to_emacs_event(&event)
    }

    /// True if this event is GNU Emacs's default `quit-char`: `C-g`.
    ///
    /// Used by the input-bridge thread to set `Context::quit_requested`
    /// without a round-trip through the evaluator. The evaluator's own
    /// `event_is_quit_char` is still consulted in `read_char` to honor
    /// customized `quit-char` values; this helper only catches the
    /// overwhelmingly common default so a blocked bytecode loop can be
    /// interrupted.
    pub fn is_default_quit_char(&self) -> bool {
        if !matches!(self.key, Key::Char('g')) {
            return false;
        }
        let m = self.modifiers;
        m.ctrl && !m.meta && !m.super_ && !m.hyper
    }

    /// Format as Emacs key description (e.g., "C-x", "M-f", "RET").
    pub fn to_description(&self) -> String {
        let emacs_event = self.to_emacs_event_value();
        crate::emacs_core::keyboard::pure::describe_single_key_value(&emacs_event, false)
            .unwrap_or_else(|_| format!("{:?}", emacs_event))
    }

    /// Parse an Emacs key description (e.g., "C-x", "M-f").
    pub fn from_description(desc: &str) -> Option<Self> {
        let encoded = crate::emacs_core::kbd::parse_kbd_string(desc).ok()?;
        let events = crate::emacs_core::kbd::key_events_from_designator(&encoded).ok()?;
        let [event] = events.as_slice() else {
            return None;
        };
        Self::from_emacs_key_event(event.clone())
    }

    fn from_emacs_key_event(event: crate::emacs_core::keymap::KeyEvent) -> Option<Self> {
        match event {
            crate::emacs_core::keymap::KeyEvent::Char {
                code,
                ctrl,
                meta,
                shift,
                super_,
                hyper,
                alt,
            } => {
                if alt {
                    return None;
                }
                // GNU's `kbd` parser collapses `C-x` into raw control
                // codepoint U+0018 with `ctrl=false` (mirroring elisp
                // `?\C-x => 24`). Reverse that here so terminal-level
                // KeyEvents still carry a `ctrl` modifier for ASCII
                // letters — otherwise `(C-x).key` reads as char 0x18
                // and rendering / matching breaks.
                let (code, ctrl) = if !ctrl
                    && (code as u32) < 0x20
                    && code != '\r'
                    && code != '\t'
                    && code != '\u{1b}'
                    && code != '\u{7f}'
                {
                    let lowered = ((code as u8) | 0x60) as char;
                    if lowered.is_ascii_alphabetic() {
                        (lowered, true)
                    } else {
                        (code, false)
                    }
                } else {
                    (code, ctrl)
                };
                let key = match code {
                    '\r' => Key::Named(NamedKey::Return),
                    '\t' => Key::Named(NamedKey::Tab),
                    '\u{1b}' => Key::Named(NamedKey::Escape),
                    '\u{7f}' => Key::Named(NamedKey::Backspace),
                    other => Key::Char(other),
                };
                Some(KeyEvent {
                    key,
                    modifiers: Modifiers {
                        ctrl,
                        meta,
                        shift,
                        super_,
                        hyper,
                    },
                })
            }
            crate::emacs_core::keymap::KeyEvent::Function {
                name,
                ctrl,
                meta,
                shift,
                super_,
                hyper,
                alt,
            } => {
                if alt {
                    return None;
                }
                let key = match resolve_sym(name) {
                    "return" => Key::Named(NamedKey::Return),
                    "tab" => Key::Named(NamedKey::Tab),
                    "escape" => Key::Named(NamedKey::Escape),
                    "backspace" => Key::Named(NamedKey::Backspace),
                    "delete" => Key::Named(NamedKey::Delete),
                    "insert" => Key::Named(NamedKey::Insert),
                    "home" => Key::Named(NamedKey::Home),
                    "end" => Key::Named(NamedKey::End),
                    "prior" => Key::Named(NamedKey::PageUp),
                    "next" => Key::Named(NamedKey::PageDown),
                    "left" => Key::Named(NamedKey::Left),
                    "right" => Key::Named(NamedKey::Right),
                    "up" => Key::Named(NamedKey::Up),
                    "down" => Key::Named(NamedKey::Down),
                    other if other.starts_with('f') => {
                        let num = other.strip_prefix('f')?.parse::<u8>().ok()?;
                        Key::Named(NamedKey::F(num))
                    }
                    _ => return None,
                };
                Some(KeyEvent {
                    key,
                    modifiers: Modifiers {
                        ctrl,
                        meta,
                        shift,
                        super_,
                        hyper,
                    },
                })
            }
        }
    }

    /// Convert to Emacs integer event representation.
    pub fn to_event_int(&self) -> u32 {
        let base = match &self.key {
            Key::Char(c) => *c as u32,
            Key::Named(n) => match n {
                NamedKey::Return => 13,
                NamedKey::Tab => 9,
                NamedKey::Escape => 27,
                NamedKey::Backspace => 127,
                _ => 0,
            },
        };
        base | self.modifiers.to_bits()
    }
}

// ---------------------------------------------------------------------------
// Key sequence
// ---------------------------------------------------------------------------

/// A sequence of key events forming a complete key binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeySequence {
    pub events: Vec<KeyEvent>,
}

impl KeySequence {
    pub fn new() -> Self {
        Self { events: Vec::new() }
    }

    pub fn single(event: KeyEvent) -> Self {
        Self {
            events: vec![event],
        }
    }

    pub fn push(&mut self, event: KeyEvent) {
        self.events.push(event);
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Format as Emacs key sequence description.
    pub fn to_description(&self) -> String {
        self.events
            .iter()
            .map(|e| e.to_description())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Parse an Emacs key sequence description (e.g., "C-x C-f").
    pub fn from_description(desc: &str) -> Option<Self> {
        let encoded = crate::emacs_core::kbd::parse_kbd_string(desc).ok()?;
        let emacs_events = crate::emacs_core::kbd::key_events_from_designator(&encoded).ok()?;
        let events = emacs_events
            .into_iter()
            .map(KeyEvent::from_emacs_key_event)
            .collect::<Option<Vec<_>>>()?;
        Some(Self { events })
    }
}

impl Default for KeySequence {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReadKeySequenceState {
    raw_events: Vec<Value>,
    translated_events: Vec<Value>,
}

impl ReadKeySequenceState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.raw_events.clear();
        self.translated_events.clear();
    }

    pub fn push_input_event(&mut self, event: Value) {
        self.raw_events.push(event);
        self.translated_events.push(event);
    }

    pub fn replace_translated_events(&mut self, events: Vec<Value>) {
        self.translated_events = events;
    }

    pub fn raw_events(&self) -> &[Value] {
        &self.raw_events
    }

    pub fn translated_events(&self) -> &[Value] {
        &self.translated_events
    }

    pub fn snapshot(&self) -> (Vec<Value>, Vec<Value>) {
        (self.translated_events.clone(), self.raw_events.clone())
    }

    /// Remove the last raw and translated event. Used by the
    /// help-char dispatch path in `read_key_sequence` to strip
    /// the help event from the sequence before running
    /// `prefix-help-command`, matching GNU
    /// `keyboard.c:10220-10230` which discards the help event so
    /// `(this-command-keys)` reports the prefix only.
    pub fn pop_last_events_for_help_char(&mut self) {
        self.raw_events.pop();
        self.translated_events.pop();
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ReadKeySequenceOptions {
    pub prompt: Value,
    pub dont_downcase_last: bool,
    pub can_return_switch_frame: bool,
}

impl ReadKeySequenceOptions {
    pub(crate) fn new(
        prompt: Value,
        dont_downcase_last: bool,
        can_return_switch_frame: bool,
    ) -> Self {
        Self {
            prompt,
            dont_downcase_last,
            can_return_switch_frame,
        }
    }
}

impl Default for ReadKeySequenceOptions {
    fn default() -> Self {
        Self {
            prompt: Value::NIL,
            dont_downcase_last: false,
            can_return_switch_frame: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct KeySequenceSuffixTranslation {
    start: usize,
    replacement: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq)]
struct CurrentKeySequenceTranslation {
    translated_events: Vec<Value>,
    has_pending_translation_prefix: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct KeySequenceShiftTranslation {
    index: usize,
    original_event: Value,
}

#[derive(Clone, Debug)]
enum UndefinedMouseSequenceFallback {
    Rewrite {
        events: Vec<Value>,
        resolved: crate::emacs_core::keymap::ActiveKeyBindingResolution,
    },
    Drop {
        retained_events: Vec<Value>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MouseEventFallbackStep {
    Rewrite,
    Drop,
}

// ---------------------------------------------------------------------------
// Keysym conversion (X11/winit keysyms → neovm-core KeyEvent)
// ---------------------------------------------------------------------------

// X11 keysym constants used by the render thread (winit) and TTY frontend.
pub const XK_RETURN: u32 = 0xFF0D;
pub const XK_TAB: u32 = 0xFF09;
pub const XK_BACKSPACE: u32 = 0xFF08;
pub const XK_DELETE: u32 = 0xFFFF;
pub const XK_ESCAPE: u32 = 0xFF1B;
pub const XK_LEFT: u32 = 0xFF51;
pub const XK_UP: u32 = 0xFF52;
pub const XK_RIGHT: u32 = 0xFF53;
pub const XK_DOWN: u32 = 0xFF54;
pub const XK_HOME: u32 = 0xFF50;
pub const XK_END: u32 = 0xFF57;
pub const XK_PAGE_UP: u32 = 0xFF55;
pub const XK_PAGE_DOWN: u32 = 0xFF56;
pub const XK_INSERT: u32 = 0xFF63;
pub const XK_F1: u32 = 0xFFBE;
pub const XK_F24: u32 = 0xFFD5;

// Render thread modifier bitmask constants.
pub const RENDER_SHIFT_MASK: u32 = 1 << 0;
pub const RENDER_CTRL_MASK: u32 = 1 << 1;
pub const RENDER_META_MASK: u32 = 1 << 2;
pub const RENDER_SUPER_MASK: u32 = 1 << 3;

/// Convert frontend render/TTY modifier bits into the core modifier model.
pub fn render_modifiers_to_modifiers(bits: u32) -> Modifiers {
    Modifiers {
        ctrl: bits & RENDER_CTRL_MASK != 0,
        meta: bits & RENDER_META_MASK != 0,
        shift: bits & RENDER_SHIFT_MASK != 0,
        super_: bits & RENDER_SUPER_MASK != 0,
        hyper: false,
    }
}

/// Convert frontend key transport facts into the core input event model.
///
/// Key releases are ignored here so the command loop only sees the GNU-like
/// cooked keypress stream.
pub fn render_key_transport_to_input_event(
    keysym: u32,
    modifiers: u32,
    pressed: bool,
    emacs_frame_id: u64,
) -> Option<InputEvent> {
    if !pressed {
        return None;
    }

    let key_event = keysym_to_key_event(keysym, modifiers)?;
    Some(InputEvent::key_press_in_frame(key_event, emacs_frame_id))
}

/// Convert a raw keysym and modifier bitmask (from the render thread) into
/// a neovm-core `KeyEvent`.
///
/// Returns `None` for keysyms that should be ignored (modifier-only keys,
/// unknown keysyms, etc.).
pub fn keysym_to_key_event(keysym: u32, modifiers: u32) -> Option<KeyEvent> {
    let mods = render_modifiers_to_modifiers(modifiers);

    let key = match keysym {
        // Control characters (Ctrl + letter): winit gives us the control
        // character (0x01-0x1A) as the keysym when Ctrl is held.  Convert
        // back to the corresponding letter and force the ctrl modifier.
        0x01..=0x1A => {
            let ch = (keysym + 0x60) as u8 as char; // 0x18 → 'x'
            return Some(KeyEvent {
                key: Key::Char(ch),
                modifiers: Modifiers {
                    ctrl: true,
                    shift: false,
                    ..mods
                },
            });
        }
        // Printable ASCII
        0x20..=0x7E => Key::Char(keysym as u8 as char),
        // Named keys
        XK_RETURN => Key::Named(NamedKey::Return),
        XK_TAB => Key::Named(NamedKey::Tab),
        XK_BACKSPACE => Key::Named(NamedKey::Backspace),
        XK_DELETE => Key::Named(NamedKey::Delete),
        XK_ESCAPE => Key::Named(NamedKey::Escape),
        XK_LEFT => Key::Named(NamedKey::Left),
        XK_RIGHT => Key::Named(NamedKey::Right),
        XK_UP => Key::Named(NamedKey::Up),
        XK_DOWN => Key::Named(NamedKey::Down),
        XK_HOME => Key::Named(NamedKey::Home),
        XK_END => Key::Named(NamedKey::End),
        XK_PAGE_UP => Key::Named(NamedKey::PageUp),
        XK_PAGE_DOWN => Key::Named(NamedKey::PageDown),
        XK_INSERT => Key::Named(NamedKey::Insert),
        // Function keys F1-F24
        k if (XK_F1..=XK_F24).contains(&k) => Key::Named(NamedKey::F((k - XK_F1 + 1) as u8)),
        // Printable Unicode scalar values from TTY or GUI backends.
        k if char::from_u32(k).is_some_and(|ch| !ch.is_control()) => {
            Key::Char(char::from_u32(k).unwrap())
        }
        // Ignore modifier-only keys and unknown keysyms
        _ => return None,
    };

    let modifiers = match key {
        Key::Char(_) => Modifiers {
            shift: false,
            ..mods
        },
        Key::Named(_) => mods,
    };

    Some(KeyEvent { key, modifiers })
}

// ---------------------------------------------------------------------------
// Input event types
// ---------------------------------------------------------------------------

/// Input events from the display layer.
#[derive(Clone, Debug)]
pub enum InputEvent {
    /// Keyboard key press.
    KeyPress { key: KeyEvent, emacs_frame_id: u64 },
    /// Mouse button press.
    MousePress {
        button: MouseButton,
        x: f32,
        y: f32,
        modifiers: Modifiers,
        target_frame_id: u64,
    },
    /// Mouse button release.
    MouseRelease {
        button: MouseButton,
        x: f32,
        y: f32,
        target_frame_id: u64,
    },
    /// Mouse movement.
    MouseMove {
        x: f32,
        y: f32,
        modifiers: Modifiers,
        target_frame_id: u64,
    },
    /// Mouse scroll.
    MouseScroll {
        delta_x: f32,
        delta_y: f32,
        x: f32,
        y: f32,
        modifiers: Modifiers,
        target_frame_id: u64,
    },
    /// Window resize.
    Resize {
        width: u32,
        height: u32,
        emacs_frame_id: u64,
    },
    /// Window focus change.
    Focus { focused: bool, emacs_frame_id: u64 },
    /// Monitor configuration changed.
    MonitorsChanged {
        monitors: Vec<crate::emacs_core::builtins::NeomacsMonitorInfo>,
    },
    /// Window-selection change.
    SelectWindow { window_id: crate::window::WindowId },
    /// Window-manager close request.
    WindowClose { emacs_frame_id: u64 },
}

impl InputEvent {
    pub fn key_press(key: KeyEvent) -> Self {
        Self::KeyPress {
            key,
            emacs_frame_id: 0,
        }
    }

    pub fn key_press_in_frame(key: KeyEvent, emacs_frame_id: u64) -> Self {
        Self::KeyPress {
            key,
            emacs_frame_id,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Button4,
    Button5,
}

// ---------------------------------------------------------------------------
// Prefix argument
// ---------------------------------------------------------------------------

/// The current prefix argument state.
#[derive(Clone, Debug, PartialEq)]
pub enum PrefixArg {
    /// No prefix argument.
    None,
    /// Numeric prefix (e.g., C-u 4, M-3).
    Numeric(i64),
    /// Raw prefix (C-u without number).
    Raw(i32), // number of C-u presses: 1 = (4), 2 = (16), etc.
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MousePixelPositionState {
    pub frame_id: Option<crate::window::FrameId>,
    pub x: i64,
    pub y: i64,
}

impl PrefixArg {
    /// Convert to Lisp value for `current-prefix-arg`.
    pub fn to_value(&self) -> Value {
        match self {
            PrefixArg::None => Value::NIL,
            PrefixArg::Numeric(n) => Value::fixnum(*n),
            PrefixArg::Raw(n) => {
                let val = 4i64.pow(*n as u32);
                Value::list(vec![Value::fixnum(val)])
            }
        }
    }

    /// Numeric value (for commands that use the prefix as a count).
    pub fn numeric_value(&self) -> i64 {
        match self {
            PrefixArg::None => 1,
            PrefixArg::Numeric(n) => *n,
            PrefixArg::Raw(n) => 4i64.pow(*n as u32),
        }
    }
}

// ---------------------------------------------------------------------------
// Command loop state
// ---------------------------------------------------------------------------

/// Keyboard-local state owned by the active terminal/keyboard.
///
/// GNU Emacs keeps unread events, command-key history, translation maps, and
/// keyboard-macro playback on `kboard` state. NeoVM still has one active
/// keyboard, but it now models that owner explicitly.
pub struct KBoard {
    /// Deferred switch-frame/select-window event that should be delivered
    /// before ordinary unread input, matching GNU
    /// `unread_switch_frame` plus read-key-sequence delayed selection events.
    pub unread_selection_event: Option<Value>,
    /// Last frame observed by `internal-handle-focus-in`, matching GNU
    /// `internal_last_event_frame`.
    pub internal_last_event_frame: Option<crate::window::FrameId>,
    /// Last known mouse position in frame pixel coordinates.
    pub mouse_pixel_position: Option<MousePixelPositionState>,
    /// Last queued internal `help-echo` event for deduping mouse-motion help.
    pub last_help_echo_event: Option<Value>,
    /// Unread command events in the Lisp-visible Emacs event form.
    pub unread_events: VecDeque<Value>,
    /// Current raw/translated key sequence being accumulated by `read_key_sequence`.
    pub current_key_sequence: ReadKeySequenceState,
    /// Last translated key sequence read by the command loop or `read-key*`.
    pub command_keys: Vec<Value>,
    /// Raw key sequence before translation maps, for GNU
    /// `this-single-command-raw-keys`.
    pub raw_command_keys: Vec<Value>,
    /// Recent input history published through `recent-keys`.
    pub recent_input_events: Vec<Value>,
    /// Terminal-local `input-decode-map`.
    input_decode_map: Value,
    /// Terminal-local `local-function-key-map`.
    local_function_key_map: Value,
    /// Defining keyboard macro (if any).
    pub defining_kbd_macro: bool,
    /// Whether the current definition is appending to the prior macro.
    pub appending_kbd_macro: bool,
    /// Keyboard macro buffer being defined, as Lisp-visible Emacs events.
    pub kbd_macro_events: Vec<Value>,
    /// Finalized prefix of `kbd_macro_events` that belongs to completed commands.
    pub kbd_macro_end: usize,
    /// The last completed keyboard macro, matching GNU `last-kbd-macro`.
    pub last_kbd_macro: Option<Vec<Value>>,
    /// Keyboard macro being executed, as Lisp-visible Emacs events.
    pub executing_kbd_macro: Option<Vec<Value>>,
    /// Index into executing keyboard macro.
    pub kbd_macro_index: usize,
    /// Number of successful iterations for the innermost executing macro.
    pub executing_kbd_macro_iterations: usize,
    /// Open dribble file handle. GNU
    /// `src/keyboard.c:64 (FILE *dribble)` is the global file
    /// handle that `open-dribble-file` opens and
    /// `record_input_event` writes to. Keyboard audit Finding 11.
    dribble: Option<std::fs::File>,
    /// Recursion guard for `input-method-function`. GNU
    /// `keyboard.c` uses the `immediate_echo` flag to suppress
    /// re-entry; we use a dedicated bool so an input-method that
    /// calls `read-event` recursively does not re-translate the
    /// character it was given. Keyboard audit Finding 10.
    pub in_input_method_function: bool,
    /// Last mouse down event seen by the event-ingest path. GNU
    /// `keyboard.c:6041-6130` (`make_lispy_event` / the
    /// `button_down_time` / `last_mouse_click` globals) tracks
    /// the previous click's button, position, frame, and
    /// timestamp so it can compute the click count for
    /// `double-click-time` / `double-click-fuzz` comparisons.
    /// Keyboard audit Finding 12.
    pub last_mouse_click: Option<LastMouseClick>,
}

/// Snapshot of the most recent mouse click used for double/triple
/// click detection. Mirrors the GNU `button_down_info` bundle.
/// Keyboard audit Finding 12.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LastMouseClick {
    pub button: MouseButton,
    pub x: f32,
    pub y: f32,
    pub frame_id: u64,
    pub timestamp: std::time::Instant,
    /// Sequential click count: 1 = single, 2 = double, 3 = triple.
    /// Reset to 1 whenever a click falls outside
    /// `double-click-time` / `double-click-fuzz` of the previous
    /// click. Capped at 3.
    pub click_count: u32,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ExecutingKbdMacroRuntimeSnapshot {
    pub events: Option<Vec<Value>>,
    pub index: usize,
}

impl KBoard {
    pub fn new() -> Self {
        Self {
            unread_selection_event: None,
            internal_last_event_frame: None,
            mouse_pixel_position: None,
            last_help_echo_event: None,
            unread_events: VecDeque::new(),
            current_key_sequence: ReadKeySequenceState::new(),
            command_keys: Vec::new(),
            raw_command_keys: Vec::new(),
            recent_input_events: Vec::new(),
            input_decode_map: Value::NIL,
            local_function_key_map: Value::NIL,
            defining_kbd_macro: false,
            appending_kbd_macro: false,
            kbd_macro_events: Vec::new(),
            kbd_macro_end: 0,
            last_kbd_macro: None,
            executing_kbd_macro: None,
            kbd_macro_index: 0,
            executing_kbd_macro_iterations: 0,
            dribble: None,
            in_input_method_function: false,
            last_mouse_click: None,
        }
    }

    /// Open the dribble file at PATH for input event logging.
    /// Closes any previously open file. Mirrors GNU
    /// `Fopen_dribble_file` (`src/keyboard.c:12327-12367`):
    ///
    ///   if (dribble) fclose (dribble);
    ///   dribble = fopen (file, "w");
    ///   if (! dribble) report_file_error ("Opening dribble", file);
    ///
    /// Keyboard audit Finding 11.
    pub fn open_dribble_file(&mut self, path: &str) -> std::io::Result<()> {
        self.close_dribble_file();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        self.dribble = Some(file);
        Ok(())
    }

    /// Close the dribble file. Mirrors GNU's
    /// `Fopen_dribble_file (Qnil)` path.
    pub fn close_dribble_file(&mut self) {
        if let Some(mut f) = self.dribble.take() {
            use std::io::Write;
            let _ = f.flush();
        }
    }

    /// Write an input event to the dribble file. Mirrors GNU
    /// `dribble_event` / the inline writes inside
    /// `kbd_buffer_get_event` (`src/keyboard.c:4053-4087`).
    /// A nil event is logged as `nil`; ASCII printable characters
    /// are written as themselves; other events are formatted via
    /// the standard event-to-string fallback. The dribble is
    /// flushed after every event so a crash leaves a complete
    /// record on disk.
    pub fn dribble_event(&mut self, event: Value) {
        let Some(file) = self.dribble.as_mut() else {
            return;
        };
        use std::io::Write;
        if let Some(ch) = event.as_fixnum() {
            if (32..127).contains(&ch) {
                let _ = write!(file, "{}", ch as u8 as char);
                let _ = file.flush();
                return;
            }
            let _ = write!(file, " 0x{:x}", ch);
            let _ = file.flush();
            return;
        }
        let _ = write!(file, " {}", event);
        let _ = file.flush();
    }

    pub fn set_terminal_translation_maps(
        &mut self,
        input_decode_map: Value,
        local_function_key_map: Value,
    ) {
        self.input_decode_map = input_decode_map;
        self.local_function_key_map = local_function_key_map;
    }

    pub fn set_input_decode_map(&mut self, map: Value) {
        self.input_decode_map = map;
    }

    pub fn input_decode_map(&self) -> Value {
        self.input_decode_map
    }

    pub fn set_local_function_key_map(&mut self, map: Value) {
        self.local_function_key_map = map;
    }

    pub fn local_function_key_map(&self) -> Value {
        self.local_function_key_map
    }

    pub fn unread_event(&mut self, event: Value) {
        self.unread_events.push_back(event);
    }

    pub fn set_unread_selection_event(&mut self, event: Value) {
        self.unread_selection_event = Some(event);
    }

    pub fn internal_last_event_frame(&self) -> Option<crate::window::FrameId> {
        self.internal_last_event_frame
    }

    pub fn set_internal_last_event_frame(&mut self, frame_id: crate::window::FrameId) {
        self.internal_last_event_frame = Some(frame_id);
    }

    pub fn mouse_pixel_position(&self) -> Option<MousePixelPositionState> {
        self.mouse_pixel_position
    }

    pub fn set_mouse_pixel_position(
        &mut self,
        frame_id: Option<crate::window::FrameId>,
        x: i64,
        y: i64,
    ) {
        self.mouse_pixel_position = Some(MousePixelPositionState { frame_id, x, y });
    }

    pub fn unread_key(&mut self, event: KeyEvent) {
        self.unread_event(event.to_emacs_event_value());
    }

    pub fn reset_key_sequence(&mut self) {
        self.current_key_sequence.reset();
    }

    pub fn push_key_sequence_input_event(&mut self, event: Value) {
        self.current_key_sequence.push_input_event(event);
    }

    pub fn rewrite_key_sequence_translation(&mut self, events: Vec<Value>) {
        self.current_key_sequence.replace_translated_events(events);
    }

    pub fn key_sequence_snapshot(&self) -> (Vec<Value>, Vec<Value>) {
        self.current_key_sequence.snapshot()
    }

    pub fn set_command_key_sequences(&mut self, translated: Vec<Value>, raw: Vec<Value>) {
        self.command_keys = translated;
        self.raw_command_keys = raw;
    }

    pub fn set_translated_command_keys(&mut self, keys: Vec<Value>) {
        self.command_keys = keys;
    }

    pub fn set_read_command_keys(&mut self, keys: Vec<Value>) {
        self.command_keys = keys.clone();
        self.raw_command_keys = keys;
    }

    pub fn clear_read_command_keys(&mut self) {
        self.command_keys.clear();
        self.raw_command_keys.clear();
    }

    pub fn read_command_keys(&self) -> &[Value] {
        &self.command_keys
    }

    pub fn read_raw_command_keys(&self) -> &[Value] {
        &self.raw_command_keys
    }

    pub fn record_input_event(&mut self, event: Value) {
        self.recent_input_events.push(event);
        if self.recent_input_events.len() > crate::emacs_core::eval::RECENT_INPUT_EVENT_LIMIT {
            self.recent_input_events.remove(0);
        }
        // GNU `kbd_buffer_get_event` writes every read event to
        // the dribble file (if open). Mirroring that here at the
        // canonical lossage-ring entry point captures every event
        // that flows through the keyboard module.
        self.dribble_event(event);
    }

    pub fn recent_input_events(&self) -> &[Value] {
        &self.recent_input_events
    }

    pub fn clear_recent_input_events(&mut self) {
        self.recent_input_events.clear();
    }

    pub fn start_kbd_macro(&mut self) {
        self.start_kbd_macro_with_initial(None, false);
    }

    pub fn start_kbd_macro_with_initial(&mut self, initial_events: Option<&[Value]>, append: bool) {
        self.defining_kbd_macro = true;
        self.appending_kbd_macro = append;
        self.kbd_macro_events.clear();
        if let Some(initial_events) = initial_events {
            self.kbd_macro_events.extend_from_slice(initial_events);
        }
        self.kbd_macro_end = self.kbd_macro_events.len();
    }

    pub fn store_kbd_macro_event(&mut self, event: Value) {
        if self.defining_kbd_macro {
            self.kbd_macro_events.push(event);
        }
    }

    pub fn finalize_kbd_macro_chars(&mut self) {
        self.kbd_macro_end = self.kbd_macro_events.len();
    }

    pub fn cancel_kbd_macro_events(&mut self) {
        self.kbd_macro_events.truncate(self.kbd_macro_end);
    }

    pub fn end_kbd_macro(&mut self) -> Vec<Value> {
        self.defining_kbd_macro = false;
        self.appending_kbd_macro = false;
        let finalized = self.kbd_macro_events[..self.kbd_macro_end].to_vec();
        self.last_kbd_macro = Some(finalized.clone());
        finalized
    }

    pub fn last_kbd_macro(&self) -> Option<&[Value]> {
        self.last_kbd_macro.as_deref()
    }

    pub fn begin_executing_kbd_macro(&mut self, events: Vec<Value>) {
        self.executing_kbd_macro = Some(events);
        self.kbd_macro_index = 0;
        self.executing_kbd_macro_iterations = 0;
    }

    pub fn finish_executing_kbd_macro(&mut self) {
        self.executing_kbd_macro = None;
        self.kbd_macro_index = 0;
    }

    pub(crate) fn snapshot_executing_kbd_macro_runtime(&self) -> ExecutingKbdMacroRuntimeSnapshot {
        ExecutingKbdMacroRuntimeSnapshot {
            events: self.executing_kbd_macro.clone(),
            index: self.kbd_macro_index,
        }
    }

    pub(crate) fn restore_executing_kbd_macro_runtime(
        &mut self,
        snapshot: ExecutingKbdMacroRuntimeSnapshot,
    ) {
        self.executing_kbd_macro = snapshot.events;
        self.kbd_macro_index = snapshot.index;
    }

    pub(crate) fn set_executing_kbd_macro_index(&mut self, index: usize) {
        self.kbd_macro_index = index;
    }

    pub(crate) fn note_executing_kbd_macro_iteration(&mut self, success_count: usize) {
        self.executing_kbd_macro_iterations = success_count;
    }
}

impl Default for KBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::gc_trace::GcTrace for KBoard {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        if let Some(event) = self.unread_selection_event {
            roots.push(event);
        }
        if let Some(event) = self.last_help_echo_event {
            roots.push(event);
        }
        roots.extend(self.unread_events.iter().copied());
        roots.extend(self.current_key_sequence.raw_events().iter().copied());
        roots.extend(
            self.current_key_sequence
                .translated_events()
                .iter()
                .copied(),
        );
        roots.extend(self.command_keys.iter().copied());
        roots.extend(self.raw_command_keys.iter().copied());
        roots.extend(self.recent_input_events.iter().copied());
        roots.push(self.input_decode_map);
        roots.push(self.local_function_key_map);
        roots.extend(self.kbd_macro_events.iter().copied());
        if let Some(events) = &self.last_kbd_macro {
            roots.extend(events.iter().copied());
        }
        if let Some(events) = &self.executing_kbd_macro {
            roots.extend(events.iter().copied());
        }
    }
}

/// Keyboard runtime state shared by the command loop.
///
/// This owns transport-facing queues plus the active keyboard-local `KBoard`
/// state, which is the nearest NeoVM equivalent to GNU `keyboard.c` +
/// `kboard`.
pub struct KeyboardRuntime {
    /// Input event queue used by unit tests and non-blocking command-loop paths.
    pub event_queue: VecDeque<InputEvent>,
    /// Input already received from the host but not yet returned by `read_char`.
    pub pending_input_events: VecDeque<InputEvent>,
    /// Terminal id for the currently active `kboard`.
    active_terminal_id: u64,
    /// Parked keyboard-local state for terminals that are not currently active.
    parked_kboards: HashMap<u64, KBoard>,
    /// Keyboard-local command-loop state.
    pub kboard: KBoard,
}

impl KeyboardRuntime {
    fn event_counts_as_pending_input(event: &Value) -> bool {
        !matches!(
            crate::emacs_core::value::list_to_vec(event).as_deref(),
            Some([head, ..]) if head.is_symbol_named("help-echo")
        )
    }

    pub fn new() -> Self {
        Self {
            event_queue: VecDeque::new(),
            pending_input_events: VecDeque::new(),
            active_terminal_id: crate::emacs_core::terminal::pure::TERMINAL_ID,
            parked_kboards: HashMap::new(),
            kboard: KBoard::new(),
        }
    }

    pub fn active_terminal_id(&self) -> u64 {
        self.active_terminal_id
    }

    pub fn mouse_pixel_position(&self) -> Option<MousePixelPositionState> {
        self.kboard.mouse_pixel_position()
    }

    pub fn set_mouse_pixel_position(
        &mut self,
        frame_id: Option<crate::window::FrameId>,
        x: i64,
        y: i64,
    ) {
        self.kboard.set_mouse_pixel_position(frame_id, x, y);
    }

    pub fn select_terminal(&mut self, terminal_id: u64) {
        if self.active_terminal_id == terminal_id {
            return;
        }
        let current_id = self.active_terminal_id;
        let current = std::mem::take(&mut self.kboard);
        self.parked_kboards.insert(current_id, current);
        self.kboard = self.parked_kboards.remove(&terminal_id).unwrap_or_default();
        self.active_terminal_id = terminal_id;
    }

    pub fn delete_terminal_kboard(&mut self, terminal_id: u64) {
        self.parked_kboards.remove(&terminal_id);
        if self.active_terminal_id == terminal_id {
            self.kboard = KBoard::default();
        }
    }

    fn parked_terminal_ids(&self) -> Vec<u64> {
        let mut ids = crate::emacs_core::terminal::pure::live_terminal_ids_in_keyboard_poll_order()
            .into_iter()
            .filter(|terminal_id| self.parked_kboards.contains_key(terminal_id))
            .collect::<Vec<_>>();
        let mut unknown_ids = self
            .parked_kboards
            .keys()
            .copied()
            .filter(|terminal_id| !ids.contains(terminal_id))
            .collect::<Vec<_>>();
        unknown_ids.sort_unstable();
        ids.extend(unknown_ids);
        ids
    }

    fn poll_parked_kboard<R>(&mut self, mut f: impl FnMut(&mut KBoard) -> Option<R>) -> Option<R> {
        for terminal_id in self.parked_terminal_ids() {
            let Some(kboard) = self.parked_kboards.get_mut(&terminal_id) else {
                continue;
            };
            let Some(result) = f(kboard) else {
                continue;
            };
            self.select_terminal(terminal_id);
            return Some(result);
        }
        None
    }

    pub fn take_unread_selection_event(&mut self) -> Option<Value> {
        self.kboard
            .unread_selection_event
            .take()
            .or_else(|| self.poll_parked_kboard(|kboard| kboard.unread_selection_event.take()))
    }

    pub fn pop_unread_event(&mut self) -> Option<Value> {
        self.kboard
            .unread_events
            .pop_front()
            .or_else(|| self.poll_parked_kboard(|kboard| kboard.unread_events.pop_front()))
    }

    pub fn next_executing_kbd_macro_event(&mut self) -> Option<Value> {
        if let Some(ref macro_events) = self.kboard.executing_kbd_macro
            && self.kboard.kbd_macro_index < macro_events.len()
        {
            let event = macro_events[self.kboard.kbd_macro_index];
            self.kboard.kbd_macro_index += 1;
            return Some(event);
        }
        self.poll_parked_kboard(|kboard| {
            let macro_events = kboard.executing_kbd_macro.as_ref()?;
            if kboard.kbd_macro_index >= macro_events.len() {
                return None;
            }
            let event = macro_events[kboard.kbd_macro_index];
            kboard.kbd_macro_index += 1;
            Some(event)
        })
    }

    pub fn has_pending_kboard_input(&self) -> bool {
        if self.kboard.unread_selection_event.is_some()
            || self
                .kboard
                .unread_events
                .iter()
                .any(Self::event_counts_as_pending_input)
            || self
                .kboard
                .executing_kbd_macro
                .as_ref()
                .is_some_and(|events| self.kboard.kbd_macro_index < events.len())
        {
            return true;
        }
        self.parked_kboards.values().any(|kboard| {
            kboard.unread_selection_event.is_some()
                || kboard
                    .unread_events
                    .iter()
                    .any(Self::event_counts_as_pending_input)
                || kboard
                    .executing_kbd_macro
                    .as_ref()
                    .is_some_and(|events| kboard.kbd_macro_index < events.len())
        })
    }

    pub fn set_terminal_translation_maps(
        &mut self,
        input_decode_map: Value,
        local_function_key_map: Value,
    ) {
        self.kboard
            .set_terminal_translation_maps(input_decode_map, local_function_key_map);
    }

    pub fn set_input_decode_map(&mut self, map: Value) {
        self.kboard.set_input_decode_map(map);
    }

    pub fn input_decode_map(&self) -> Value {
        self.kboard.input_decode_map()
    }

    pub fn set_local_function_key_map(&mut self, map: Value) {
        self.kboard.set_local_function_key_map(map);
    }

    pub fn local_function_key_map(&self) -> Value {
        self.kboard.local_function_key_map()
    }

    pub fn enqueue_event(&mut self, event: InputEvent) {
        self.event_queue.push_back(event);
    }

    pub fn unread_event(&mut self, event: Value) {
        self.kboard.unread_events.push_back(event);
    }

    pub fn unread_key(&mut self, event: KeyEvent) {
        self.unread_event(event.to_emacs_event_value());
    }

    pub fn read_key_event(&mut self) -> Option<Value> {
        if let Some(event) = self.pop_unread_event() {
            return Some(event);
        }

        if let Some(event) = self.next_executing_kbd_macro_event() {
            return Some(event);
        }

        while let Some(event) = self.event_queue.pop_front() {
            if let InputEvent::KeyPress { key, .. } = event {
                let emacs_event = key.to_emacs_event_value();
                self.kboard.store_kbd_macro_event(emacs_event);
                return Some(emacs_event);
            }
        }

        None
    }

    pub fn reset_key_sequence(&mut self) {
        self.kboard.current_key_sequence.reset();
    }

    pub fn push_key_sequence_input_event(&mut self, event: Value) {
        self.kboard.current_key_sequence.push_input_event(event);
    }

    pub fn rewrite_key_sequence_translation(&mut self, events: Vec<Value>) {
        self.kboard
            .current_key_sequence
            .replace_translated_events(events);
    }

    pub fn key_sequence_snapshot(&self) -> (Vec<Value>, Vec<Value>) {
        self.kboard.current_key_sequence.snapshot()
    }

    pub fn set_command_key_sequences(&mut self, translated: Vec<Value>, raw: Vec<Value>) {
        self.kboard.command_keys = translated;
        self.kboard.raw_command_keys = raw;
    }

    pub fn set_translated_command_keys(&mut self, keys: Vec<Value>) {
        self.kboard.command_keys = keys;
    }

    pub fn set_read_command_keys(&mut self, keys: Vec<Value>) {
        self.kboard.command_keys = keys.clone();
        self.kboard.raw_command_keys = keys;
    }

    pub fn clear_read_command_keys(&mut self) {
        self.kboard.command_keys.clear();
        self.kboard.raw_command_keys.clear();
    }

    pub fn read_command_keys(&self) -> &[Value] {
        &self.kboard.command_keys
    }

    pub fn read_raw_command_keys(&self) -> &[Value] {
        &self.kboard.raw_command_keys
    }

    pub fn record_input_event(&mut self, event: Value) {
        self.kboard.recent_input_events.push(event);
        if self.kboard.recent_input_events.len() > crate::emacs_core::eval::RECENT_INPUT_EVENT_LIMIT
        {
            self.kboard.recent_input_events.remove(0);
        }
    }

    pub fn recent_input_events(&self) -> &[Value] {
        &self.kboard.recent_input_events
    }

    pub fn clear_recent_input_events(&mut self) {
        self.kboard.recent_input_events.clear();
    }

    pub fn start_kbd_macro(&mut self) {
        self.kboard.start_kbd_macro();
    }

    pub fn start_kbd_macro_with_initial(&mut self, initial_events: Option<&[Value]>, append: bool) {
        self.kboard
            .start_kbd_macro_with_initial(initial_events, append);
    }

    pub fn store_kbd_macro_event(&mut self, event: Value) {
        self.kboard.store_kbd_macro_event(event);
    }

    pub fn finalize_kbd_macro_chars(&mut self) {
        self.kboard.finalize_kbd_macro_chars();
    }

    pub fn cancel_kbd_macro_events(&mut self) {
        self.kboard.cancel_kbd_macro_events();
    }

    pub fn end_kbd_macro(&mut self) -> Vec<Value> {
        self.kboard.end_kbd_macro()
    }

    pub fn last_kbd_macro(&self) -> Option<&[Value]> {
        self.kboard.last_kbd_macro()
    }

    pub fn begin_executing_kbd_macro(&mut self, events: Vec<Value>) {
        self.kboard.begin_executing_kbd_macro(events);
    }

    pub fn finish_executing_kbd_macro(&mut self) {
        self.kboard.finish_executing_kbd_macro();
    }
}

impl Default for KeyboardRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::gc_trace::GcTrace for KeyboardRuntime {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        self.kboard.trace_roots(roots);
        for kboard in self.parked_kboards.values() {
            kboard.trace_roots(roots);
        }
    }
}

/// State of the command loop.
pub struct CommandLoop {
    /// Keyboard-local runtime state.
    pub keyboard: KeyboardRuntime,
    /// Current prefix argument.
    pub prefix_arg: PrefixArg,
    /// Whether we are in a recursive edit.
    pub recursive_depth: usize,
    /// Whether the command loop is running.
    pub running: bool,
    /// Whether C-g was pressed (quit flag).
    pub quit_flag: bool,
    /// Inhibit quit (during critical sections).
    pub inhibit_quit: bool,
    /// GNU-style idle timer epoch: when Emacs most recently became idle.
    idle_start_time: Option<std::time::Instant>,
    /// Last idle epoch preserved across non-user internal events.
    last_idle_start_time: Option<std::time::Instant>,
    /// Number of non-macro input events that have been read by the
    /// command loop. Mirrors GNU `num_nonmacro_input_events` at
    /// `src/keyboard.c:106`, which is incremented from
    /// `record_char` whenever the event did *not* come from an
    /// executing keyboard macro. Used by the `auto-save-interval`
    /// check in `command_loop_1` (audit Finding 9).
    pub num_nonmacro_input_events: u64,
    /// Value of `num_nonmacro_input_events` the last time an
    /// auto-save fired from the command loop. GNU tracks this in
    /// the global `last_auto_save` at `src/keyboard.c:107`.
    pub last_auto_save_input_events: u64,
}

impl CommandLoop {
    pub fn new() -> Self {
        Self {
            keyboard: KeyboardRuntime::new(),
            prefix_arg: PrefixArg::None,
            recursive_depth: 0,
            running: false,
            quit_flag: false,
            inhibit_quit: false,
            idle_start_time: None,
            last_idle_start_time: None,
            num_nonmacro_input_events: 0,
            last_auto_save_input_events: 0,
        }
    }

    /// Push an input event.
    pub fn enqueue_event(&mut self, event: InputEvent) {
        self.keyboard.enqueue_event(event);
    }

    /// Push an unread command event (to be processed before the queue).
    pub fn unread_event(&mut self, event: Value) {
        self.keyboard.unread_event(event);
    }

    /// Push an unread key event (to be processed before the queue).
    pub fn unread_key(&mut self, event: KeyEvent) {
        self.keyboard.unread_key(event);
    }

    /// Read the next key event as a Lisp-visible Emacs event.
    /// Returns from unread events first, then the event queue.
    pub fn read_key_event(&mut self) -> Option<Value> {
        self.keyboard.read_key_event()
    }

    /// Reset the key sequence accumulator.
    pub fn reset_key_sequence(&mut self) {
        self.keyboard.reset_key_sequence();
    }

    pub fn set_command_key_sequences(&mut self, translated: Vec<Value>, raw: Vec<Value>) {
        self.keyboard.set_command_key_sequences(translated, raw);
    }

    pub fn set_translated_command_keys(&mut self, keys: Vec<Value>) {
        self.keyboard.set_translated_command_keys(keys);
    }

    pub fn set_read_command_keys(&mut self, keys: Vec<Value>) {
        self.keyboard.set_read_command_keys(keys);
    }

    pub fn clear_read_command_keys(&mut self) {
        self.keyboard.clear_read_command_keys();
    }

    pub fn read_command_keys(&self) -> &[Value] {
        self.keyboard.read_command_keys()
    }

    pub fn read_raw_command_keys(&self) -> &[Value] {
        self.keyboard.read_raw_command_keys()
    }

    pub fn record_input_event(&mut self, event: Value) {
        // GNU `src/keyboard.c:11168-11175` (record_char) increments
        // `num_nonmacro_input_events` only when the event did not
        // come from an executing keyboard macro. We mirror that
        // here so the command_loop_1 auto-save check (audit
        // Finding 9) sees the same counter GNU does.
        if self.keyboard.kboard.executing_kbd_macro.is_none() {
            self.num_nonmacro_input_events = self.num_nonmacro_input_events.saturating_add(1);
        }
        self.keyboard.record_input_event(event);
    }

    pub fn recent_input_events(&self) -> &[Value] {
        self.keyboard.recent_input_events()
    }

    pub fn clear_recent_input_events(&mut self) {
        self.keyboard.clear_recent_input_events();
    }

    /// Start recording a keyboard macro.
    pub fn start_kbd_macro(&mut self) {
        self.keyboard.start_kbd_macro();
    }

    pub fn start_kbd_macro_with_initial(&mut self, initial_events: Option<&[Value]>, append: bool) {
        self.keyboard
            .start_kbd_macro_with_initial(initial_events, append);
    }

    pub fn store_kbd_macro_event(&mut self, event: Value) {
        self.keyboard.store_kbd_macro_event(event);
    }

    pub fn finalize_kbd_macro_chars(&mut self) {
        self.keyboard.finalize_kbd_macro_chars();
    }

    pub fn cancel_kbd_macro_events(&mut self) {
        self.keyboard.cancel_kbd_macro_events();
    }

    /// Stop recording a keyboard macro.
    pub fn end_kbd_macro(&mut self) -> Vec<Value> {
        self.keyboard.end_kbd_macro()
    }

    pub fn last_kbd_macro(&self) -> Option<&[Value]> {
        self.keyboard.last_kbd_macro()
    }

    pub fn begin_executing_kbd_macro(&mut self, events: Vec<Value>) {
        self.keyboard.begin_executing_kbd_macro(events);
    }

    pub fn finish_executing_kbd_macro(&mut self) {
        self.keyboard.finish_executing_kbd_macro();
    }

    /// Signal a quit (C-g).
    pub fn signal_quit(&mut self) {
        if !self.inhibit_quit {
            self.quit_flag = true;
        }
    }

    /// Clear the quit flag and return whether it was set.
    pub fn check_quit(&mut self) -> bool {
        let was_set = self.quit_flag;
        self.quit_flag = false;
        was_set
    }
}

impl Default for CommandLoop {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::gc_trace::GcTrace for CommandLoop {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        self.keyboard.trace_roots(roots);
    }
}

fn apply_resize_input_event_in_keyboard_runtime(
    frames: &mut crate::window::FrameManager,
    width: u32,
    height: u32,
    emacs_frame_id: u64,
) {
    let target_fid = if emacs_frame_id == 0 {
        frames.selected_frame().map(|frame| frame.id)
    } else {
        Some(crate::window::FrameId(emacs_frame_id))
    };

    if let Some(fid) = target_fid
        && let Some(frame) = frames.get_mut(fid)
    {
        frame.resize_pixelwise(width, height);
    }
}

fn pending_live_gui_resize_target(
    frames: &crate::window::FrameManager,
    emacs_frame_id: u64,
) -> Option<crate::window::FrameId> {
    let target_fid = if emacs_frame_id == 0 {
        frames.selected_frame().map(|frame| frame.id)
    } else {
        Some(crate::window::FrameId(emacs_frame_id))
    }?;
    frames
        .get(target_fid)
        .and_then(|frame| frame.pending_gui_resize.as_ref().map(|_| target_fid))
}

fn sync_pending_resize_events_in_keyboard_runtime(
    frames: &mut crate::window::FrameManager,
    input_rx: &mut Option<crossbeam_channel::Receiver<InputEvent>>,
    keyboard: &mut KeyboardRuntime,
) -> bool {
    let mut applied_resize = false;
    let mut deferred = VecDeque::new();
    let pending_input_events = &mut keyboard.pending_input_events;

    loop {
        match pending_input_events.front() {
            Some(InputEvent::Focus { .. }) => {
                if let Some(event) = pending_input_events.pop_front() {
                    deferred.push_back(event);
                }
            }
            Some(InputEvent::Resize {
                width,
                height,
                emacs_frame_id,
            }) => {
                if pending_live_gui_resize_target(frames, *emacs_frame_id).is_some() {
                    break;
                }
                let (width, height, emacs_frame_id) = (*width, *height, *emacs_frame_id);
                pending_input_events.pop_front();
                apply_resize_input_event_in_keyboard_runtime(frames, width, height, emacs_frame_id);
                applied_resize = true;
            }
            _ => break,
        }
    }

    if !pending_input_events.is_empty() {
        while let Some(event) = deferred.pop_back() {
            pending_input_events.push_front(event);
        }
        return applied_resize;
    }

    let Some(rx) = input_rx.clone() else {
        while let Some(event) = deferred.pop_back() {
            pending_input_events.push_front(event);
        }
        return applied_resize;
    };

    loop {
        match rx.try_recv() {
            Ok(InputEvent::Resize {
                width,
                height,
                emacs_frame_id,
            }) => {
                if pending_live_gui_resize_target(frames, emacs_frame_id).is_some() {
                    // Preserve host resize acks until the geometry-query path
                    // flushes the deferred live-GUI resize request.
                    deferred.push_back(InputEvent::Resize {
                        width,
                        height,
                        emacs_frame_id,
                    });
                    break;
                }
                apply_resize_input_event_in_keyboard_runtime(frames, width, height, emacs_frame_id);
                applied_resize = true;
            }
            Ok(event @ InputEvent::Focus { .. }) => {
                deferred.push_back(event);
            }
            Ok(event) => {
                deferred.push_back(event);
                break;
            }
            Err(crossbeam_channel::TryRecvError::Empty) => break,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                break;
            }
        }
    }

    while let Some(event) = deferred.pop_back() {
        pending_input_events.push_front(event);
    }

    applied_resize
}

fn wait_for_pending_resize_events_in_keyboard_runtime(
    frames: &mut crate::window::FrameManager,
    input_rx: &mut Option<crossbeam_channel::Receiver<InputEvent>>,
    keyboard: &mut KeyboardRuntime,
    timeout: Duration,
) -> bool {
    if sync_pending_resize_events_in_keyboard_runtime(frames, input_rx, keyboard) {
        return true;
    }

    let Some(rx) = input_rx.clone() else {
        return false;
    };
    let deadline = Instant::now() + timeout;
    let pending_input_events = &mut keyboard.pending_input_events;

    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        match rx.recv_timeout(remaining) {
            Ok(InputEvent::Resize {
                width,
                height,
                emacs_frame_id,
            }) => {
                apply_resize_input_event_in_keyboard_runtime(frames, width, height, emacs_frame_id);
                return true;
            }
            Ok(event) => pending_input_events.push_back(event),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => break,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    false
}

fn input_event_triggers_throw_on_input(event: &InputEvent) -> bool {
    !matches!(
        event,
        InputEvent::Resize { .. }
            | InputEvent::Focus { .. }
            | InputEvent::MonitorsChanged { .. }
            | InputEvent::MouseMove { .. }
    )
}

fn input_event_is_wait_path_special(event: &InputEvent) -> bool {
    matches!(
        event,
        InputEvent::Resize { .. }
            | InputEvent::MonitorsChanged { .. }
            | InputEvent::MouseMove { .. }
            | InputEvent::WindowClose { .. }
    )
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct WaitPathSpecialInputOutcome {
    pub(crate) redisplay_needed: bool,
}

fn sync_opening_gui_frame_size_from_host_in_keyboard_runtime(
    frames: &mut crate::window::FrameManager,
    display_host: Option<&dyn crate::emacs_core::eval::DisplayHost>,
) {
    let trace_host_sync = std::env::var("NEOMACS_TRACE_HOST_SYNC")
        .ok()
        .is_some_and(|value| value == "1");
    let Some(host) = display_host else {
        if trace_host_sync {
            tracing::debug!("sync_opening_gui_frame_size_from_host: no display host");
        }
        return;
    };
    if !host.opening_gui_frame_pending() {
        if trace_host_sync {
            tracing::debug!("sync_opening_gui_frame_size_from_host: no opening gui frame pending");
        }
        return;
    }
    let Some(size) = host.current_primary_window_size() else {
        if trace_host_sync {
            tracing::debug!("sync_opening_gui_frame_size_from_host: host size unavailable");
        }
        return;
    };
    if size.width == 0 || size.height == 0 {
        if trace_host_sync {
            tracing::debug!(
                "sync_opening_gui_frame_size_from_host: ignoring zero host size {}x{}",
                size.width,
                size.height
            );
        }
        return;
    }
    let Some(fid) = frames.selected_frame().map(|frame| frame.id) else {
        if trace_host_sync {
            tracing::debug!("sync_opening_gui_frame_size_from_host: no selected frame");
        }
        return;
    };
    let Some(frame) = frames.get_mut(fid) else {
        if trace_host_sync {
            tracing::debug!(
                "sync_opening_gui_frame_size_from_host: selected frame {:?} missing",
                fid
            );
        }
        return;
    };
    if frame.effective_window_system().is_none() {
        if trace_host_sync {
            tracing::debug!(
                "sync_opening_gui_frame_size_from_host: selected frame {:?} is not gui (size={}x{})",
                fid,
                frame.width,
                frame.height
            );
        }
        return;
    }
    if frame.width == size.width && frame.height == size.height {
        if trace_host_sync {
            tracing::debug!(
                "sync_opening_gui_frame_size_from_host: selected frame {:?} already matches host size {}x{}",
                fid,
                size.width,
                size.height
            );
        }
        return;
    }
    tracing::debug!(
        "sync_opening_gui_frame_size_from_host: resizing selected frame {:?} from {}x{} to {}x{}",
        fid,
        frame.width,
        frame.height,
        size.width,
        size.height
    );
    frame.resize_pixelwise(size.width, size.height);
}

fn pending_gnu_timer_in_keyboard_runtime(
    timer: Value,
) -> Option<crate::emacs_core::eval::PendingGnuTimer> {
    if !timer.is_vector() {
        return None;
    };

    let slots = timer.as_vector_data()?.clone();
    if !(9..=10).contains(&slots.len()) {
        return None;
    }

    if !slots[0].is_nil() {
        return None;
    }

    if !slots[7].is_nil() {
        return None;
    }

    Some(crate::emacs_core::eval::PendingGnuTimer {
        timer,
        when: crate::emacs_core::eval::GnuTimerTimestamp {
            high_seconds: slots[1].as_int()?,
            low_seconds: slots[2].as_int()?,
            usecs: slots[3].as_int()?,
            psecs: slots.get(8).and_then(|v| v.as_int()).unwrap_or(0),
        },
    })
}

fn pending_gnu_idle_timer_in_keyboard_runtime(
    timer: Value,
) -> Option<crate::emacs_core::eval::PendingGnuTimer> {
    if !timer.is_vector() {
        return None;
    };

    let slots = timer.as_vector_data()?.clone();
    if !(9..=10).contains(&slots.len()) {
        return None;
    }

    if !slots[0].is_nil() {
        return None;
    }

    if slots[7].is_nil() {
        return None;
    }

    Some(crate::emacs_core::eval::PendingGnuTimer {
        timer,
        when: crate::emacs_core::eval::GnuTimerTimestamp {
            high_seconds: slots[1].as_int()?,
            low_seconds: slots[2].as_int()?,
            usecs: slots[3].as_int()?,
            psecs: slots.get(8).and_then(|v| v.as_int()).unwrap_or(0),
        },
    })
}

#[derive(Clone, Copy, Debug)]
struct MousePosnMetrics {
    point: Option<i64>,
    col: Option<i64>,
    row: Option<i64>,
    width: Option<i64>,
    height: Option<i64>,
}

#[derive(Clone, Copy, Debug)]
struct MousePosnDescriptor {
    window_or_frame: Value,
    area: Option<&'static str>,
    x: i64,
    y: i64,
    metrics: MousePosnMetrics,
}

impl crate::emacs_core::eval::Context {
    fn restore_delayed_selection_event(&mut self, delayed_selection_event: &mut Option<Value>) {
        if let Some(event) = delayed_selection_event.take() {
            self.command_loop
                .keyboard
                .kboard
                .set_unread_selection_event(event);
        }
    }

    fn restore_key_sequence_current_buffer(
        &mut self,
        saved_current_buffer: &mut Option<crate::buffer::BufferId>,
    ) {
        if let Some(buffer_id) = saved_current_buffer.take() {
            self.restore_current_buffer_if_live(buffer_id);
        }
    }

    fn has_switch_frame_event_kind(event: &Value) -> bool {
        match event.kind() {
            ValueKind::Cons => {
                let pair_car = event.cons_car();
                let pair_cdr = event.cons_cdr();
                matches!(
                    pair_car.as_symbol_name(),
                    Some("switch-frame") | Some("select-window")
                )
            }
            _ => false,
        }
    }

    fn lispy_frame_event_target(&self, emacs_frame_id: u64) -> Option<Value> {
        if emacs_frame_id == 0 {
            self.frames
                .selected_frame()
                .map(|frame| Value::make_frame(frame.id.0))
        } else {
            let fid = crate::window::FrameId(emacs_frame_id);
            self.frames.get(fid)?;
            Some(Value::make_frame(emacs_frame_id))
        }
    }

    fn make_lispy_switch_frame_event(&self, emacs_frame_id: u64) -> Option<Value> {
        let frame = self.lispy_frame_event_target(emacs_frame_id)?;
        Some(Value::list(vec![Value::symbol("switch-frame"), frame]))
    }

    fn make_lispy_focus_event(&self, focused: bool, emacs_frame_id: u64) -> Option<Value> {
        let frame = self.lispy_frame_event_target(emacs_frame_id)?;
        Some(Value::list(vec![
            Value::symbol(if focused { "focus-in" } else { "focus-out" }),
            frame,
        ]))
    }

    fn make_lispy_delete_frame_event(&self, emacs_frame_id: u64) -> Option<Value> {
        let frame = self.lispy_frame_event_target(emacs_frame_id)?;
        Some(Value::list(vec![
            Value::symbol("delete-frame"),
            Value::list(vec![frame]),
        ]))
    }

    fn make_lispy_select_window_event(&self, window_id: crate::window::WindowId) -> Option<Value> {
        for frame_id in self.frames.frame_list() {
            let Some(frame) = self.frames.get(frame_id) else {
                continue;
            };
            if frame.find_window(window_id).is_some() {
                return Some(Value::list(vec![
                    Value::symbol("select-window"),
                    Value::list(vec![Value::make_window(window_id.0)]),
                ]));
            }
        }
        None
    }

    fn current_key_sequence_translation_maps(&self) -> [Value; 3] {
        [
            self.command_loop.keyboard.input_decode_map(),
            self.command_loop.keyboard.local_function_key_map(),
            self.eval_symbol("key-translation-map")
                .unwrap_or(Value::NIL),
        ]
    }

    fn special_event_binding(&self, event: &Value) -> Option<Value> {
        let special_event_map = self.obarray.symbol_value("special-event-map").copied()?;
        // GNU `read_char` calls `access_keymap` on the event head, so a
        // full lispy event like `(focus-in FRAME)` must match a keymap entry
        // stored under just `focus-in`.
        let lookup_event = match event.kind() {
            ValueKind::Cons => event.cons_car(),
            _ => *event,
        };
        let binding = crate::emacs_core::keymap::lookup_key_in_keymaps_in_obarray(
            self.obarray(),
            &[special_event_map],
            &[lookup_event],
            true,
        );
        if binding.is_nil() || binding.is_fixnum() {
            None
        } else {
            Some(binding)
        }
    }

    fn execute_special_event_if_bound(
        &mut self,
        event: Value,
    ) -> Result<bool, crate::emacs_core::error::Flow> {
        let Some(binding) = self.special_event_binding(&event) else {
            return Ok(false);
        };
        if !self.function_value_is_callable(&binding) {
            return Ok(false);
        }

        self.assign("last-input-event", event);
        let keys = Value::vector(vec![event]);
        self.apply(
            Value::symbol("command-execute"),
            vec![binding, Value::NIL, keys, Value::T],
        )?;
        Ok(true)
    }

    fn resolve_key_sequence_translation_binding(
        &mut self,
        binding: Value,
        prompt: Value,
    ) -> Result<Option<Vec<Value>>, crate::emacs_core::error::Flow> {
        let resolved = if self.function_value_is_callable(&binding) {
            self.apply(binding, vec![prompt])?
        } else {
            binding
        };
        Ok(key_sequence_translation_events(resolved))
    }

    fn lookup_key_sequence_suffix_translation(
        &mut self,
        map: Value,
        events: &[Value],
        prompt: Value,
    ) -> Result<Option<KeySequenceSuffixTranslation>, crate::emacs_core::error::Flow> {
        use crate::emacs_core::keymap::{is_list_keymap, list_keymap_lookup_seq};

        if map.is_nil() || !is_list_keymap(&map) {
            return Ok(None);
        }

        for start in 0..events.len() {
            let lookup = list_keymap_lookup_seq(&map, &events[start..]);
            let Some(replacement) =
                self.resolve_key_sequence_translation_binding(lookup, prompt)?
            else {
                continue;
            };
            return Ok(Some(KeySequenceSuffixTranslation { start, replacement }));
        }

        Ok(None)
    }

    fn translation_map_has_pending_suffix_prefix(&self, map: Value, events: &[Value]) -> bool {
        use crate::emacs_core::keymap::{is_list_keymap, list_keymap_lookup_seq};

        if map.is_nil() || !is_list_keymap(&map) {
            return false;
        }

        (0..events.len())
            .any(|start| is_list_keymap(&list_keymap_lookup_seq(&map, &events[start..])))
    }

    fn apply_translation_maps_to_current_key_sequence(
        &mut self,
        options: ReadKeySequenceOptions,
    ) -> Result<CurrentKeySequenceTranslation, crate::emacs_core::error::Flow> {
        let mut translated = self
            .command_loop
            .keyboard
            .kboard
            .current_key_sequence
            .translated_events()
            .to_vec();
        let mut has_pending_translation_prefix = false;

        for map in self.current_key_sequence_translation_maps() {
            if let Some(suffix_translation) =
                self.lookup_key_sequence_suffix_translation(map, &translated, options.prompt)?
            {
                translated.truncate(suffix_translation.start);
                translated.extend(suffix_translation.replacement);
            }
            has_pending_translation_prefix |=
                self.translation_map_has_pending_suffix_prefix(map, &translated);
        }

        self.command_loop
            .keyboard
            .rewrite_key_sequence_translation(translated.clone());

        Ok(CurrentKeySequenceTranslation {
            translated_events: translated,
            has_pending_translation_prefix,
        })
    }

    fn translate_upper_case_key_bindings_enabled(&self) -> bool {
        self.eval_symbol("translate-upper-case-key-bindings")
            .is_ok_and(|value| !value.is_nil())
    }

    /// Fetch the effective value of `echo-keystrokes` as seconds.
    /// Returns `None` when the variable is nil/unbound or of a
    /// wrong type. Mirrors GNU `keyboard.c` consumers which call
    /// `FIXNUMP` / `FLOATP` before using the value.
    fn lisp_echo_keystrokes_seconds(&self) -> Option<f64> {
        let value = self.eval_symbol("echo-keystrokes").ok()?;
        if value.is_nil() {
            return None;
        }
        value.as_number_f64()
    }

    /// Return true if EVENT matches `help-char` (default `Ctrl-h`
    /// == 8) or any entry in `help-event-list`.
    ///
    /// Mirrors GNU `keyboard.c:3014-3031` (`help_char_p`): the
    /// predicate used by `read_key_sequence` to decide whether an
    /// incoming event is a "help event" for the purposes of
    /// dispatching `prefix-help-command`.
    fn event_matches_help_char(&self, event: &Value) -> bool {
        let help_char = self.eval_symbol("help-char").unwrap_or(Value::NIL);
        if !help_char.is_nil() && *event == help_char {
            return true;
        }
        let help_event_list = self.eval_symbol("help-event-list").unwrap_or(Value::NIL);
        if help_event_list.is_nil() {
            return false;
        }
        let mut cursor = help_event_list;
        while cursor.is_cons() {
            if cursor.cons_car() == *event {
                return true;
            }
            cursor = cursor.cons_cdr();
        }
        false
    }

    /// Invoke `input-method-function` on a just-read character
    /// event. Returns `Ok(true)` when the character was consumed
    /// by the input method (the translated events are now on the
    /// `unread-command-events` queue and the caller should drop
    /// the original event from the current key sequence), or
    /// `Ok(false)` when the input method did not apply (no
    /// function bound, non-character event, or function returned
    /// a result that should flow through as-is).
    ///
    /// Mirrors GNU `keyboard.c:2707-2770`: when
    /// `input-method-function` is non-nil and the event is a
    /// printable character, call it with the character and
    /// re-route the returned list of events via
    /// `unread-command-events`.
    ///
    /// Keyboard audit Finding 10.
    fn maybe_apply_input_method_function(
        &mut self,
        event: Value,
    ) -> Result<bool, crate::emacs_core::error::Flow> {
        // Only translate ordinary printable characters. GNU
        // skips non-character events (function keys, mouse,
        // etc.) and the NUL byte. We apply the same filter.
        let Some(c) = event.as_fixnum() else {
            return Ok(false);
        };
        if c <= 0 || c >= 0x3fff_ff {
            return Ok(false);
        }

        let im_fn = self
            .eval_symbol("input-method-function")
            .unwrap_or(Value::NIL);
        if im_fn.is_nil() {
            return Ok(false);
        }

        // Guard against recursive input-method invocation. GNU
        // uses the `immediate_echo` flag; we use a dedicated bool
        // on CommandLoop so a pathological input-method that
        // re-reads input via `read-event` does not re-enter.
        if self.command_loop.keyboard.kboard.in_input_method_function {
            return Ok(false);
        }
        self.command_loop.keyboard.kboard.in_input_method_function = true;
        let call_result = self.apply(im_fn, vec![event]);
        self.command_loop.keyboard.kboard.in_input_method_function = false;
        let result = call_result?;

        // A nil or empty-list result drops the event. A cons
        // list is a queue of events to be re-read. Any other
        // value is treated as a single event replacement.
        let events: Vec<Value> = if result.is_nil() {
            Vec::new()
        } else if result.is_cons() {
            crate::emacs_core::value::list_to_vec(&result).unwrap_or_else(|| vec![result])
        } else {
            vec![result]
        };

        // Prepend onto `unread-command-events` in reverse so the
        // first translated event ends up at the head of the
        // queue. `push_unread_command_event` is itself a
        // prepending operation.
        for ev in events.into_iter().rev() {
            self.push_unread_command_event(ev);
        }

        Ok(true)
    }

    /// Dispatch `prefix-help-command` via `call-interactively`,
    /// abandoning the current key sequence. Returns `Ok(Some(..))`
    /// when the help command ran (the read_key_sequence caller
    /// should forward the empty sequence to the command loop) or
    /// `Ok(None)` when `prefix-help-command` is unbound (fall
    /// back to the ordinary lookup path).
    ///
    /// Mirrors GNU `keyboard.c:10188-10250`: pop the help-char
    /// from the current sequence so the help command sees the
    /// prefix as `(this-command-keys)`, then run
    /// `Fcall_interactively (Vprefix_help_command, Qnil, Qnil)`.
    ///
    /// Keyboard audit Finding 5.
    fn dispatch_prefix_help_command(
        &mut self,
        delayed_selection_event: &mut Option<Value>,
    ) -> Result<Option<(Vec<Value>, Value)>, crate::emacs_core::error::Flow> {
        let prefix_help_command = self
            .eval_symbol("prefix-help-command")
            .unwrap_or(Value::NIL);
        if prefix_help_command.is_nil() {
            return Ok(None);
        }
        // Pop the trailing help-char event from the current raw
        // sequence and from the translated event buffer. GNU's
        // read_key_sequence removes the help event before running
        // the help command so `(this-command-keys)` reports the
        // prefix only — matching the classic "C-x ?" behaviour.
        self.command_loop
            .keyboard
            .kboard
            .current_key_sequence
            .pop_last_events_for_help_char();

        self.restore_delayed_selection_event(delayed_selection_event);

        // Run the help command via call-interactively so advice
        // and `this-command` bookkeeping in the Lisp interactive
        // dispatcher stay consistent with every other interactive
        // command.
        let _ = self.apply(
            Value::symbol("call-interactively"),
            vec![prefix_help_command],
        )?;

        // Return an empty key sequence — command_loop_1 treats
        // that as "nothing to dispatch this tick" and immediately
        // reads the next key.
        self.command_loop
            .set_command_key_sequences(Vec::new(), Vec::new());
        Ok(Some((Vec::new(), Value::NIL)))
    }

    fn shift_translated_key_sequence_event(event: Value) -> Option<Value> {
        let key_event = crate::emacs_core::keymap::emacs_event_to_key_event(&event)?;

        match key_event {
            crate::emacs_core::keymap::KeyEvent::Char {
                code,
                ctrl,
                meta,
                shift,
                super_,
                hyper,
                alt,
            } => {
                if shift {
                    return Some(crate::emacs_core::keymap::key_event_to_emacs_event(
                        &crate::emacs_core::keymap::KeyEvent::Char {
                            code,
                            ctrl,
                            meta,
                            shift: false,
                            super_,
                            hyper,
                            alt,
                        },
                    ));
                }

                if !code.is_uppercase() {
                    return None;
                }

                let lowered = code.to_lowercase().next().unwrap_or(code);
                if lowered == code {
                    return None;
                }

                Some(crate::emacs_core::keymap::key_event_to_emacs_event(
                    &crate::emacs_core::keymap::KeyEvent::Char {
                        code: lowered,
                        ctrl,
                        meta,
                        shift,
                        super_,
                        hyper,
                        alt,
                    },
                ))
            }
            crate::emacs_core::keymap::KeyEvent::Function {
                name,
                ctrl,
                meta,
                shift,
                super_,
                hyper,
                alt,
            } => {
                if !shift {
                    return None;
                }

                Some(crate::emacs_core::keymap::key_event_to_emacs_event(
                    &crate::emacs_core::keymap::KeyEvent::Function {
                        name,
                        ctrl,
                        meta,
                        shift: false,
                        super_,
                        hyper,
                        alt,
                    },
                ))
            }
        }
    }

    fn apply_shift_translation_to_current_key_sequence(
        &mut self,
    ) -> Option<KeySequenceShiftTranslation> {
        let translated = self
            .command_loop
            .keyboard
            .kboard
            .current_key_sequence
            .translated_events()
            .to_vec();
        let (index, original_event) = translated
            .len()
            .checked_sub(1)
            .map(|index| (index, translated[index]))?;
        let translated_event = Self::shift_translated_key_sequence_event(original_event)?;
        let mut rewritten = translated;
        rewritten[index] = translated_event;
        self.command_loop
            .keyboard
            .rewrite_key_sequence_translation(rewritten);
        Some(KeySequenceShiftTranslation {
            index,
            original_event,
        })
    }

    fn finalize_shift_translated_key_sequence(
        &mut self,
        sequence_is_undefined: bool,
        options: ReadKeySequenceOptions,
        shift_translation: Option<KeySequenceShiftTranslation>,
    ) {
        let mut shift_translated = false;

        if let Some(shift_translation) = shift_translation {
            let current_len = self
                .command_loop
                .keyboard
                .kboard
                .current_key_sequence
                .translated_events()
                .len();
            let restore_original = (options.dont_downcase_last || sequence_is_undefined)
                && shift_translation.index + 1 == current_len;
            if restore_original {
                let mut translated = self
                    .command_loop
                    .keyboard
                    .kboard
                    .current_key_sequence
                    .translated_events()
                    .to_vec();
                translated[shift_translation.index] = shift_translation.original_event;
                self.command_loop
                    .keyboard
                    .rewrite_key_sequence_translation(translated);
            } else {
                shift_translated = true;
            }
        }

        self.assign(
            "this-command-keys-shift-translated",
            if shift_translated {
                Value::T
            } else {
                Value::NIL
            },
        );
    }

    pub(crate) fn apply_resize_input_event(
        &mut self,
        width: u32,
        height: u32,
        emacs_frame_id: u64,
        trigger_redisplay: bool,
    ) {
        let trace_frame_geometry = std::env::var("NEOMACS_TRACE_FRAME_GEOMETRY")
            .ok()
            .is_some_and(|value| value == "1");
        let target_fid = if emacs_frame_id == 0 {
            self.frames.selected_frame().map(|frame| frame.id)
        } else {
            Some(crate::window::FrameId(emacs_frame_id))
        };
        let selected_fid = self.frames.selected_frame().map(|selected| selected.id);
        tracing::debug!(
            "apply_resize_input_event: {}x{} emacs_frame_id=0x{:x} target_fid={:?}",
            width,
            height,
            emacs_frame_id,
            target_fid
        );
        if let Some(fid) = target_fid {
            if trace_frame_geometry {
                if let Some(frame) = self.frames.get(fid) {
                    tracing::debug!(
                        "apply_resize_input_event: before fid={:?} selected={:?} size={}x{} effective_ws={:?} param_ws={:?}",
                        fid,
                        selected_fid,
                        frame.width,
                        frame.height,
                        frame.effective_window_system(),
                        frame.parameter("window-system")
                    );
                }
            }
            apply_resize_input_event_in_keyboard_runtime(
                &mut self.frames,
                width,
                height,
                emacs_frame_id,
            );
            if let Some(frame) = self.frames.get(fid) {
                tracing::debug!(
                    "apply_resize_input_event: resized frame {:?} to {}x{}",
                    fid,
                    frame.width,
                    frame.height
                );
                if trace_frame_geometry {
                    tracing::debug!(
                        "apply_resize_input_event: after fid={:?} selected={:?} size={}x{} effective_ws={:?} param_ws={:?}",
                        fid,
                        selected_fid,
                        frame.width,
                        frame.height,
                        frame.effective_window_system(),
                        frame.parameter("window-system")
                    );
                }
            }
        }
        if trigger_redisplay {
            self.redisplay();
        }
    }

    pub(crate) fn sync_pending_resize_events(&mut self) -> bool {
        let applied_resize = sync_pending_resize_events_in_keyboard_runtime(
            &mut self.frames,
            &mut self.input_rx,
            &mut self.command_loop.keyboard,
        );
        sync_opening_gui_frame_size_from_host_in_keyboard_runtime(
            &mut self.frames,
            self.display_host.as_deref(),
        );
        applied_resize
    }

    pub(crate) fn wait_for_pending_resize_events(&mut self, timeout: Duration) -> bool {
        let applied_resize = wait_for_pending_resize_events_in_keyboard_runtime(
            &mut self.frames,
            &mut self.input_rx,
            &mut self.command_loop.keyboard,
            timeout,
        );
        sync_opening_gui_frame_size_from_host_in_keyboard_runtime(
            &mut self.frames,
            self.display_host.as_deref(),
        );
        applied_resize
    }

    fn take_next_wait_path_special_input_event(
        &mut self,
    ) -> Result<Option<InputEvent>, crate::emacs_core::error::Flow> {
        if let Some(event) = self
            .command_loop
            .keyboard
            .pending_input_events
            .front()
            .cloned()
        {
            if input_event_is_wait_path_special(&event) {
                self.command_loop.keyboard.pending_input_events.pop_front();
                self.timer_stop_idle();
                return Ok(Some(event));
            }
            return Ok(None);
        }

        if self.stage_next_host_input_event_if_available()? {
            if let Some(event) = self
                .command_loop
                .keyboard
                .pending_input_events
                .front()
                .cloned()
                && input_event_is_wait_path_special(&event)
            {
                self.command_loop.keyboard.pending_input_events.pop_front();
                self.timer_stop_idle();
                return Ok(Some(event));
            }
            return Ok(None);
        }

        Ok(None)
    }

    pub(crate) fn stage_next_host_input_event_if_available(
        &mut self,
    ) -> Result<bool, crate::emacs_core::error::Flow> {
        let Some(ref rx) = self.input_rx else {
            return Ok(false);
        };
        match rx.try_recv() {
            Ok(event) => {
                self.command_loop
                    .keyboard
                    .pending_input_events
                    .push_back(event);
                Ok(true)
            }
            Err(crossbeam_channel::TryRecvError::Empty) => Ok(false),
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                self.handle_display_terminal_disconnect();
                Err(crate::emacs_core::error::signal("quit", vec![]))
            }
        }
    }

    pub(crate) fn service_wait_path_special_input_events(
        &mut self,
    ) -> Result<WaitPathSpecialInputOutcome, crate::emacs_core::error::Flow> {
        let mut outcome = WaitPathSpecialInputOutcome::default();

        if self.sync_pending_resize_events() {
            outcome.redisplay_needed = true;
        }

        while let Some(event) = self.take_next_wait_path_special_input_event()? {
            if input_event_triggers_throw_on_input(&event)
                && self.interrupt_for_input_event_if_requested(event.clone())?
            {
                continue;
            }

            match event {
                InputEvent::Resize {
                    width,
                    height,
                    emacs_frame_id,
                } => {
                    self.apply_resize_input_event(width, height, emacs_frame_id, false);
                    outcome.redisplay_needed = true;
                }
                InputEvent::MonitorsChanged { monitors } => {
                    crate::emacs_core::builtins::set_neomacs_monitor_info(monitors);
                    let hook_sym = crate::emacs_core::hook_runtime::hook_symbol_by_name(
                        self,
                        "display-monitors-changed-functions",
                    );
                    let terminal = crate::emacs_core::terminal::pure::terminal_handle_value();
                    let _ = crate::emacs_core::hook_runtime::run_named_hook(
                        self,
                        hook_sym,
                        &[terminal],
                    )?;
                }
                InputEvent::MouseMove {
                    x,
                    y,
                    target_frame_id,
                    ..
                } => {
                    self.note_mouse_move_input_event(x, y, target_frame_id);
                    self.timer_resume_idle();
                }
                InputEvent::WindowClose { emacs_frame_id } => {
                    self.handle_window_close_input_event(emacs_frame_id)?;
                }
                _ => {}
            }
        }

        Ok(outcome)
    }

    fn handle_window_close_input_event(
        &mut self,
        emacs_frame_id: u64,
    ) -> Result<(), crate::emacs_core::error::Flow> {
        self.timer_resume_idle();
        if let Some(event) = self.make_lispy_delete_frame_event(emacs_frame_id) {
            if self.execute_special_event_if_bound(event)? {
                return Ok(());
            }
        }
        self.command_loop.running = false;
        Err(crate::emacs_core::error::signal("quit", vec![]))
    }

    /// Read a complete key sequence through keymaps.
    ///
    /// Mirrors GNU Emacs `read_key_sequence()` (keyboard.c:10098).
    /// Reads keys one at a time, following prefix keymaps until a
    /// complete binding (command) or undefined key is found.
    ///
    /// After each key, checks translation maps in order:
    /// 1. `input-decode-map` — terminal-specific key decoding
    /// 2. `local-function-key-map` (inherits `function-key-map`) — function
    ///    key translation
    /// 3. `key-translation-map` — user-defined key translations
    ///
    /// Returns (key_events_as_emacs_values, binding).
    /// binding is Value::NIL if the key sequence is undefined.
    pub(crate) fn read_key_sequence(
        &mut self,
    ) -> Result<(Vec<Value>, Value), crate::emacs_core::error::Flow> {
        self.read_key_sequence_with_options(ReadKeySequenceOptions::default())
    }

    pub(crate) fn read_key_sequence_with_options(
        &mut self,
        options: ReadKeySequenceOptions,
    ) -> Result<(Vec<Value>, Value), crate::emacs_core::error::Flow> {
        use crate::emacs_core::keymap::{
            resolve_active_key_binding, resolve_prefix_keymap_binding_in_obarray,
        };

        self.sync_keyboard_terminal_owner();
        self.command_loop.reset_key_sequence();
        self.assign("this-command-keys-shift-translated", Value::NIL);
        let mut shift_translation: Option<KeySequenceShiftTranslation> = None;
        let mut delayed_selection_event: Option<Value> = None;
        let mut saved_current_buffer: Option<crate::buffer::BufferId> = None;
        let mut replay_current_sequence = false;

        loop {
            if replay_current_sequence {
                replay_current_sequence = false;
                tracing::debug!("read_key_sequence: replaying buffered sequence");
            } else {
                let emacs_event = match self.read_char_with_timeout(None) {
                    Ok(Some(event)) => event,
                    Ok(None) => {
                        self.restore_delayed_selection_event(&mut delayed_selection_event);
                        self.restore_key_sequence_current_buffer(&mut saved_current_buffer);
                        self.command_loop
                            .set_command_key_sequences(Vec::new(), Vec::new());
                        return Ok((Vec::new(), Value::NIL));
                    }
                    Err(err) => {
                        self.restore_delayed_selection_event(&mut delayed_selection_event);
                        self.restore_key_sequence_current_buffer(&mut saved_current_buffer);
                        return Err(err);
                    }
                };
                self.clear_quit_flag_after_read_key_sequence_event(&emacs_event);
                if Self::has_switch_frame_event_kind(&emacs_event)
                    && (!self
                        .command_loop
                        .keyboard
                        .kboard
                        .current_key_sequence
                        .raw_events()
                        .is_empty()
                        || !options.can_return_switch_frame)
                {
                    delayed_selection_event = Some(emacs_event);
                    tracing::debug!("read_key_sequence: deferring selection event");
                    continue;
                }
                self.command_loop
                    .keyboard
                    .push_key_sequence_input_event(emacs_event);

                self.record_input_event(emacs_event);

                tracing::debug!(
                    "read_key_sequence: event={} starting translation",
                    crate::emacs_core::print::print_value(&emacs_event)
                );

                // Keyboard audit Finding 5: help-char dispatch.
                // GNU `keyboard.c:10188-10250` in `read_key_sequence`
                // checks every incoming event against
                // `Vhelp_char` / `Vhelp_event_list` and, if the
                // current sequence already has a prefix, runs
                // `Vprefix_help_command` (default
                // `describe-prefix-bindings`) via
                // `Fcall_interactively` instead of looking the
                // resulting key up in the active keymap.
                //
                // We do the same here, after the raw event is
                // pushed so `(this-command-keys)` sees the full
                // sequence, but before translation/lookup — the
                // help command must shadow any hypothetical
                // binding for the help character under the prefix.
                //
                // `raw_events().len() > 1` is the analog of GNU's
                // "in a prefix state" check: we only dispatch if
                // there is at least one earlier event in the
                // current sequence. A bare help-char at sequence
                // start still goes through the ordinary keymap
                // lookup so `C-h` etc. can be rebound or extended
                // as a prefix itself.
                if self.event_matches_help_char(&emacs_event)
                    && self
                        .command_loop
                        .keyboard
                        .kboard
                        .current_key_sequence
                        .raw_events()
                        .len()
                        > 1
                {
                    if let Some(result) =
                        self.dispatch_prefix_help_command(&mut delayed_selection_event)?
                    {
                        self.restore_key_sequence_current_buffer(&mut saved_current_buffer);
                        return Ok(result);
                    }
                }

                // Keyboard audit Finding 10: input-method-function.
                // GNU `keyboard.c:2707-2770` in `read_char`: when
                // we just read a printable character at the start
                // of a key sequence and `input-method-function`
                // is bound, call it to let quail/robin/etc.
                // translate the character into a (possibly empty)
                // list of events. The translated events are
                // re-queued via `unread-command-events` and the
                // read_key_sequence loop re-reads from the top.
                //
                // We only fire at sequence start — GNU gates on
                // `!current_kboard->immediate_echo` which is
                // effectively the same condition, because a
                // key-sequence in progress has already been
                // echoed. Mid-sequence characters go straight
                // through to keymap lookup untranslated.
                if self
                    .command_loop
                    .keyboard
                    .kboard
                    .current_key_sequence
                    .raw_events()
                    .len()
                    == 1
                    && self.maybe_apply_input_method_function(emacs_event)?
                {
                    self.command_loop.reset_key_sequence();
                    replay_current_sequence = false;
                    shift_translation = None;
                    continue;
                }
            }

            let translation = match self.apply_translation_maps_to_current_key_sequence(options) {
                Ok(translation) => translation,
                Err(err) => {
                    self.restore_delayed_selection_event(&mut delayed_selection_event);
                    return Err(err);
                }
            };
            let mut translated_events = translation.translated_events;

            if self
                .command_loop
                .keyboard
                .kboard
                .current_key_sequence
                .raw_events()
                .len()
                == 1
                && let Some(prefixed) = Self::maybe_prefix_mouse_area(&translated_events)
            {
                self.command_loop
                    .keyboard
                    .rewrite_key_sequence_translation(prefixed.clone());
                translated_events = prefixed;
            }
            if self
                .command_loop
                .keyboard
                .kboard
                .current_key_sequence
                .raw_events()
                .len()
                == 1
                && saved_current_buffer.is_none()
                && let Some(target_buffer_id) =
                    Self::key_sequence_target_buffer_id(&translated_events, &self.frames)
                && self.buffers.current_buffer_id() != Some(target_buffer_id)
            {
                saved_current_buffer = self.buffers.current_buffer_id();
                if let Err(err) = self.switch_current_buffer(target_buffer_id) {
                    self.restore_delayed_selection_event(&mut delayed_selection_event);
                    self.restore_key_sequence_current_buffer(&mut saved_current_buffer);
                    return Err(err);
                }
            }
            let lookup_position = Self::key_sequence_lookup_position(&translated_events);

            tracing::debug!(
                "read_key_sequence: looking up binding for {:?}",
                translated_events
                    .iter()
                    .map(crate::emacs_core::print::print_value)
                    .collect::<Vec<_>>()
            );
            let mut resolved = match resolve_active_key_binding(
                self,
                &translated_events,
                false,
                false,
                lookup_position.as_ref(),
            ) {
                Ok(resolved) => resolved,
                Err(err) => {
                    self.restore_delayed_selection_event(&mut delayed_selection_event);
                    self.restore_key_sequence_current_buffer(&mut saved_current_buffer);
                    return Err(err);
                }
            };
            let mut binding = resolved.binding;
            let mut lookup_is_undefined =
                resolved.lookup.is_nil() || resolved.lookup == Value::symbol("undefined");
            let mut binding_is_undefined =
                binding.is_nil() || binding == Value::symbol("undefined");
            let mut sequence_is_undefined = lookup_is_undefined || binding_is_undefined;
            tracing::debug!(
                "read_key_sequence: binding={}",
                crate::emacs_core::print::print_value(&binding)
            );

            if sequence_is_undefined {
                if translation.has_pending_translation_prefix {
                    tracing::debug!(
                        "read_key_sequence: continuing because translation suffix is still a prefix"
                    );
                    continue;
                }
                if let Some(fallback) = self.resolve_undefined_mouse_sequence_fallback(
                    &translated_events,
                    lookup_position.as_ref(),
                )? {
                    match fallback {
                        UndefinedMouseSequenceFallback::Rewrite {
                            events,
                            resolved: rewritten_resolution,
                        } => {
                            tracing::debug!(
                                "read_key_sequence: simplifying undefined mouse event to {:?}",
                                events
                                    .iter()
                                    .map(crate::emacs_core::print::print_value)
                                    .collect::<Vec<_>>()
                            );
                            self.command_loop
                                .keyboard
                                .rewrite_key_sequence_translation(events.clone());
                            translated_events = events;
                            resolved = rewritten_resolution;
                            binding = resolved.binding;
                            lookup_is_undefined = resolved.lookup.is_nil()
                                || resolved.lookup == Value::symbol("undefined");
                            binding_is_undefined =
                                binding.is_nil() || binding == Value::symbol("undefined");
                            sequence_is_undefined = lookup_is_undefined || binding_is_undefined;
                        }
                        UndefinedMouseSequenceFallback::Drop { retained_events } => {
                            tracing::debug!(
                                "read_key_sequence: dropping undefined mouse event and retaining {:?}",
                                retained_events
                                    .iter()
                                    .map(crate::emacs_core::print::print_value)
                                    .collect::<Vec<_>>()
                            );
                            self.command_loop
                                .keyboard
                                .rewrite_key_sequence_translation(retained_events);
                            shift_translation = None;
                            continue;
                        }
                    }
                }
                if sequence_is_undefined {
                    if self.translate_upper_case_key_bindings_enabled()
                        && let Some(applied_shift_translation) =
                            self.apply_shift_translation_to_current_key_sequence()
                    {
                        shift_translation = Some(applied_shift_translation);
                        replay_current_sequence = true;
                        tracing::debug!(
                            "read_key_sequence: replaying after shift/downcase translation"
                        );
                        continue;
                    }
                    self.finalize_shift_translated_key_sequence(
                        sequence_is_undefined,
                        options,
                        shift_translation.take(),
                    );
                    self.restore_delayed_selection_event(&mut delayed_selection_event);
                    self.restore_key_sequence_current_buffer(&mut saved_current_buffer);
                    let (translated, raw) = self.command_loop.keyboard.key_sequence_snapshot();
                    self.command_loop
                        .set_command_key_sequences(translated.clone(), raw);
                    return Ok((translated, binding));
                }
            }

            let is_prefix =
                resolve_prefix_keymap_binding_in_obarray(&self.obarray, &resolved.lookup).is_some();

            if is_prefix {
                // Keyboard audit Finding 6: consult `echo-keystrokes`.
                // GNU `keyboard.c:2769,10206,10261` only echoes the
                // pending prefix when `echo-keystrokes` is a
                // positive number; a zero/nil value suppresses the
                // echo entirely so fast typists never see a
                // mid-sequence flash.
                //
                // Note: full GNU parity also *delays* the echo by
                // `echo-keystrokes` seconds of no further input.
                // neomacs currently echoes immediately when the
                // variable is positive. The delay path is tracked
                // separately as future work — the present fix
                // corrects the "echo regardless of
                // echo-keystrokes" bug but leaves the deadline
                // scheduler for a later pass.
                if self.lisp_echo_keystrokes_seconds().is_some_and(|s| s > 0.0) {
                    let key_vec = Value::vector(translated_events.clone());
                    if let Ok(desc) =
                        crate::emacs_core::builtins::keymaps::builtin_key_description(vec![key_vec])
                    {
                        if let Some(s) = desc.as_utf8_str() {
                            let echo_msg = format!("{}-", s);
                            let _ = crate::emacs_core::builtins::dispatch_builtin(
                                self,
                                "message",
                                vec![Value::string(echo_msg)],
                            );
                        }
                    }
                }
                continue;
            }

            self.finalize_shift_translated_key_sequence(
                sequence_is_undefined,
                options,
                shift_translation.take(),
            );
            self.restore_delayed_selection_event(&mut delayed_selection_event);
            self.restore_key_sequence_current_buffer(&mut saved_current_buffer);
            let (translated, raw) = self.command_loop.keyboard.key_sequence_snapshot();
            self.command_loop
                .set_command_key_sequences(translated.clone(), raw);
            return Ok((translated, binding));
        }
    }

    /// Read a single input event, blocking if necessary.
    ///
    /// Mirrors GNU Emacs `read_char()` (keyboard.c:2489).
    /// This is THE blocking point in the command loop.
    /// Before blocking, triggers redisplay.
    fn drain_ready_input_event_for_read_char(&mut self) -> Option<InputEvent> {
        if let Some(event) = self.command_loop.keyboard.pending_input_events.pop_front() {
            self.timer_stop_idle();
            return Some(event);
        }

        let Some(ref rx) = self.input_rx else {
            return None;
        };
        match rx.try_recv() {
            Ok(event) => {
                self.timer_stop_idle();
                Some(event)
            }
            Err(crossbeam_channel::TryRecvError::Empty) => None,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                self.handle_display_terminal_disconnect();
                None
            }
        }
    }

    fn handle_display_terminal_disconnect(&mut self) {
        self.input_rx = None;
        let _ = crate::emacs_core::terminal::pure::delete_terminal_noelisp_owned(
            self,
            crate::emacs_core::terminal::pure::TERMINAL_ID,
        );
        self.request_shutdown(0, false);
    }

    fn handle_read_char_input_event(
        &mut self,
        event: InputEvent,
    ) -> Result<Option<Value>, crate::emacs_core::error::Flow> {
        if input_event_triggers_throw_on_input(&event)
            && self.interrupt_for_input_event_if_requested(event.clone())?
        {
            return Ok(None);
        }

        match event {
            InputEvent::WindowClose { emacs_frame_id } => {
                self.handle_window_close_input_event(emacs_frame_id)?;
                Ok(None)
            }
            InputEvent::Resize {
                width,
                height,
                emacs_frame_id,
            } => {
                self.apply_resize_input_event(width, height, emacs_frame_id, true);
                self.redisplay();
                self.timer_resume_idle();
                Ok(None)
            }
            InputEvent::Focus {
                focused,
                emacs_frame_id,
            } => {
                self.timer_resume_idle();
                if let Some(event) = self.make_lispy_focus_event(focused, emacs_frame_id) {
                    if self.execute_special_event_if_bound(event)? {
                        return Ok(None);
                    }
                    if focused {
                        // GNU `frame.el` routes `handle-focus-in` through the
                        // C primitive `internal-handle-focus-in`. In source
                        // bootstrap contexts that Lisp wrapper is not loaded
                        // yet, but focus-in still should not surface as a
                        // user event and must preserve the switch-frame side
                        // effect for other frames.
                        crate::emacs_core::builtins::symbols::builtin_internal_handle_focus_in(
                            self,
                            vec![event],
                        )?;
                    }
                    return Ok(None);
                }
                Ok(None)
            }
            InputEvent::MonitorsChanged { monitors } => {
                self.timer_resume_idle();
                crate::emacs_core::builtins::set_neomacs_monitor_info(monitors);
                let hook_sym = crate::emacs_core::hook_runtime::hook_symbol_by_name(
                    self,
                    "display-monitors-changed-functions",
                );
                let terminal = crate::emacs_core::terminal::pure::terminal_handle_value();
                let _ =
                    crate::emacs_core::hook_runtime::run_named_hook(self, hook_sym, &[terminal])?;
                Ok(None)
            }
            InputEvent::SelectWindow { window_id } => {
                self.timer_resume_idle();
                Ok(self.make_lispy_select_window_event(window_id))
            }
            InputEvent::KeyPress {
                ref key,
                emacs_frame_id,
            } => {
                self.sync_keyboard_terminal_owner_for_input_frame(emacs_frame_id);
                tracing::debug!("read_char: received KeyPress {:?}", key);
                self.clear_current_message();
                let emacs_event = key.to_emacs_event_value();
                if self.event_is_quit_char(&emacs_event) {
                    self.request_quit_from_keyboard_input();
                }
                self.command_loop.store_kbd_macro_event(emacs_event);
                Ok(Some(emacs_event))
            }
            InputEvent::MousePress {
                button,
                x,
                y,
                modifiers,
                target_frame_id,
            } => {
                self.clear_current_message();
                // Keyboard audit Finding 12: compute the click
                // count for this press based on the previous
                // click state and update `last_mouse_click` so
                // the matching release can read the same count.
                // Mirrors GNU `keyboard.c:6041-6130`.
                let click_count = self.classify_mouse_click_on_press(button, x, y, target_frame_id);
                let prefix = Self::mouse_event_prefix_for_click_count("down-mouse", click_count);
                let event = Self::make_mouse_event(
                    &button,
                    x,
                    y,
                    target_frame_id,
                    &modifiers,
                    &prefix,
                    self,
                );
                self.command_loop.store_kbd_macro_event(event);
                Ok(Some(event))
            }
            InputEvent::MouseRelease {
                button,
                x,
                y,
                target_frame_id,
            } => {
                self.clear_current_message();
                // Use the click count recorded on the matching
                // press so the release event carries the same
                // double/triple modifier. Keyboard audit F12.
                let click_count = self
                    .command_loop
                    .keyboard
                    .kboard
                    .last_mouse_click
                    .filter(|state| state.button == button)
                    .map(|state| state.click_count)
                    .unwrap_or(1);
                let prefix = Self::mouse_event_prefix_for_click_count("mouse", click_count);
                let event = Self::make_mouse_event(
                    &button,
                    x,
                    y,
                    target_frame_id,
                    &Modifiers::none(),
                    &prefix,
                    self,
                );
                self.command_loop.store_kbd_macro_event(event);
                Ok(Some(event))
            }
            InputEvent::MouseScroll {
                delta_x: _,
                delta_y,
                x,
                y,
                modifiers,
                target_frame_id,
            } => {
                let dir = if delta_y > 0.0 {
                    "wheel-up"
                } else {
                    "wheel-down"
                };
                let mut sym = String::new();
                Self::append_modifier_prefix(&modifiers, &mut sym);
                sym.push_str(dir);
                let position = Self::make_mouse_position(x, y, target_frame_id, self);
                let event = Value::list(vec![Value::symbol(&sym), position]);
                self.command_loop.store_kbd_macro_event(event);
                Ok(Some(event))
            }
            InputEvent::MouseMove {
                x,
                y,
                modifiers,
                target_frame_id,
            } => {
                self.note_mouse_move_input_event(x, y, target_frame_id);
                self.timer_resume_idle();
                if !self.track_mouse_enabled() {
                    return Ok(None);
                }
                self.clear_current_message();
                let mut sym = String::new();
                Self::append_modifier_prefix(&modifiers, &mut sym);
                sym.push_str("mouse-movement");
                let position = Self::make_mouse_position(x, y, target_frame_id, self);
                let event = Value::list(vec![Value::symbol(&sym), position]);
                self.command_loop.store_kbd_macro_event(event);
                Ok(Some(event))
            }
        }
    }

    pub(crate) fn read_char_with_timeout(
        &mut self,
        timeout: Option<std::time::Duration>,
    ) -> Result<Option<Value>, crate::emacs_core::error::Flow> {
        let deadline = timeout.map(|timeout| std::time::Instant::now() + timeout);

        loop {
            self.sync_keyboard_terminal_owner();
            if let Some(event) = self.command_loop.keyboard.take_unread_selection_event() {
                return Ok(Some(event));
            }

            if let Some(event) = self.command_loop.keyboard.pop_unread_event() {
                if self.handle_help_echo_event(event)? {
                    continue;
                }
                if self.execute_special_event_if_bound(event)? {
                    continue;
                }
                return Ok(Some(event));
            }

            if let Some(event) = self.command_loop.keyboard.next_executing_kbd_macro_event() {
                self.assign(
                    "executing-kbd-macro-index",
                    Value::fixnum(self.command_loop.keyboard.kboard.kbd_macro_index as i64),
                );
                return Ok(Some(event));
            }

            if self.sync_pending_resize_events() {
                self.redisplay();
            }
            if let Some(event) = self.drain_ready_input_event_for_read_char() {
                if let Some(value) = self.handle_read_char_input_event(event)? {
                    return Ok(Some(value));
                }
                continue;
            }
            if self.shutdown_request.is_some() {
                return Err(crate::emacs_core::error::signal("quit", vec![]));
            }

            if self.noninteractive() && self.input_rx.is_none() {
                self.timer_stop_idle();
                return Ok(None);
            }

            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                self.timer_stop_idle();
                return Ok(None);
            }

            self.redisplay();
            let _ = self.service_wait_path_once(None, false, true, true)?;

            tracing::debug!(
                "read_char: blocking on input (input_rx={})...",
                self.input_rx.is_some()
            );

            if self.sync_pending_resize_events() {
                self.redisplay();
            }

            if let Some(event) = self.drain_ready_input_event_for_read_char() {
                if let Some(value) = self.handle_read_char_input_event(event)? {
                    return Ok(Some(value));
                }
                continue;
            }
            if self.shutdown_request.is_some() {
                return Err(crate::emacs_core::error::signal("quit", vec![]));
            }

            let rx = match self.input_rx {
                Some(ref rx) => rx.clone(),
                None => {
                    if !self.command_loop.running {
                        return Err(crate::emacs_core::error::signal("quit", vec![]));
                    }
                    tracing::debug!("read_char: no input_rx (batch mode), returning Nil");
                    return Ok(Some(Value::NIL));
                }
            };

            self.timer_start_idle();
            let mut timeout = self.next_input_wait_timeout();
            if let Some(deadline) = deadline {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    self.timer_stop_idle();
                    return Ok(None);
                }
                timeout = Some(timeout.map_or(remaining, |current| current.min(remaining)));
            }
            self.waiting_for_user_input = true;
            let wait_result = if let Some(timeout) = timeout {
                rx.recv_timeout(timeout)
            } else {
                rx.recv()
                    .map_err(|_| crossbeam_channel::RecvTimeoutError::Disconnected)
            };
            self.waiting_for_user_input = false;

            match wait_result {
                Ok(event) => {
                    self.timer_stop_idle();
                    if let Some(value) = self.handle_read_char_input_event(event)? {
                        return Ok(Some(value));
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                        self.timer_stop_idle();
                        return Ok(None);
                    }
                    let _ = self.service_wait_path_once(None, false, true, true)?;
                    continue;
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    self.handle_display_terminal_disconnect();
                    return Err(crate::emacs_core::error::signal("quit", vec![]));
                }
            }
        }
    }

    pub(crate) fn read_char(&mut self) -> Result<Value, crate::emacs_core::error::Flow> {
        Ok(self.read_char_with_timeout(None)?.unwrap_or(Value::NIL))
    }

    fn resolve_input_frame_id(&self, emacs_frame_id: u64) -> Option<crate::window::FrameId> {
        if emacs_frame_id == 0 {
            self.frames.selected_frame().map(|frame| frame.id)
        } else {
            let frame_id = crate::window::FrameId(emacs_frame_id);
            self.frames.get(frame_id).map(|frame| frame.id)
        }
    }

    fn record_mouse_pixel_position(
        &mut self,
        frame_id: Option<crate::window::FrameId>,
        x: i64,
        y: i64,
    ) {
        self.command_loop
            .keyboard
            .set_mouse_pixel_position(frame_id, x, y);
    }

    fn make_help_echo_event(
        frame: Value,
        help: Value,
        window: Value,
        object: Value,
        pos: Value,
    ) -> Value {
        Value::list(vec![
            Value::symbol("help-echo"),
            frame,
            help,
            window,
            object,
            pos,
        ])
    }

    fn resolve_text_area_help_echo_event(
        &mut self,
        frame_id: crate::window::FrameId,
        x: i64,
        y: i64,
    ) -> Option<Value> {
        let frame = self.frames.get(frame_id)?;
        let window_id = frame.window_at(x as f32, y as f32)?;
        let snapshot = frame.window_display_snapshot(window_id)?;
        let window = frame.find_window(window_id)?;
        let buffer_id = window.buffer_id()?;
        let bounds = window.bounds();
        let query_x = x - bounds.x.round() as i64 - snapshot.text_area_left_offset;
        let query_y = y - bounds.y.round() as i64;
        let point = snapshot.point_at_coords(query_x, query_y)?;

        let pair = crate::emacs_core::textprop::builtin_get_char_property_and_overlay_in_state(
            &self.obarray,
            &self.buffers,
            vec![
                Value::fixnum(point.buffer_pos as i64),
                Value::symbol("help-echo"),
                Value::make_buffer(buffer_id),
            ],
        )
        .ok()?;
        if !pair.is_cons() {
            return None;
        };
        let pair_car = pair.cons_car();
        let pair_cdr = pair.cons_cdr();
        if pair_car.is_nil() {
            return None;
        }
        let object = if pair_cdr.is_nil() {
            Value::make_buffer(buffer_id)
        } else {
            pair_cdr
        };
        Some(Self::make_help_echo_event(
            Value::make_frame(frame_id.0),
            pair_car,
            Value::make_window(window_id.0),
            object,
            Value::fixnum(point.buffer_pos as i64),
        ))
    }

    fn queue_mouse_help_echo_update(
        &mut self,
        frame_id: Option<crate::window::FrameId>,
        x: i64,
        y: i64,
    ) {
        let next = frame_id.and_then(|fid| self.resolve_text_area_help_echo_event(fid, x, y));
        let previous = self.command_loop.keyboard.kboard.last_help_echo_event;
        let changed = match (previous, next) {
            (Some(prev), Some(next)) => !crate::emacs_core::value::equal_value(&prev, &next, 0),
            (None, None) => false,
            _ => true,
        };
        if !changed {
            return;
        }

        match next {
            Some(event) => {
                self.command_loop.keyboard.unread_event(event);
                self.command_loop.keyboard.kboard.last_help_echo_event = Some(event);
            }
            None => {
                if let Some(fid) = frame_id {
                    self.command_loop
                        .keyboard
                        .unread_event(Self::make_help_echo_event(
                            Value::make_frame(fid.0),
                            Value::NIL,
                            Value::NIL,
                            Value::NIL,
                            Value::fixnum(0),
                        ));
                }
                self.command_loop.keyboard.kboard.last_help_echo_event = None;
            }
        }
    }

    pub(crate) fn note_mouse_move_for_frame(
        &mut self,
        frame_id: Option<crate::window::FrameId>,
        x: i64,
        y: i64,
    ) {
        self.record_mouse_pixel_position(frame_id, x, y);
        self.queue_mouse_help_echo_update(frame_id, x, y);
    }

    fn note_mouse_move_input_event(&mut self, x: f32, y: f32, target_frame_id: u64) {
        self.note_mouse_move_for_frame(
            self.resolve_input_frame_id(target_frame_id),
            x.round() as i64,
            y.round() as i64,
        );
    }

    fn handle_help_echo_event(
        &mut self,
        event: Value,
    ) -> Result<bool, crate::emacs_core::error::Flow> {
        let Some(parts) = crate::emacs_core::value::list_to_vec(&event) else {
            return Ok(false);
        };
        if parts.len() != 6 {
            return Ok(false);
        }
        let head = parts[0];
        let mut help = parts[2];
        let window = parts[3];
        let object = parts[4];
        let pos = parts[5];
        if !head.is_symbol_named("help-echo") {
            return Ok(false);
        }

        if !help.is_nil() && !help.is_string() {
            help = if self.function_value_is_callable(&help) {
                self.funcall_general(help, vec![window, object, pos])?
            } else {
                self.eval_value(&help)?
            };
            if !help.is_nil() && !help.is_string() {
                return Ok(true);
            }
        }

        help = self.fixup_help_echo_message(help)?;

        if help.is_string() {
            help = self.substitute_help_echo_command_keys(help)?;
        }

        let show_help_function = self
            .obarray
            .symbol_value("show-help-function")
            .copied()
            .unwrap_or(Value::NIL);
        if self.function_value_is_callable(&show_help_function) {
            let _ = self.funcall_general(show_help_function, vec![help])?;
        } else if let Some(message) = help.as_lisp_string() {
            self.set_current_message(Some(message.clone()));
            self.redisplay();
        } else {
            self.clear_current_message();
        }

        self.timer_resume_idle();
        Ok(true)
    }

    fn fixup_help_echo_message(
        &mut self,
        help: Value,
    ) -> Result<Value, crate::emacs_core::error::Flow> {
        // GNU `show_help_echo` applies `mouse-fixup-help-message` whenever the
        // resolved help text is a string; it is not conditional on whether the
        // current runtime is actively polling host input.
        if help.as_utf8_str().is_none() {
            return Ok(help);
        }

        match self.obarray.symbol_function("mouse-fixup-help-message") {
            Some(function) if !crate::emacs_core::autoload::is_autoload_value(&function) => {
                self.funcall_general(Value::symbol("mouse-fixup-help-message"), vec![help])
            }
            _ => Ok(help),
        }
    }

    fn substitute_help_echo_command_keys(
        &mut self,
        help: Value,
    ) -> Result<Value, crate::emacs_core::error::Flow> {
        if help.is_nil() || help.as_utf8_str().is_none() {
            return Ok(help);
        }

        let inhibit = crate::emacs_core::textprop::builtin_get_text_property_in_state(
            &self.obarray,
            &self.buffers,
            vec![
                Value::fixnum(1),
                Value::symbol("help-echo-inhibit-substitution"),
                help,
            ],
        )?;
        if inhibit.is_truthy() {
            return Ok(help);
        }

        match self.obarray.symbol_function("substitute-command-keys") {
            Some(function) if !crate::emacs_core::autoload::is_autoload_value(&function) => {
                self.funcall_general(Value::symbol("substitute-command-keys"), vec![help])
            }
            _ => Ok(help),
        }
    }

    /// Build an Emacs mouse event value.
    ///
    /// Returns `(EVENT-SYMBOL POSITION)` where EVENT-SYMBOL is e.g.
    /// `mouse-1`, `down-mouse-2`, `C-mouse-1`, etc.
    /// Compute the click-count for a just-received
    /// MousePress event and update `KBoard.last_mouse_click`.
    /// Returns the 1-based click count (1 = single, 2 = double,
    /// 3 = triple). Subsequent clicks beyond triple saturate at
    /// 3, matching GNU `keyboard.c:6120-6128` where the count
    /// wraps via `min (3, ...)`. Keyboard audit Finding 12.
    fn classify_mouse_click_on_press(
        &mut self,
        button: MouseButton,
        x: f32,
        y: f32,
        frame_id: u64,
    ) -> u32 {
        let now = std::time::Instant::now();
        let double_click_time_ms = self
            .eval_symbol("double-click-time")
            .ok()
            .and_then(|v| v.as_fixnum())
            .filter(|&n| n > 0)
            .map(|n| n as u64)
            .unwrap_or(500);
        let double_click_fuzz = self
            .eval_symbol("double-click-fuzz")
            .ok()
            .and_then(|v| v.as_fixnum())
            .filter(|&n| n >= 0)
            .map(|n| n as f32)
            .unwrap_or(3.0);

        let count = match self.command_loop.keyboard.kboard.last_mouse_click {
            Some(prev)
                if prev.button == button
                    && prev.frame_id == frame_id
                    && (prev.x - x).abs() <= double_click_fuzz
                    && (prev.y - y).abs() <= double_click_fuzz
                    && now.saturating_duration_since(prev.timestamp).as_millis()
                        <= double_click_time_ms as u128 =>
            {
                (prev.click_count + 1).min(3)
            }
            _ => 1,
        };

        self.command_loop.keyboard.kboard.last_mouse_click = Some(LastMouseClick {
            button,
            x,
            y,
            frame_id,
            timestamp: now,
            click_count: count,
        });
        count
    }

    /// Build the event symbol prefix for a mouse click, taking
    /// the click count into account. `base` is either
    /// `down-mouse` (for presses) or `mouse` (for releases).
    /// For `count == 1`, returns `base` unchanged. For 2, prefix
    /// with `double-`; for 3, `triple-`. Keyboard audit Finding
    /// 12.
    fn mouse_event_prefix_for_click_count(base: &str, count: u32) -> String {
        match count {
            0 | 1 => base.to_string(),
            2 => format!("double-{}", base),
            _ => format!("triple-{}", base),
        }
    }

    pub(crate) fn make_mouse_event(
        button: &MouseButton,
        x: f32,
        y: f32,
        target_frame_id: u64,
        modifiers: &Modifiers,
        prefix: &str,
        eval: &Self,
    ) -> Value {
        let button_num = match button {
            MouseButton::Left => 1,
            MouseButton::Middle => 2,
            MouseButton::Right => 3,
            MouseButton::Button4 => 4,
            MouseButton::Button5 => 5,
        };
        let mut sym = String::new();
        Self::append_modifier_prefix(modifiers, &mut sym);
        sym.push_str(&format!("{}-{}", prefix, button_num));

        let position = Self::make_mouse_position(x, y, target_frame_id, eval);
        Value::list(vec![Value::symbol(&sym), position])
    }

    fn event_position(event: &Value) -> Option<Value> {
        let event_slots = crate::emacs_core::value::list_to_vec(event)?;
        let position = *event_slots.get(1)?;
        let position_slots = crate::emacs_core::value::list_to_vec(&position)?;
        (position_slots.len() >= 4).then_some(position)
    }

    fn key_sequence_lookup_position(events: &[Value]) -> Option<Value> {
        events.iter().find_map(Self::event_position)
    }

    fn key_sequence_target_buffer_id(
        events: &[Value],
        frames: &crate::window::FrameManager,
    ) -> Option<crate::buffer::BufferId> {
        let position = Self::key_sequence_lookup_position(events)?;
        let slots = crate::emacs_core::value::list_to_vec(&position)?;
        let first = *slots.first()?;
        let wid = first.as_window_id()?;
        let window_id = crate::window::WindowId(wid);

        for frame_id in frames.frame_list() {
            let Some(frame) = frames.get(frame_id) else {
                continue;
            };
            let Some(window) = frame.find_window(window_id) else {
                continue;
            };
            if let Some(buffer_id) = window.buffer_id() {
                return Some(buffer_id);
            }
        }

        None
    }

    fn event_position_area(position: &Value) -> Option<Value> {
        let slots = crate::emacs_core::value::list_to_vec(position)?;
        let area_or_pos = *slots.get(1)?;
        match area_or_pos.kind() {
            ValueKind::Symbol(_) => Some(area_or_pos),
            ValueKind::Cons => {
                let head = area_or_pos.cons_car();
                head.as_symbol_name().map(|_| head)
            }
            _ => None,
        }
    }

    fn maybe_prefix_mouse_area(events: &[Value]) -> Option<Vec<Value>> {
        let first = events.first()?;
        let position = Self::event_position(first)?;
        let area = Self::event_position_area(&position)?;
        let mut prefixed = Vec::with_capacity(events.len() + 1);
        prefixed.push(area);
        prefixed.extend_from_slice(events);
        Some(prefixed)
    }

    fn split_event_symbol_modifiers(mut name: &str) -> (String, &str) {
        let mut prefix = String::new();
        let is_single_char = |s: &str| {
            let mut chars = s.chars();
            chars.next().is_some() && chars.next().is_none()
        };

        loop {
            if let Some(rest) = name.strip_prefix("C-") {
                if is_single_char(rest) {
                    break;
                }
                prefix.push_str("C-");
                name = rest;
                continue;
            }
            if let Some(rest) = name.strip_prefix("M-") {
                if is_single_char(rest) {
                    break;
                }
                prefix.push_str("M-");
                name = rest;
                continue;
            }
            if let Some(rest) = name.strip_prefix("S-") {
                if is_single_char(rest) {
                    break;
                }
                prefix.push_str("S-");
                name = rest;
                continue;
            }
            if let Some(rest) = name.strip_prefix("s-") {
                if is_single_char(rest) {
                    break;
                }
                prefix.push_str("s-");
                name = rest;
                continue;
            }
            if let Some(rest) = name.strip_prefix("H-") {
                if is_single_char(rest) {
                    break;
                }
                prefix.push_str("H-");
                name = rest;
                continue;
            }
            if let Some(rest) = name.strip_prefix("A-") {
                if is_single_char(rest) {
                    break;
                }
                prefix.push_str("A-");
                name = rest;
                continue;
            }
            break;
        }

        (prefix, name)
    }

    fn mouse_event_symbol_name(event: &Value) -> Option<String> {
        match event.kind() {
            ValueKind::Symbol(_) => event.as_symbol_name().map(str::to_owned),
            ValueKind::Cons => {
                let slots = crate::emacs_core::value::list_to_vec(event)?;
                slots.first()?.as_symbol_name().map(str::to_owned)
            }
            _ => None,
        }
    }

    fn rewrite_mouse_event_symbol(event: Value, symbol_name: &str) -> Option<Value> {
        match event.kind() {
            ValueKind::Symbol(_) => Some(Value::symbol(symbol_name)),
            ValueKind::Cons => {
                let mut slots = crate::emacs_core::value::list_to_vec(&event)?;
                let head = slots.first_mut()?;
                if head.as_symbol_name().is_none() {
                    return None;
                }
                *head = Value::symbol(symbol_name);
                Some(Value::list(slots))
            }
            _ => None,
        }
    }

    fn simplify_mouse_event_once(event: Value) -> Option<(MouseEventFallbackStep, Option<Value>)> {
        let symbol_name = Self::mouse_event_symbol_name(&event)?;
        let (modifier_prefix, base) = Self::split_event_symbol_modifiers(&symbol_name);
        let mut rewritten_name = modifier_prefix;

        let rewritten_base = if let Some(rest) = base.strip_prefix("triple-") {
            rewritten_name.push_str("double-");
            rewritten_name.push_str(rest);
            return Some((
                MouseEventFallbackStep::Rewrite,
                Self::rewrite_mouse_event_symbol(event, &rewritten_name),
            ));
        } else if let Some(rest) = base.strip_prefix("double-") {
            rest
        } else if let Some(rest) = base.strip_prefix("drag-") {
            rest
        } else if base.starts_with("down-") || base.starts_with("up-") {
            return Some((MouseEventFallbackStep::Drop, None));
        } else {
            return None;
        };

        rewritten_name.push_str(rewritten_base);
        Some((
            MouseEventFallbackStep::Rewrite,
            Self::rewrite_mouse_event_symbol(event, &rewritten_name),
        ))
    }

    fn retained_key_sequence_len_after_mouse_drop(events: &[Value]) -> usize {
        let Some(last_event) = events.last() else {
            return 0;
        };

        let mut retained_len = events.len().saturating_sub(1);
        if retained_len > 0
            && let Some(position) = Self::event_position(last_event)
            && let Some(area) = Self::event_position_area(&position)
            && events[retained_len - 1] == area
        {
            retained_len -= 1;
        }
        retained_len
    }

    fn resolve_undefined_mouse_sequence_fallback(
        &mut self,
        events: &[Value],
        lookup_position: Option<&Value>,
    ) -> Result<Option<UndefinedMouseSequenceFallback>, crate::emacs_core::error::Flow> {
        use crate::emacs_core::keymap::resolve_active_key_binding;

        let Some(last_index) = events.len().checked_sub(1) else {
            return Ok(None);
        };
        let mut rewritten_events = events.to_vec();

        loop {
            let Some((step, rewritten_event)) =
                Self::simplify_mouse_event_once(rewritten_events[last_index])
            else {
                return Ok(None);
            };

            match step {
                MouseEventFallbackStep::Rewrite => {
                    let Some(rewritten_event) = rewritten_event else {
                        return Ok(None);
                    };
                    rewritten_events[last_index] = rewritten_event;
                    let resolved = resolve_active_key_binding(
                        self,
                        &rewritten_events,
                        false,
                        false,
                        lookup_position,
                    )?;
                    let lookup_is_undefined =
                        resolved.lookup.is_nil() || resolved.lookup == Value::symbol("undefined");
                    let binding_is_undefined =
                        resolved.binding.is_nil() || resolved.binding == Value::symbol("undefined");
                    if !(lookup_is_undefined || binding_is_undefined) {
                        return Ok(Some(UndefinedMouseSequenceFallback::Rewrite {
                            events: rewritten_events,
                            resolved,
                        }));
                    }
                }
                MouseEventFallbackStep::Drop => {
                    let retained_len =
                        Self::retained_key_sequence_len_after_mouse_drop(&rewritten_events);
                    rewritten_events.truncate(retained_len);
                    return Ok(Some(UndefinedMouseSequenceFallback::Drop {
                        retained_events: rewritten_events,
                    }));
                }
            }
        }
    }

    fn window_point(window: &crate::window::Window) -> Option<i64> {
        match window {
            crate::window::Window::Leaf { point, .. } => Some(*point as i64),
            crate::window::Window::Internal { .. } => None,
        }
    }

    fn mouse_posn_descriptor_value(desc: MousePosnDescriptor) -> Value {
        let area_or_pos = match desc.area {
            Some(area) => Value::symbol(area),
            None => desc.metrics.point.map(Value::fixnum).unwrap_or(Value::NIL),
        };
        let pos = desc.metrics.point.map(Value::fixnum).unwrap_or(Value::NIL);
        let col_row = match (desc.metrics.col, desc.metrics.row) {
            (Some(col), Some(row)) => Value::cons(Value::fixnum(col), Value::fixnum(row)),
            _ => Value::NIL,
        };
        let width_height = match (desc.metrics.width, desc.metrics.height) {
            (Some(width), Some(height)) => Value::cons(Value::fixnum(width), Value::fixnum(height)),
            _ => Value::NIL,
        };

        Value::list(vec![
            desc.window_or_frame,
            area_or_pos,
            Value::cons(Value::fixnum(desc.x), Value::fixnum(desc.y)),
            Value::fixnum(0),
            Value::NIL,
            pos,
            col_row,
            Value::NIL,
            Value::cons(Value::fixnum(0), Value::fixnum(0)),
            width_height,
        ])
    }

    fn mouse_window_at(
        frame: &crate::window::Frame,
        x: f32,
        y: f32,
    ) -> Option<crate::window::WindowId> {
        if let Some(minibuffer) = frame.minibuffer_leaf.as_ref()
            && minibuffer.bounds().contains(x, y)
        {
            return Some(minibuffer.id());
        }
        frame.window_at(x, y)
    }

    fn make_mouse_position(x: f32, y: f32, target_frame_id: u64, eval: &Self) -> Value {
        let frame_id = if target_frame_id == 0 {
            match eval.frames.selected_frame() {
                Some(frame) => frame.id.0,
                None => 0,
            }
        } else {
            target_frame_id
        };

        let Some(frame) = (if frame_id == 0 {
            eval.frames.selected_frame()
        } else {
            eval.frames.get(crate::window::FrameId(frame_id))
        }) else {
            return Self::mouse_posn_descriptor_value(MousePosnDescriptor {
                window_or_frame: Value::NIL,
                area: None,
                x: x.round() as i64,
                y: y.round() as i64,
                metrics: MousePosnMetrics {
                    point: None,
                    col: None,
                    row: None,
                    width: None,
                    height: None,
                },
            });
        };

        let frame_x = x.round() as i64;
        let frame_y = y.round() as i64;
        let frame_height = frame.height as i64;
        let menu_bar_height = frame.menu_bar_height as i64;
        let tool_bar_height = frame.tool_bar_height as i64;
        let tab_bar_height = frame.tab_bar_height as i64;

        if menu_bar_height > 0 && frame_y < menu_bar_height {
            return Self::mouse_posn_descriptor_value(MousePosnDescriptor {
                window_or_frame: Value::make_frame(frame.id.0),
                area: Some("menu-bar"),
                x: frame_x,
                y: frame_y,
                metrics: MousePosnMetrics {
                    point: None,
                    col: None,
                    row: None,
                    width: None,
                    height: None,
                },
            });
        }
        if tool_bar_height > 0 && frame_y < menu_bar_height + tool_bar_height {
            return Self::mouse_posn_descriptor_value(MousePosnDescriptor {
                window_or_frame: Value::make_frame(frame.id.0),
                area: Some("tool-bar"),
                x: frame_x,
                y: frame_y - menu_bar_height,
                metrics: MousePosnMetrics {
                    point: None,
                    col: None,
                    row: None,
                    width: None,
                    height: None,
                },
            });
        }
        if tab_bar_height > 0 && frame_y < menu_bar_height + tool_bar_height + tab_bar_height {
            return Self::mouse_posn_descriptor_value(MousePosnDescriptor {
                window_or_frame: Value::make_frame(frame.id.0),
                area: Some("tab-bar"),
                x: frame_x,
                y: frame_y - menu_bar_height - tool_bar_height,
                metrics: MousePosnMetrics {
                    point: None,
                    col: None,
                    row: None,
                    width: None,
                    height: None,
                },
            });
        }

        let Some(window_id) = Self::mouse_window_at(frame, x, y) else {
            return Self::mouse_posn_descriptor_value(MousePosnDescriptor {
                window_or_frame: Value::make_frame(frame.id.0),
                area: None,
                x: frame_x,
                y: frame_y.min(frame_height.saturating_sub(1)),
                metrics: MousePosnMetrics {
                    point: None,
                    col: None,
                    row: None,
                    width: None,
                    height: None,
                },
            });
        };
        let Some(window) = frame.find_window(window_id) else {
            return Self::mouse_posn_descriptor_value(MousePosnDescriptor {
                window_or_frame: Value::make_frame(frame.id.0),
                area: None,
                x: frame_x,
                y: frame_y,
                metrics: MousePosnMetrics {
                    point: None,
                    col: None,
                    row: None,
                    width: None,
                    height: None,
                },
            });
        };

        let bounds = window.bounds();
        let window_x = (x - bounds.x).round() as i64;
        let window_y = (y - bounds.y).round() as i64;
        let fallback_metrics = MousePosnMetrics {
            point: Self::window_point(window),
            col: None,
            row: None,
            width: None,
            height: None,
        };

        if let Some(snapshot) = frame.window_display_snapshot(window_id) {
            let tab_line_bottom = snapshot.tab_line_height.max(0);
            let header_line_bottom =
                snapshot.tab_line_height.max(0) + snapshot.header_line_height.max(0);
            let mode_line_top = (bounds.height.round() as i64 - snapshot.mode_line_height.max(0))
                .max(header_line_bottom);

            if tab_line_bottom > 0 && window_y < tab_line_bottom {
                return Self::mouse_posn_descriptor_value(MousePosnDescriptor {
                    window_or_frame: Value::make_window(window_id.0),
                    area: Some("tab-line"),
                    x: window_x,
                    y: window_y,
                    metrics: fallback_metrics,
                });
            }
            if snapshot.header_line_height > 0 && window_y < header_line_bottom {
                return Self::mouse_posn_descriptor_value(MousePosnDescriptor {
                    window_or_frame: Value::make_window(window_id.0),
                    area: Some("header-line"),
                    x: window_x,
                    y: window_y,
                    metrics: fallback_metrics,
                });
            }
            if snapshot.mode_line_height > 0 && window_y >= mode_line_top {
                return Self::mouse_posn_descriptor_value(MousePosnDescriptor {
                    window_or_frame: Value::make_window(window_id.0),
                    area: Some("mode-line"),
                    x: window_x,
                    y: window_y,
                    metrics: fallback_metrics,
                });
            }

            let text_area_x = window_x - snapshot.text_area_left_offset;
            if let Some(point) = snapshot.point_at_coords(text_area_x, window_y) {
                return Self::mouse_posn_descriptor_value(MousePosnDescriptor {
                    window_or_frame: Value::make_window(window_id.0),
                    area: None,
                    x: text_area_x,
                    y: window_y,
                    metrics: MousePosnMetrics {
                        point: Some(point.buffer_pos as i64),
                        col: Some(point.col),
                        row: Some(point.row),
                        width: Some(point.width.max(1)),
                        height: Some(point.height.max(1)),
                    },
                });
            }

            if text_area_x < 0 {
                return Self::mouse_posn_descriptor_value(MousePosnDescriptor {
                    window_or_frame: Value::make_window(window_id.0),
                    area: Some("left-margin"),
                    x: window_x,
                    y: window_y,
                    metrics: fallback_metrics,
                });
            }
        }

        Self::mouse_posn_descriptor_value(MousePosnDescriptor {
            window_or_frame: Value::make_window(window_id.0),
            area: None,
            x: window_x,
            y: window_y,
            metrics: fallback_metrics,
        })
    }

    /// Append modifier prefix characters to a symbol name string.
    pub(crate) fn append_modifier_prefix(modifiers: &Modifiers, out: &mut String) {
        if modifiers.ctrl {
            out.push_str("C-");
        }
        if modifiers.meta {
            out.push_str("M-");
        }
        if modifiers.shift {
            out.push_str("S-");
        }
        if modifiers.super_ {
            out.push_str("s-");
        }
        if modifiers.hyper {
            out.push_str("H-");
        }
    }

    pub(crate) fn current_idle_duration(&self) -> Option<std::time::Duration> {
        self.command_loop
            .idle_start_time
            .map(|start| start.elapsed())
    }

    pub(crate) fn current_idle_time_value(&self) -> Value {
        let Some(idle_duration) = self.current_idle_duration() else {
            return Value::NIL;
        };
        let secs = idle_duration.as_secs() as i64;
        let usecs = idle_duration.subsec_micros() as i64;
        Value::list(vec![
            Value::fixnum((secs >> 16) & 0xFFFF_FFFF),
            Value::fixnum(secs & 0xFFFF),
            Value::fixnum(usecs),
            Value::fixnum(0),
        ])
    }

    pub(crate) fn timer_start_idle(&mut self) {
        if self.command_loop.idle_start_time.is_some() {
            return;
        }
        let now = std::time::Instant::now();
        self.command_loop.idle_start_time = Some(now);
        self.command_loop.last_idle_start_time = Some(now);

        if self.obarray.fboundp("internal-timer-start-idle") {
            if let Err(err) = self.apply(Value::symbol("internal-timer-start-idle"), vec![]) {
                tracing::warn!("internal-timer-start-idle failed: {:?}", err);
            }
        }
    }

    pub(crate) fn timer_stop_idle(&mut self) {
        if let Some(start) = self.command_loop.idle_start_time.take() {
            self.command_loop.last_idle_start_time = Some(start);
        }
    }

    pub(crate) fn timer_resume_idle(&mut self) {
        if self.command_loop.idle_start_time.is_none() {
            self.command_loop.idle_start_time = self.command_loop.last_idle_start_time;
        }
    }

    pub(crate) fn due_gnu_timers_snapshot(&self) -> Vec<Value> {
        let timers = self
            .obarray
            .symbol_value("timer-list")
            .and_then(crate::emacs_core::value::list_to_vec)
            .unwrap_or_default();
        let now = crate::emacs_core::eval::GnuTimerTimestamp::now();

        timers
            .into_iter()
            .filter_map(pending_gnu_timer_in_keyboard_runtime)
            .filter(|timer| timer.when <= now)
            .map(|timer| timer.timer)
            .collect()
    }

    pub(crate) fn due_gnu_idle_timers_snapshot(&self) -> Vec<Value> {
        let Some(idle_duration) = self.current_idle_duration() else {
            return Vec::new();
        };
        let idle_timers = self
            .obarray
            .symbol_value("timer-idle-list")
            .and_then(crate::emacs_core::value::list_to_vec)
            .unwrap_or_default();
        let now = crate::emacs_core::eval::GnuTimerTimestamp::from_duration(idle_duration);

        idle_timers
            .into_iter()
            .filter_map(pending_gnu_idle_timer_in_keyboard_runtime)
            .filter(|timer| timer.when <= now)
            .map(|timer| timer.timer)
            .collect()
    }

    pub(crate) fn next_due_gnu_timer_snapshot(&self) -> Option<Value> {
        let ordinary_now = crate::emacs_core::eval::GnuTimerTimestamp::now();
        let next_ordinary = self
            .obarray
            .symbol_value("timer-list")
            .and_then(crate::emacs_core::value::list_to_vec)
            .unwrap_or_default()
            .into_iter()
            .filter_map(pending_gnu_timer_in_keyboard_runtime)
            .find(|timer| timer.when <= ordinary_now);

        let next_idle = self
            .current_idle_duration()
            .map(crate::emacs_core::eval::GnuTimerTimestamp::from_duration)
            .and_then(|idle_now| {
                self.obarray
                    .symbol_value("timer-idle-list")
                    .and_then(crate::emacs_core::value::list_to_vec)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(pending_gnu_idle_timer_in_keyboard_runtime)
                    .find(|timer| timer.when <= idle_now)
                    .map(|timer| (timer, idle_now))
            });

        match (next_ordinary, next_idle) {
            (Some(ordinary), Some((idle, idle_now))) => {
                let ordinary_overdue = ordinary.when.overdue_duration(ordinary_now);
                let idle_overdue = idle.when.overdue_duration(idle_now);
                if ordinary_overdue > idle_overdue {
                    Some(ordinary.timer)
                } else {
                    Some(idle.timer)
                }
            }
            (Some(ordinary), None) => Some(ordinary.timer),
            (None, Some((idle, _))) => Some(idle.timer),
            (None, None) => None,
        }
    }

    pub(crate) fn next_ordinary_gnu_timer_timeout(&self) -> Option<std::time::Duration> {
        let timers = self
            .obarray
            .symbol_value("timer-list")
            .and_then(crate::emacs_core::value::list_to_vec)
            .unwrap_or_default();
        let now = crate::emacs_core::eval::GnuTimerTimestamp::now();

        timers
            .into_iter()
            .filter_map(pending_gnu_timer_in_keyboard_runtime)
            .map(|timer| timer.when.duration_until(now))
            .min()
    }

    pub(crate) fn next_idle_gnu_timer_timeout(&self) -> Option<std::time::Duration> {
        let Some(idle_duration) = self.current_idle_duration() else {
            return None;
        };
        let idle_timers = self
            .obarray
            .symbol_value("timer-idle-list")
            .and_then(crate::emacs_core::value::list_to_vec)
            .unwrap_or_default();
        let now = crate::emacs_core::eval::GnuTimerTimestamp::from_duration(idle_duration);

        idle_timers
            .into_iter()
            .filter_map(pending_gnu_idle_timer_in_keyboard_runtime)
            .map(|timer| timer.when.duration_until(now))
            .min()
    }

    pub(crate) fn next_input_wait_timeout(&self) -> Option<std::time::Duration> {
        let idle_dur = self.current_idle_duration();
        let mut timeout = self.timers.next_fire_time(idle_dur);

        if let Some(gnu_timeout) = self.next_ordinary_gnu_timer_timeout() {
            timeout = Some(timeout.map_or(gnu_timeout, |current| current.min(gnu_timeout)));
        }

        if let Some(idle_timeout) = self.next_idle_gnu_timer_timeout() {
            timeout = Some(timeout.map_or(idle_timeout, |current| current.min(idle_timeout)));
        }

        if !self.processes.live_process_ids().is_empty() {
            let process_poll = std::time::Duration::from_millis(100);
            timeout = Some(timeout.map_or(process_poll, |current| current.min(process_poll)));
        }

        timeout
    }

    pub(crate) fn fire_pending_timers(&mut self) {
        let _ = self.service_pending_timers_with_wait_policy(true);
    }

    pub(crate) fn poll_process_output(&mut self) {
        let _ = self.poll_process_output_with_wait_policy(None, false);
    }

    pub(crate) fn record_input_event(&mut self, event: Value) {
        self.assign("last-input-event", event);
        self.command_loop.record_input_event(event);
    }

    pub(crate) fn record_nonmenu_input_event(&mut self, event: Value) {
        self.assign("last-nonmenu-event", event);
    }

    pub(crate) fn recent_input_events(&self) -> &[Value] {
        self.command_loop.recent_input_events()
    }

    pub(crate) fn clear_recent_input_events(&mut self) {
        self.command_loop.clear_recent_input_events();
    }

    pub(crate) fn set_command_key_sequences(&mut self, translated: Vec<Value>, raw: Vec<Value>) {
        self.command_loop.set_command_key_sequences(translated, raw);
    }

    pub(crate) fn set_translated_command_keys(&mut self, keys: Vec<Value>) {
        self.command_loop.set_translated_command_keys(keys);
    }

    pub(crate) fn set_read_command_keys(&mut self, keys: Vec<Value>) {
        self.command_loop.set_read_command_keys(keys);
    }

    pub(crate) fn clear_read_command_keys(&mut self) {
        self.command_loop.clear_read_command_keys();
    }

    pub(crate) fn read_command_keys(&self) -> &[Value] {
        self.command_loop.read_command_keys()
    }

    pub(crate) fn read_raw_command_keys(&self) -> &[Value] {
        self.command_loop.read_raw_command_keys()
    }

    pub(crate) fn sync_keyboard_macro_runtime_vars(&mut self) {
        self.assign(
            "defining-kbd-macro",
            if self.command_loop.keyboard.kboard.defining_kbd_macro {
                if self.command_loop.keyboard.kboard.appending_kbd_macro {
                    Value::symbol("append")
                } else {
                    Value::T
                }
            } else {
                Value::NIL
            },
        );
        let last_kbd_macro = self
            .command_loop
            .last_kbd_macro()
            .map(|events| Value::vector(events.to_vec()))
            .unwrap_or(Value::NIL);
        self.assign("last-kbd-macro", last_kbd_macro);
        let executing_kbd_macro = self
            .command_loop
            .keyboard
            .kboard
            .executing_kbd_macro
            .as_ref()
            .map(|events| Value::vector(events.clone()))
            .unwrap_or(Value::NIL);
        self.assign("executing-kbd-macro", executing_kbd_macro);
        self.assign(
            "executing-kbd-macro-index",
            Value::fixnum(self.command_loop.keyboard.kboard.kbd_macro_index as i64),
        );
    }

    pub(crate) fn start_kbd_macro_runtime(
        &mut self,
        initial_events: Option<&[Value]>,
        append: bool,
    ) -> Result<(), crate::emacs_core::error::Flow> {
        if self.command_loop.keyboard.kboard.defining_kbd_macro {
            return Err(crate::emacs_core::error::signal(
                "error",
                vec![Value::string("Already defining a keyboard macro")],
            ));
        }
        self.command_loop
            .start_kbd_macro_with_initial(initial_events, append);
        self.sync_keyboard_macro_runtime_vars();
        Ok(())
    }

    pub(crate) fn store_kbd_macro_runtime_event(&mut self, event: Value) {
        self.command_loop.store_kbd_macro_event(event);
    }

    pub(crate) fn finalize_kbd_macro_runtime_chars(&mut self) {
        self.command_loop.finalize_kbd_macro_chars();
    }

    pub(crate) fn cancel_kbd_macro_runtime_events(&mut self) {
        self.command_loop.cancel_kbd_macro_events();
    }

    pub(crate) fn end_kbd_macro_runtime(
        &mut self,
    ) -> Result<Vec<Value>, crate::emacs_core::error::Flow> {
        if !self.command_loop.keyboard.kboard.defining_kbd_macro {
            return Err(crate::emacs_core::error::signal(
                "error",
                vec![Value::string("Not defining a keyboard macro")],
            ));
        }
        let previous = self
            .command_loop
            .last_kbd_macro()
            .map(|events| events.to_vec());
        let recorded = self.command_loop.end_kbd_macro();
        if let Some(previous) = previous {
            self.kmacro.macro_ring.push(previous);
        }
        self.sync_keyboard_macro_runtime_vars();
        Ok(recorded)
    }

    pub(crate) fn begin_executing_kbd_macro_runtime(&mut self, events: Vec<Value>) {
        self.command_loop.begin_executing_kbd_macro(events);
        self.sync_keyboard_macro_runtime_vars();
    }

    pub(crate) fn snapshot_executing_kbd_macro_runtime(&self) -> ExecutingKbdMacroRuntimeSnapshot {
        self.command_loop
            .keyboard
            .kboard
            .snapshot_executing_kbd_macro_runtime()
    }

    pub(crate) fn restore_executing_kbd_macro_runtime(
        &mut self,
        snapshot: ExecutingKbdMacroRuntimeSnapshot,
    ) {
        self.command_loop
            .keyboard
            .kboard
            .restore_executing_kbd_macro_runtime(snapshot);
        self.sync_keyboard_macro_runtime_vars();
    }

    pub(crate) fn set_executing_kbd_macro_runtime_index(&mut self, index: usize) {
        self.command_loop
            .keyboard
            .kboard
            .set_executing_kbd_macro_index(index);
        self.sync_keyboard_macro_runtime_vars();
    }

    pub(crate) fn note_executing_kbd_macro_iteration(&mut self, success_count: usize) {
        self.command_loop
            .keyboard
            .kboard
            .note_executing_kbd_macro_iteration(success_count);
        self.sync_keyboard_macro_runtime_vars();
    }

    pub(crate) fn finish_executing_kbd_macro_runtime(&mut self) {
        self.command_loop.finish_executing_kbd_macro();
        self.sync_keyboard_macro_runtime_vars();
    }

    pub(crate) fn clear_command_key_state(&mut self, keep_record: bool) {
        self.clear_read_command_keys();
        if !keep_record {
            self.clear_recent_input_events();
        }
    }

    pub(crate) fn set_this_command_keys_from_string(
        &mut self,
        keys: &str,
    ) -> Result<(), crate::emacs_core::error::Flow> {
        let key_bytes = keys.as_bytes();
        let mut translated = Vec::new();
        let mut pos = 0;
        let mut idx = 0;
        while pos < key_bytes.len() {
            let (mut code, len) = crate::emacs_core::emacs_char::string_char(&key_bytes[pos..]);
            // Match GNU `keyboard.c:12239-12252`: byte8 chars are normalized
            // back to raw 8-bit bytes before the `M-x` special case runs.
            if (0xE080..=0xE0FF).contains(&code) {
                code = (code - 0xE000) as u8 as u32;
            } else if (0xE300..=0xE3FF).contains(&code) {
                // Unibyte sentinel: raw byte value
                code = (code - 0xE300) as u32;
            }
            let event = if idx == 0 && code == 248 {
                Value::fixnum(('x' as i64) | KEY_CHAR_META)
            } else {
                Value::fixnum(code as i64)
            };
            translated.push(event);
            pos += len;
            idx += 1;
        }
        self.set_command_key_sequences(translated, Vec::new());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Interactive spec parsing
// ---------------------------------------------------------------------------

/// Parsed interactive argument specification.
#[derive(Clone, Debug)]
pub enum InteractiveCode {
    /// No arguments.
    None,
    /// Buffer name (with completion).
    BufferName(LispString),
    /// Character.
    Character(LispString),
    /// Point (cursor position).
    Point,
    /// Mark.
    Mark,
    /// Region (point and mark).
    Region,
    /// String from minibuffer.
    StringArg(LispString),
    /// Number from minibuffer.
    NumberArg(LispString),
    /// File name (with completion).
    FileName(LispString),
    /// Directory name.
    DirectoryName(LispString),
    /// Prefix argument (numeric).
    PrefixNumeric,
    /// Raw prefix argument.
    PrefixRaw,
    /// Function name (with completion).
    FunctionName(LispString),
    /// Variable name (with completion).
    VariableName(LispString),
    /// Command name (with completion).
    CommandName(LispString),
    /// Key sequence.
    KeySequenceArg(LispString),
    /// Lisp expression.
    Expression(LispString),
}

fn interactive_prompt_lisp_string(prompt: &str) -> LispString {
    crate::emacs_core::builtins::runtime_string_to_lisp_string(prompt, !prompt.is_ascii())
}

/// Parse an interactive specification string.
/// Example: "sSearch for: \nnRepeat count: "
pub fn parse_interactive_spec(spec: &str) -> Vec<InteractiveCode> {
    if spec.is_empty() {
        return vec![InteractiveCode::None];
    }

    let mut codes = Vec::new();
    let parts: Vec<&str> = spec.split('\n').collect();

    for part in parts {
        if part.is_empty() {
            continue;
        }
        let code = part.chars().next().unwrap();
        let prompt = &part[1..];
        let prompt = interactive_prompt_lisp_string(prompt);

        codes.push(match code {
            'b' => InteractiveCode::BufferName(prompt.clone()),
            'B' => InteractiveCode::BufferName(prompt.clone()),
            'c' => InteractiveCode::Character(prompt.clone()),
            'd' => InteractiveCode::Point,
            'm' => InteractiveCode::Mark,
            'r' => InteractiveCode::Region,
            's' => InteractiveCode::StringArg(prompt.clone()),
            'S' => InteractiveCode::StringArg(prompt.clone()),
            'n' => InteractiveCode::NumberArg(prompt.clone()),
            'N' => InteractiveCode::NumberArg(prompt.clone()),
            'f' => InteractiveCode::FileName(prompt.clone()),
            'F' => InteractiveCode::FileName(prompt.clone()),
            'D' => InteractiveCode::DirectoryName(prompt.clone()),
            'p' => InteractiveCode::PrefixNumeric,
            'P' => InteractiveCode::PrefixRaw,
            'a' => InteractiveCode::FunctionName(prompt.clone()),
            'C' => InteractiveCode::CommandName(prompt.clone()),
            'v' => InteractiveCode::VariableName(prompt.clone()),
            'k' => InteractiveCode::KeySequenceArg(prompt.clone()),
            'x' | 'X' => InteractiveCode::Expression(prompt.clone()),
            _ => InteractiveCode::StringArg(prompt),
        });
    }

    codes
}

fn key_sequence_translation_events(translation: Value) -> Option<Vec<Value>> {
    if translation.is_nil() || translation.is_fixnum() {
        return None;
    }
    if crate::emacs_core::keymap::is_list_keymap(&translation) {
        return None;
    }

    if translation.is_vector() {
        return Some(translation.as_vector_data()?.to_vec());
    }

    if let Some(s) = translation.as_utf8_str() {
        return Some(s.chars().map(|ch| Value::fixnum(ch as i64)).collect());
    }

    Some(vec![translation])
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[path = "keyboard_test.rs"]
mod tests;
