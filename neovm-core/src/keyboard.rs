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

use crate::emacs_core::keyboard::pure::KEY_CHAR_META;
use crate::emacs_core::string_escape::decode_storage_char_codes;
use crate::emacs_core::value::Value;
use std::collections::VecDeque;

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
                let key = match name.as_str() {
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
            prompt: Value::Nil,
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
) -> Option<InputEvent> {
    if !pressed {
        return None;
    }

    let key_event = keysym_to_key_event(keysym, modifiers)?;
    Some(InputEvent::KeyPress(key_event))
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
    KeyPress(KeyEvent),
    /// Mouse button press.
    MousePress {
        button: MouseButton,
        x: f32,
        y: f32,
        modifiers: Modifiers,
    },
    /// Mouse button release.
    MouseRelease { button: MouseButton, x: f32, y: f32 },
    /// Mouse movement.
    MouseMove {
        x: f32,
        y: f32,
        modifiers: Modifiers,
    },
    /// Mouse scroll.
    MouseScroll {
        delta_x: f32,
        delta_y: f32,
        x: f32,
        y: f32,
        modifiers: Modifiers,
    },
    /// Window resize.
    Resize {
        width: u32,
        height: u32,
        emacs_frame_id: u64,
    },
    /// Window focus change.
    Focus(bool),
    /// Close request.
    CloseRequested,
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

impl PrefixArg {
    /// Convert to Lisp value for `current-prefix-arg`.
    pub fn to_value(&self) -> Value {
        match self {
            PrefixArg::None => Value::Nil,
            PrefixArg::Numeric(n) => Value::Int(*n),
            PrefixArg::Raw(n) => {
                let val = 4i64.pow(*n as u32);
                Value::list(vec![Value::Int(val)])
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
    /// Keyboard macro being defined, as Lisp-visible Emacs events.
    pub kbd_macro_events: Vec<Value>,
    /// Keyboard macro being executed, as Lisp-visible Emacs events.
    pub executing_kbd_macro: Option<Vec<Value>>,
    /// Index into executing keyboard macro.
    pub kbd_macro_index: usize,
}

impl KBoard {
    pub fn new() -> Self {
        Self {
            unread_events: VecDeque::new(),
            current_key_sequence: ReadKeySequenceState::new(),
            command_keys: Vec::new(),
            raw_command_keys: Vec::new(),
            recent_input_events: Vec::new(),
            input_decode_map: Value::Nil,
            local_function_key_map: Value::Nil,
            defining_kbd_macro: false,
            kbd_macro_events: Vec::new(),
            executing_kbd_macro: None,
            kbd_macro_index: 0,
        }
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
    }

    pub fn recent_input_events(&self) -> &[Value] {
        &self.recent_input_events
    }

    pub fn clear_recent_input_events(&mut self) {
        self.recent_input_events.clear();
    }

    pub fn start_kbd_macro(&mut self) {
        self.defining_kbd_macro = true;
        self.kbd_macro_events.clear();
    }

    pub fn end_kbd_macro(&mut self) {
        self.defining_kbd_macro = false;
    }

    pub fn call_last_kbd_macro(&mut self) {
        if !self.kbd_macro_events.is_empty() {
            self.executing_kbd_macro = Some(self.kbd_macro_events.clone());
            self.kbd_macro_index = 0;
        }
    }
}

impl Default for KBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::gc::GcTrace for KBoard {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
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
    /// Keyboard-local command-loop state.
    pub kboard: KBoard,
}

impl KeyboardRuntime {
    pub fn new() -> Self {
        Self {
            event_queue: VecDeque::new(),
            pending_input_events: VecDeque::new(),
            kboard: KBoard::new(),
        }
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
        if let Some(event) = self.kboard.unread_events.pop_front() {
            return Some(event);
        }

        if let Some(ref macro_events) = self.kboard.executing_kbd_macro
            && self.kboard.kbd_macro_index < macro_events.len()
        {
            let event = macro_events[self.kboard.kbd_macro_index];
            self.kboard.kbd_macro_index += 1;
            return Some(event);
        }

        while let Some(event) = self.event_queue.pop_front() {
            if let InputEvent::KeyPress(key) = event {
                let emacs_event = key.to_emacs_event_value();
                if self.kboard.defining_kbd_macro {
                    self.kboard.kbd_macro_events.push(emacs_event);
                }
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
        self.kboard.defining_kbd_macro = true;
        self.kboard.kbd_macro_events.clear();
    }

    pub fn end_kbd_macro(&mut self) {
        self.kboard.defining_kbd_macro = false;
    }

    pub fn call_last_kbd_macro(&mut self) {
        if !self.kboard.kbd_macro_events.is_empty() {
            self.kboard.executing_kbd_macro = Some(self.kboard.kbd_macro_events.clone());
            self.kboard.kbd_macro_index = 0;
        }
    }
}

impl Default for KeyboardRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::gc::GcTrace for KeyboardRuntime {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        self.kboard.trace_roots(roots);
    }
}

/// State of the command loop.
pub struct CommandLoop {
    /// Keyboard-local runtime state.
    pub keyboard: KeyboardRuntime,
    /// Current prefix argument.
    pub prefix_arg: PrefixArg,
    /// The last command executed (symbol name).
    pub last_command: Option<String>,
    /// The current command being executed.
    pub this_command: Option<String>,
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
}

impl CommandLoop {
    pub fn new() -> Self {
        Self {
            keyboard: KeyboardRuntime::new(),
            prefix_arg: PrefixArg::None,
            last_command: None,
            this_command: None,
            recursive_depth: 0,
            running: false,
            quit_flag: false,
            inhibit_quit: false,
            idle_start_time: None,
            last_idle_start_time: None,
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

    /// Stop recording a keyboard macro.
    pub fn end_kbd_macro(&mut self) {
        self.keyboard.end_kbd_macro();
    }

    /// Execute the last keyboard macro.
    pub fn call_last_kbd_macro(&mut self) {
        self.keyboard.call_last_kbd_macro();
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

impl crate::gc::GcTrace for CommandLoop {
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
            Some(InputEvent::Focus(_)) => {
                if let Some(event) = pending_input_events.pop_front() {
                    deferred.push_back(event);
                }
            }
            Some(InputEvent::Resize {
                width,
                height,
                emacs_frame_id,
            }) => {
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
                apply_resize_input_event_in_keyboard_runtime(frames, width, height, emacs_frame_id);
                applied_resize = true;
            }
            Ok(event @ InputEvent::Focus(_)) => {
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
    let Value::Vector(timer_id) = timer else {
        return None;
    };

    let slots = crate::emacs_core::value::with_heap(|heap| heap.get_vector(timer_id).clone());
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
            psecs: slots.get(8).and_then(Value::as_int).unwrap_or(0),
        },
    })
}

fn pending_gnu_idle_timer_in_keyboard_runtime(
    timer: Value,
) -> Option<crate::emacs_core::eval::PendingGnuTimer> {
    let Value::Vector(timer_id) = timer else {
        return None;
    };

    let slots = crate::emacs_core::value::with_heap(|heap| heap.get_vector(timer_id).clone());
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
            psecs: slots.get(8).and_then(Value::as_int).unwrap_or(0),
        },
    })
}

impl crate::emacs_core::eval::Context {
    fn current_key_sequence_translation_maps(&self) -> [Value; 3] {
        [
            self.command_loop.keyboard.input_decode_map(),
            self.command_loop.keyboard.local_function_key_map(),
            self.eval_symbol("key-translation-map")
                .unwrap_or(Value::Nil),
        ]
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
            let restore_original =
                (options.dont_downcase_last || sequence_is_undefined)
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
                Value::True
            } else {
                Value::Nil
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
                        frame.parameters.get("window-system").copied()
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
                        frame.parameters.get("window-system").copied()
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
    /// binding is Value::Nil if the key sequence is undefined.
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

        self.command_loop.reset_key_sequence();
        self.assign("this-command-keys-shift-translated", Value::Nil);
        let mut shift_translation: Option<KeySequenceShiftTranslation> = None;
        let mut replay_current_sequence = false;

        loop {
            if replay_current_sequence {
                replay_current_sequence = false;
                tracing::debug!("read_key_sequence: replaying buffered sequence");
            } else {
                let emacs_event = self.read_char()?;
                self.command_loop
                    .keyboard
                    .push_key_sequence_input_event(emacs_event);

                self.record_input_event(emacs_event);

                tracing::debug!(
                    "read_key_sequence: event={} starting translation",
                    crate::emacs_core::print::print_value(&emacs_event)
                );
            }

            let translation = self.apply_translation_maps_to_current_key_sequence(options)?;
            let translated_events = translation.translated_events;

            tracing::debug!(
                "read_key_sequence: looking up binding for {:?}",
                translated_events
                    .iter()
                    .map(crate::emacs_core::print::print_value)
                    .collect::<Vec<_>>()
            );
            let resolved =
                resolve_active_key_binding(self, &translated_events, false, false, None)?;
            let binding = resolved.binding;
            let lookup_is_undefined =
                resolved.lookup.is_nil() || resolved.lookup == Value::symbol("undefined");
            let binding_is_undefined =
                binding.is_nil() || binding == Value::symbol("undefined");
            let sequence_is_undefined = lookup_is_undefined || binding_is_undefined;
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
                let (translated, raw) = self.command_loop.keyboard.key_sequence_snapshot();
                self.command_loop
                    .set_command_key_sequences(translated.clone(), raw);
                return Ok((translated, binding));
            }

            let is_prefix =
                resolve_prefix_keymap_binding_in_obarray(&self.obarray, &resolved.lookup).is_some();

            if is_prefix {
                let key_vec = Value::vector(translated_events.clone());
                if let Ok(desc) =
                    crate::emacs_core::builtins::keymaps::builtin_key_description(vec![key_vec])
                {
                    if let Some(s) = desc.as_str() {
                        let echo_msg = format!("{}-", s);
                        let _ = crate::emacs_core::builtins::dispatch_builtin(
                            self,
                            "message",
                            vec![Value::string(echo_msg)],
                        );
                    }
                }
                continue;
            }

            self.finalize_shift_translated_key_sequence(
                sequence_is_undefined,
                options,
                shift_translation.take(),
            );
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
    pub(crate) fn read_char(&mut self) -> Result<Value, crate::emacs_core::error::Flow> {
        if let Some(event) = self.command_loop.keyboard.kboard.unread_events.pop_front() {
            return Ok(event);
        }

        if let Some(ref macro_events) = self.command_loop.keyboard.kboard.executing_kbd_macro {
            if self.command_loop.keyboard.kboard.kbd_macro_index < macro_events.len() {
                let event = macro_events[self.command_loop.keyboard.kboard.kbd_macro_index];
                self.command_loop.keyboard.kboard.kbd_macro_index += 1;
                return Ok(event);
            }
        }

        self.sync_pending_resize_events();
        self.redisplay();
        self.fire_pending_timers();
        self.poll_process_output();

        tracing::debug!(
            "read_char: blocking on input (input_rx={})...",
            self.input_rx.is_some()
        );
        loop {
            if self.sync_pending_resize_events() {
                self.redisplay();
            }

            let event =
                if let Some(event) = self.command_loop.keyboard.pending_input_events.pop_front() {
                    self.timer_stop_idle();
                    event
                } else {
                    let rx = match self.input_rx {
                        Some(ref rx) => rx.clone(),
                        None => {
                            tracing::debug!("read_char: no input_rx (batch mode), returning Nil");
                            return Ok(Value::Nil);
                        }
                    };

                    self.timer_start_idle();
                    let timeout = self.next_input_wait_timeout();
                    if cfg!(test) {
                        eprintln!(
                            "read_char wait timeout={:?} idle={:?}",
                            timeout,
                            self.current_idle_duration()
                        );
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
                            if cfg!(test) {
                                eprintln!("read_char recv event={:?}", event);
                            }
                            self.timer_stop_idle();
                            event
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                            if cfg!(test) {
                                eprintln!(
                                    "read_char timeout idle={:?} ordinary={:?} idle-timer={:?}",
                                    self.current_idle_duration(),
                                    self.next_ordinary_gnu_timer_timeout(),
                                    self.next_idle_gnu_timer_timeout()
                                );
                            }
                            self.fire_pending_timers();
                            self.poll_process_output();
                            continue;
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                            self.command_loop.running = false;
                            return Err(crate::emacs_core::error::signal("quit", vec![]));
                        }
                    }
                };

            self.fire_pending_timers();
            self.poll_process_output();

            match event {
                InputEvent::CloseRequested => {
                    self.command_loop.running = false;
                    return Err(crate::emacs_core::error::signal("quit", vec![]));
                }
                InputEvent::Resize {
                    width,
                    height,
                    emacs_frame_id,
                } => {
                    self.apply_resize_input_event(width, height, emacs_frame_id, true);
                    self.timer_resume_idle();
                    continue;
                }
                InputEvent::Focus(_focused) => {
                    self.timer_resume_idle();
                    continue;
                }
                InputEvent::KeyPress(ref key) => {
                    tracing::debug!("read_char: received KeyPress {:?}", key);
                    self.clear_current_message();
                    let emacs_event = key.to_emacs_event_value();
                    if self.command_loop.keyboard.kboard.defining_kbd_macro {
                        self.command_loop
                            .keyboard
                            .kboard
                            .kbd_macro_events
                            .push(emacs_event);
                    }
                    return Ok(key.to_emacs_event_value());
                }
                InputEvent::MousePress {
                    button,
                    x,
                    y,
                    modifiers,
                } => {
                    self.clear_current_message();
                    let event =
                        Self::make_mouse_event(&button, x, y, &modifiers, "down-mouse", self);
                    return Ok(event);
                }
                InputEvent::MouseRelease { button, x, y } => {
                    self.clear_current_message();
                    let event =
                        Self::make_mouse_event(&button, x, y, &Modifiers::none(), "mouse", self);
                    return Ok(event);
                }
                InputEvent::MouseScroll {
                    delta_x: _,
                    delta_y,
                    x,
                    y,
                    modifiers,
                } => {
                    let dir = if delta_y > 0.0 {
                        "wheel-up"
                    } else {
                        "wheel-down"
                    };
                    let mut sym = String::new();
                    Self::append_modifier_prefix(&modifiers, &mut sym);
                    sym.push_str(dir);
                    let position = Self::make_mouse_position(x, y, self);
                    return Ok(Value::list(vec![Value::symbol(&sym), position]));
                }
                InputEvent::MouseMove { .. } => {
                    self.timer_resume_idle();
                    continue;
                }
            }
        }
    }

    /// Build an Emacs mouse event value.
    ///
    /// Returns `(EVENT-SYMBOL POSITION)` where EVENT-SYMBOL is e.g.
    /// `mouse-1`, `down-mouse-2`, `C-mouse-1`, etc.
    pub(crate) fn make_mouse_event(
        button: &MouseButton,
        x: f32,
        y: f32,
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

        let position = Self::make_mouse_position(x, y, eval);
        Value::list(vec![Value::symbol(&sym), position])
    }

    /// Build an Emacs mouse position value.
    ///
    /// Returns `(WINDOW POS (X . Y) TIMESTAMP)` where WINDOW is the
    /// selected window, POS is the current point, and TIMESTAMP is 0.
    pub(crate) fn make_mouse_position(x: f32, y: f32, eval: &Self) -> Value {
        let window = eval.eval_symbol("selected-window").unwrap_or(Value::Nil);
        let window_val = if window.is_nil() { Value::Nil } else { window };
        let pos = eval
            .buffers
            .current_buffer()
            .map(|buf| Value::Int(buf.point_char() as i64 + 1))
            .unwrap_or(Value::Int(1));
        let xy = Value::cons(Value::Int(x as i64), Value::Int(y as i64));
        Value::list(vec![Value::list(vec![window_val, pos, xy, Value::Int(0)])])
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
            return Value::Nil;
        };
        let secs = idle_duration.as_secs() as i64;
        let usecs = idle_duration.subsec_micros() as i64;
        Value::list(vec![
            Value::Int((secs >> 16) & 0xFFFF_FFFF),
            Value::Int(secs & 0xFFFF),
            Value::Int(usecs),
            Value::Int(0),
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

    fn due_gnu_timers_snapshot(&self) -> Vec<Value> {
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

    fn due_gnu_idle_timers_snapshot(&self) -> Vec<Value> {
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
        let mut timeout = self.timers.next_fire_time();

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
        let mut fired_any = false;

        for timer in self.due_gnu_timers_snapshot() {
            fired_any = true;
            if let Value::Vector(timer_id) = timer {
                crate::emacs_core::value::with_heap_mut(|heap| {
                    heap.get_vector_mut(timer_id)[0] = Value::True
                });
            }
            if let Err(e) = self.apply(Value::symbol("timer-event-handler"), vec![timer]) {
                tracing::warn!("GNU Lisp timer callback error: {:?}", e);
            }
        }

        for timer in self.due_gnu_idle_timers_snapshot() {
            fired_any = true;
            if let Value::Vector(timer_id) = timer {
                crate::emacs_core::value::with_heap_mut(|heap| {
                    heap.get_vector_mut(timer_id)[0] = Value::True
                });
            }
            if cfg!(test) {
                eprintln!("fire_pending_timers idle timer={:?}", timer);
            }
            if let Err(e) = self.apply(Value::symbol("timer-event-handler"), vec![timer]) {
                tracing::warn!("GNU Lisp idle timer callback error: {:?}", e);
            } else if cfg!(test) {
                eprintln!("fire_pending_timers idle callback returned");
            }
        }

        let now = std::time::Instant::now();
        let fired = self.timers.fire_pending_timers(now);
        for (callback, args) in fired {
            fired_any = true;
            let mut call_args = vec![callback];
            call_args.extend(args);
            if let Err(e) =
                crate::emacs_core::builtins::dispatch_builtin(self, "funcall", call_args)
                    .unwrap_or(Ok(Value::Nil))
            {
                tracing::warn!("Rust timer callback error: {:?}", e);
            }
        }

        if fired_any {
            self.redisplay();
        }
    }

    pub(crate) fn poll_process_output(&mut self) {
        let proc_ids = self.processes.live_process_ids();
        if proc_ids.is_empty() {
            return;
        }

        for pid in proc_ids {
            let exited = self.processes.check_child_exit(pid);

            let read_result = self.processes.read_process_output(pid);
            let is_network = self
                .processes
                .get(pid)
                .map(|p| p.kind == crate::emacs_core::process::ProcessKind::Network)
                .unwrap_or(false);

            match read_result {
                Some(ref data) if !data.is_empty() => {
                    let filter = self
                        .processes
                        .get(pid)
                        .map(|p| p.filter)
                        .unwrap_or(Value::Nil);

                    let saved_match_data = self.match_data.clone();
                    let saved_current_buffer = self.buffers.current_buffer_id();

                    if filter.is_nil() || filter.is_symbol_named("internal-default-process-filter")
                    {
                        let proc_val = Value::Int(pid as i64);
                        let output_val = Value::string(data);
                        if let Err(e) =
                            crate::emacs_core::process::builtin_internal_default_process_filter(
                                self,
                                vec![proc_val, output_val],
                            )
                        {
                            tracing::warn!("Default process filter error for pid {}: {:?}", pid, e);
                        }
                    } else if filter.is_truthy() {
                        let proc_val = Value::Int(pid as i64);
                        let output_val = Value::string(data);
                        if let Err(e) = self.apply(filter, vec![proc_val, output_val]) {
                            tracing::warn!("Process filter error for pid {}: {:?}", pid, e);
                        }
                    }

                    self.match_data = saved_match_data;
                    if let Some(buf_id) = saved_current_buffer {
                        self.restore_current_buffer_if_live(buf_id);
                    }
                }
                None if is_network => {
                    if let Some(proc) = self.processes.get_mut(pid) {
                        proc.status = crate::emacs_core::process::ProcessStatus::Exit(0);
                    }
                    let sentinel = self
                        .processes
                        .get(pid)
                        .map(|p| p.sentinel)
                        .unwrap_or(Value::Nil);
                    if !sentinel.is_nil()
                        && !sentinel.is_symbol_named("internal-default-process-sentinel")
                        && sentinel.is_truthy()
                    {
                        let saved_match_data = self.match_data.clone();
                        let saved_current_buffer = self.buffers.current_buffer_id();

                        let proc_val = Value::Int(pid as i64);
                        let msg_val = Value::string("connection broken by remote peer\n");
                        if let Err(e) = self.apply(sentinel, vec![proc_val, msg_val]) {
                            tracing::warn!("Network sentinel error for pid {}: {:?}", pid, e);
                        }

                        self.match_data = saved_match_data;
                        if let Some(buf_id) = saved_current_buffer {
                            self.restore_current_buffer_if_live(buf_id);
                        }
                    }
                    continue;
                }
                _ => {}
            }

            if exited {
                let sentinel = self
                    .processes
                    .get(pid)
                    .map(|p| p.sentinel)
                    .unwrap_or(Value::Nil);
                let exit_msg = self
                    .processes
                    .get(pid)
                    .map(|p| match &p.status {
                        crate::emacs_core::process::ProcessStatus::Exit(code) => {
                            if *code == 0 {
                                "finished\n".to_string()
                            } else {
                                format!("exited abnormally with code {}\n", code)
                            }
                        }
                        crate::emacs_core::process::ProcessStatus::Signal(sig) => {
                            format!("killed by signal {}\n", sig)
                        }
                        _ => "finished\n".to_string(),
                    })
                    .unwrap_or_else(|| "finished\n".to_string());
                if !sentinel.is_nil()
                    && !sentinel.is_symbol_named("internal-default-process-sentinel")
                    && sentinel.is_truthy()
                {
                    let saved_match_data = self.match_data.clone();
                    let saved_current_buffer = self.buffers.current_buffer_id();

                    let proc_val = Value::Int(pid as i64);
                    let msg_val = Value::string(&exit_msg);
                    if let Err(e) = self.apply(sentinel, vec![proc_val, msg_val]) {
                        tracing::warn!("Process sentinel error for pid {}: {:?}", pid, e);
                    }

                    self.match_data = saved_match_data;
                    if let Some(buf_id) = saved_current_buffer {
                        self.restore_current_buffer_if_live(buf_id);
                    }
                }
            }
        }
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
        let mut translated = Vec::with_capacity(keys.chars().count());
        for (idx, code) in decode_storage_char_codes(keys).into_iter().enumerate() {
            let event = if idx == 0 && code == 248 {
                Value::Int(('x' as i64) | KEY_CHAR_META)
            } else {
                Value::Int(i64::from(code))
            };
            translated.push(event);
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
    BufferName(String),
    /// Character.
    Character(String),
    /// Point (cursor position).
    Point,
    /// Mark.
    Mark,
    /// Region (point and mark).
    Region,
    /// String from minibuffer.
    StringArg(String),
    /// Number from minibuffer.
    NumberArg(String),
    /// File name (with completion).
    FileName(String),
    /// Directory name.
    DirectoryName(String),
    /// Prefix argument (numeric).
    PrefixNumeric,
    /// Raw prefix argument.
    PrefixRaw,
    /// Function name (with completion).
    FunctionName(String),
    /// Variable name (with completion).
    VariableName(String),
    /// Command name (with completion).
    CommandName(String),
    /// Key sequence.
    KeySequenceArg(String),
    /// Lisp expression.
    Expression(String),
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

        codes.push(match code {
            'b' => InteractiveCode::BufferName(prompt.to_string()),
            'B' => InteractiveCode::BufferName(prompt.to_string()),
            'c' => InteractiveCode::Character(prompt.to_string()),
            'd' => InteractiveCode::Point,
            'm' => InteractiveCode::Mark,
            'r' => InteractiveCode::Region,
            's' => InteractiveCode::StringArg(prompt.to_string()),
            'S' => InteractiveCode::StringArg(prompt.to_string()),
            'n' => InteractiveCode::NumberArg(prompt.to_string()),
            'N' => InteractiveCode::NumberArg(prompt.to_string()),
            'f' => InteractiveCode::FileName(prompt.to_string()),
            'F' => InteractiveCode::FileName(prompt.to_string()),
            'D' => InteractiveCode::DirectoryName(prompt.to_string()),
            'p' => InteractiveCode::PrefixNumeric,
            'P' => InteractiveCode::PrefixRaw,
            'a' => InteractiveCode::FunctionName(prompt.to_string()),
            'C' => InteractiveCode::CommandName(prompt.to_string()),
            'v' => InteractiveCode::VariableName(prompt.to_string()),
            'k' => InteractiveCode::KeySequenceArg(prompt.to_string()),
            'x' | 'X' => InteractiveCode::Expression(prompt.to_string()),
            _ => InteractiveCode::StringArg(prompt.to_string()),
        });
    }

    codes
}

fn key_sequence_translation_events(translation: Value) -> Option<Vec<Value>> {
    if translation.is_nil() || matches!(translation, Value::Int(_)) {
        return None;
    }
    if crate::emacs_core::keymap::is_list_keymap(&translation) {
        return None;
    }

    if let Value::Vector(id) = translation {
        return Some(crate::emacs_core::value::with_heap(|h| {
            let len = h.vector_len(id);
            (0..len).map(|i| h.vector_ref(id, i)).collect()
        }));
    }

    if let Some(s) = translation.as_str() {
        return Some(s.chars().map(|ch| Value::Int(ch as i64)).collect());
    }

    Some(vec![translation])
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_event_description() {
        let e = KeyEvent::char('x');
        assert_eq!(e.to_description(), "x");

        let e = KeyEvent::char_with_mods('x', Modifiers::ctrl());
        assert_eq!(e.to_description(), "C-x");

        let e = KeyEvent::char_with_mods('f', Modifiers::meta());
        assert_eq!(e.to_description(), "M-f");

        let e = KeyEvent::char_with_mods('g', Modifiers::ctrl_meta());
        assert_eq!(e.to_description(), "C-M-g");

        let e = KeyEvent::named(NamedKey::Return);
        assert_eq!(e.to_description(), "RET");
    }

    #[test]
    fn key_event_parse() {
        let e = KeyEvent::from_description("C-x").unwrap();
        assert_eq!(e.key, Key::Char('x'));
        assert!(e.modifiers.ctrl);
        assert!(!e.modifiers.meta);

        let e = KeyEvent::from_description("M-f").unwrap();
        assert_eq!(e.key, Key::Char('f'));
        assert!(e.modifiers.meta);

        let e = KeyEvent::from_description("RET").unwrap();
        assert_eq!(e.key, Key::Named(NamedKey::Return));

        let e = KeyEvent::from_description("C-M-g").unwrap();
        assert!(e.modifiers.ctrl);
        assert!(e.modifiers.meta);
    }

    #[test]
    fn key_sequence_description() {
        let seq = KeySequence::from_description("C-x C-f").unwrap();
        assert_eq!(seq.len(), 2);
        assert_eq!(seq.to_description(), "C-x C-f");
    }

    #[test]
    fn prefix_arg_values() {
        assert_eq!(PrefixArg::None.numeric_value(), 1);
        assert_eq!(PrefixArg::Numeric(5).numeric_value(), 5);
        assert_eq!(PrefixArg::Raw(1).numeric_value(), 4);
        assert_eq!(PrefixArg::Raw(2).numeric_value(), 16);
    }

    #[test]
    fn command_loop_enqueue_read() {
        let mut cl = CommandLoop::new();
        cl.enqueue_event(InputEvent::KeyPress(KeyEvent::char('a')));
        cl.enqueue_event(InputEvent::KeyPress(KeyEvent::char('b')));

        let e = cl.read_key_event().unwrap();
        assert_eq!(e, Value::Int('a' as i64));
        let e = cl.read_key_event().unwrap();
        assert_eq!(e, Value::Int('b' as i64));
        assert!(cl.read_key_event().is_none());
    }

    #[test]
    fn unread_events_have_priority() {
        let mut cl = CommandLoop::new();
        cl.enqueue_event(InputEvent::KeyPress(KeyEvent::char('a')));
        cl.unread_key(KeyEvent::char('z'));

        let e = cl.read_key_event().unwrap();
        assert_eq!(e, Value::Int('z' as i64)); // unread first
        let e = cl.read_key_event().unwrap();
        assert_eq!(e, Value::Int('a' as i64)); // then queue
    }

    #[test]
    fn keyboard_macro_recording() {
        let mut cl = CommandLoop::new();
        cl.start_kbd_macro();

        cl.enqueue_event(InputEvent::KeyPress(KeyEvent::char('h')));
        cl.enqueue_event(InputEvent::KeyPress(KeyEvent::char('i')));

        cl.read_key_event(); // 'h' — recorded
        cl.read_key_event(); // 'i' — recorded

        cl.end_kbd_macro();
        assert_eq!(cl.keyboard.kboard.kbd_macro_events.len(), 2);

        // Replay.
        cl.call_last_kbd_macro();
        let e1 = cl.read_key_event().unwrap();
        assert_eq!(e1, Value::Int('h' as i64));
        let e2 = cl.read_key_event().unwrap();
        assert_eq!(e2, Value::Int('i' as i64));
    }

    #[test]
    fn quit_flag() {
        let mut cl = CommandLoop::new();
        assert!(!cl.check_quit());

        cl.signal_quit();
        assert!(cl.check_quit());
        assert!(!cl.check_quit()); // cleared
    }

    #[test]
    fn interactive_spec_parsing() {
        let codes = parse_interactive_spec("sSearch for: \nnCount: ");
        assert_eq!(codes.len(), 2);
        assert!(matches!(&codes[0], InteractiveCode::StringArg(p) if p == "Search for: "));
        assert!(matches!(&codes[1], InteractiveCode::NumberArg(p) if p == "Count: "));
    }

    #[test]
    fn modifier_bits_round_trip() {
        let m = Modifiers {
            ctrl: true,
            meta: true,
            shift: false,
            super_: false,
            hyper: false,
        };
        let bits = m.to_bits();
        let m2 = Modifiers::from_bits(bits);
        assert_eq!(m, m2);
    }

    #[test]
    fn modifier_bits_round_trip_all_combinations() {
        // Test each individual modifier
        for (field, expected_bit) in [
            ("ctrl", 1u32 << 26),
            ("meta", 1u32 << 27),
            ("shift", 1u32 << 25),
            ("super", 1u32 << 23),
            ("hyper", 1u32 << 24),
        ] {
            let m = match field {
                "ctrl" => Modifiers {
                    ctrl: true,
                    ..Modifiers::none()
                },
                "meta" => Modifiers {
                    meta: true,
                    ..Modifiers::none()
                },
                "shift" => Modifiers {
                    shift: true,
                    ..Modifiers::none()
                },
                "super" => Modifiers {
                    super_: true,
                    ..Modifiers::none()
                },
                "hyper" => Modifiers {
                    hyper: true,
                    ..Modifiers::none()
                },
                _ => unreachable!(),
            };
            assert_eq!(m.to_bits(), expected_bit, "bit mismatch for {}", field);
            assert_eq!(
                Modifiers::from_bits(m.to_bits()),
                m,
                "round-trip failed for {}",
                field
            );
        }

        // All modifiers set
        let all = Modifiers {
            ctrl: true,
            meta: true,
            shift: true,
            super_: true,
            hyper: true,
        };
        assert_eq!(Modifiers::from_bits(all.to_bits()), all);

        // No modifiers
        assert_eq!(Modifiers::none().to_bits(), 0);
        assert_eq!(Modifiers::from_bits(0), Modifiers::none());
    }

    #[test]
    fn prefix_string_various() {
        assert_eq!(Modifiers::none().prefix_string(), "");
        assert_eq!(Modifiers::ctrl().prefix_string(), "C-");
        assert_eq!(Modifiers::meta().prefix_string(), "M-");
        assert_eq!(Modifiers::ctrl_meta().prefix_string(), "C-M-");

        let all = Modifiers {
            ctrl: true,
            meta: true,
            shift: true,
            super_: true,
            hyper: true,
        };
        // Order: H- s- C- M- S-
        assert_eq!(all.prefix_string(), "H-s-C-M-S-");
    }

    #[test]
    fn modifiers_is_empty() {
        assert!(Modifiers::none().is_empty());
        assert!(!Modifiers::ctrl().is_empty());
        assert!(!Modifiers::meta().is_empty());
    }

    #[test]
    fn key_event_from_description_all_named_keys() {
        let cases = [
            ("RET", Key::Named(NamedKey::Return)),
            ("TAB", Key::Named(NamedKey::Tab)),
            ("ESC", Key::Named(NamedKey::Escape)),
            ("DEL", Key::Named(NamedKey::Backspace)),
            ("SPC", Key::Char(' ')),
            ("<delete>", Key::Named(NamedKey::Delete)),
            ("<insert>", Key::Named(NamedKey::Insert)),
            ("<home>", Key::Named(NamedKey::Home)),
            ("<end>", Key::Named(NamedKey::End)),
            ("<prior>", Key::Named(NamedKey::PageUp)),
            ("<next>", Key::Named(NamedKey::PageDown)),
            ("<left>", Key::Named(NamedKey::Left)),
            ("<right>", Key::Named(NamedKey::Right)),
            ("<up>", Key::Named(NamedKey::Up)),
            ("<down>", Key::Named(NamedKey::Down)),
            ("<f1>", Key::Named(NamedKey::F(1))),
            ("<f12>", Key::Named(NamedKey::F(12))),
        ];
        for (desc, expected_key) in cases {
            let e = KeyEvent::from_description(desc)
                .unwrap_or_else(|| panic!("failed to parse: {}", desc));
            assert_eq!(e.key, expected_key, "mismatch for {}", desc);
            assert!(e.modifiers.is_empty(), "unexpected modifiers for {}", desc);
        }
    }

    #[test]
    fn key_event_description_round_trip() {
        let descriptions = [
            "C-x", "M-f", "C-M-g", "S-<f1>", "H-s-a", "RET", "TAB", "SPC", "<left>",
        ];
        for desc in descriptions {
            let event = KeyEvent::from_description(desc).unwrap();
            let back = event.to_description();
            let reparsed = KeyEvent::from_description(&back).unwrap();
            assert_eq!(event, reparsed, "round-trip failed for {}", desc);
        }
    }

    #[test]
    fn key_event_to_event_int() {
        // Plain 'a' = 97
        let e = KeyEvent::char('a');
        assert_eq!(e.to_event_int(), 97);

        // C-a = 97 | (1 << 26)
        let e = KeyEvent::char_with_mods('a', Modifiers::ctrl());
        assert_eq!(e.to_event_int(), 97 | (1 << 26));

        // RET = 13
        let e = KeyEvent::named(NamedKey::Return);
        assert_eq!(e.to_event_int(), 13);
    }

    #[test]
    fn prefix_arg_to_value() {
        assert_eq!(PrefixArg::None.to_value(), Value::Nil);
        assert_eq!(PrefixArg::Numeric(3).to_value(), Value::Int(3));
        // Raw(1) = C-u once = (4)
        let raw1 = PrefixArg::Raw(1).to_value();
        assert!(matches!(raw1, Value::Cons(_)));
    }

    #[test]
    fn key_sequence_from_description_multi() {
        let seq = KeySequence::from_description("C-x C-s").unwrap();
        assert_eq!(seq.len(), 2);
        assert_eq!(seq.events[0], KeyEvent::from_description("C-x").unwrap());
        assert_eq!(seq.events[1], KeyEvent::from_description("C-s").unwrap());
    }

    #[test]
    fn key_sequence_empty() {
        let seq = KeySequence::new();
        assert!(seq.is_empty());
        assert_eq!(seq.to_description(), "");
    }

    #[test]
    fn read_key_sequence_state_tracks_raw_and_translated_events() {
        let mut state = ReadKeySequenceState::new();
        state.push_input_event(Value::Int('A' as i64));
        state.push_input_event(Value::Int('B' as i64));
        state.replace_translated_events(vec![Value::Int('a' as i64)]);

        let (translated, raw) = state.snapshot();
        assert_eq!(translated, vec![Value::Int('a' as i64)]);
        assert_eq!(raw, vec![Value::Int('A' as i64), Value::Int('B' as i64)]);
    }

    #[test]
    fn key_sequence_translation_events_normalizes_vector_string_and_scalar() {
        let vector = Value::vector(vec![Value::Int('x' as i64), Value::Int('y' as i64)]);
        assert_eq!(
            key_sequence_translation_events(vector),
            Some(vec![Value::Int('x' as i64), Value::Int('y' as i64)])
        );
        assert_eq!(
            key_sequence_translation_events(Value::string("ab")),
            Some(vec![Value::Int('a' as i64), Value::Int('b' as i64)])
        );
        assert_eq!(
            key_sequence_translation_events(Value::symbol("f1")),
            Some(vec![Value::symbol("f1")])
        );
        assert_eq!(key_sequence_translation_events(Value::Nil), None);
    }

    #[test]
    fn parse_interactive_spec_all_codes() {
        let codes = parse_interactive_spec("d");
        assert!(matches!(&codes[0], InteractiveCode::Point));

        let codes = parse_interactive_spec("m");
        assert!(matches!(&codes[0], InteractiveCode::Mark));

        let codes = parse_interactive_spec("r");
        assert!(matches!(&codes[0], InteractiveCode::Region));

        let codes = parse_interactive_spec("p");
        assert!(matches!(&codes[0], InteractiveCode::PrefixNumeric));

        let codes = parse_interactive_spec("P");
        assert!(matches!(&codes[0], InteractiveCode::PrefixRaw));

        let codes = parse_interactive_spec("fFile: ");
        assert!(matches!(&codes[0], InteractiveCode::FileName(p) if p == "File: "));

        let codes = parse_interactive_spec("DDirectory: ");
        assert!(matches!(&codes[0], InteractiveCode::DirectoryName(p) if p == "Directory: "));
    }

    #[test]
    fn parse_interactive_spec_empty() {
        let codes = parse_interactive_spec("");
        assert_eq!(codes.len(), 1);
        assert!(matches!(&codes[0], InteractiveCode::None));
    }

    #[test]
    fn inhibit_quit_blocks_signal() {
        let mut cl = CommandLoop::new();
        cl.inhibit_quit = true;
        cl.signal_quit();
        assert!(!cl.quit_flag); // should not be set when inhibited
    }

    // ===================================================================
    // keysym_to_key_event — control characters
    // ===================================================================

    #[test]
    fn keysym_ctrl_x_from_control_char() {
        // Ctrl+x → winit gives keysym 0x18 (control character)
        let event = keysym_to_key_event(0x18, RENDER_CTRL_MASK).unwrap();
        assert_eq!(event.key, Key::Char('x'));
        assert!(event.modifiers.ctrl);
    }

    #[test]
    fn keysym_ctrl_a_from_control_char() {
        let event = keysym_to_key_event(0x01, RENDER_CTRL_MASK).unwrap();
        assert_eq!(event.key, Key::Char('a'));
        assert!(event.modifiers.ctrl);
    }

    #[test]
    fn keysym_ctrl_z_from_control_char() {
        let event = keysym_to_key_event(0x1A, RENDER_CTRL_MASK).unwrap();
        assert_eq!(event.key, Key::Char('z'));
        assert!(event.modifiers.ctrl);
    }

    #[test]
    fn keysym_ctrl_g_from_control_char_no_modifier() {
        // Even without explicit ctrl modifier bit, control char implies ctrl
        let event = keysym_to_key_event(0x07, 0).unwrap();
        assert_eq!(event.key, Key::Char('g'));
        assert!(event.modifiers.ctrl);
    }

    #[test]
    fn keysym_ctrl_x_from_printable_with_modifier() {
        // Ctrl+x when winit gives keysym 0x78 ('x') with ctrl modifier
        let event = keysym_to_key_event(0x78, RENDER_CTRL_MASK).unwrap();
        assert_eq!(event.key, Key::Char('x'));
        assert!(event.modifiers.ctrl);
    }

    #[test]
    fn keysym_shifted_uppercase_char_drops_shift_modifier() {
        let event = keysym_to_key_event('A' as u32, RENDER_SHIFT_MASK).unwrap();
        assert_eq!(event.key, Key::Char('A'));
        assert!(!event.modifiers.shift);
    }

    #[test]
    fn keysym_unicode_scalar_maps_to_character_event() {
        let event = keysym_to_key_event('中' as u32, 0).unwrap();
        assert_eq!(event.key, Key::Char('中'));
        assert!(event.modifiers.is_empty());
    }

    #[test]
    fn keysym_ctrl_shift_x_drops_shift_modifier() {
        let event = keysym_to_key_event(0x18, RENDER_CTRL_MASK | RENDER_SHIFT_MASK).unwrap();
        assert_eq!(event.key, Key::Char('x'));
        assert!(event.modifiers.ctrl);
        assert!(!event.modifiers.shift);
    }

    #[test]
    fn render_modifiers_helper_matches_transport_bit_layout() {
        let mods =
            render_modifiers_to_modifiers(RENDER_SHIFT_MASK | RENDER_CTRL_MASK | RENDER_META_MASK);
        assert!(mods.shift);
        assert!(mods.ctrl);
        assert!(mods.meta);
        assert!(!mods.super_);
        assert!(!mods.hyper);
    }

    #[test]
    fn render_key_transport_drops_key_releases() {
        assert!(render_key_transport_to_input_event(XK_RETURN, 0, false).is_none());
    }
}
