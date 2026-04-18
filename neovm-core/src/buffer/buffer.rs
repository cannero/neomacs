//! Buffer and BufferManager — the core text container for the Elisp VM.
//!
//! A `Buffer` wraps a [`BufferText`] with Emacs-style point, mark, narrowing,
//! markers, and buffer-local variables.  `BufferManager` owns all live buffers
//! and tracks the current buffer.

#[path = "insdel.rs"]
mod insdel;

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use super::buffer_text::BufferText;
// Phase 10F: BufferLocals is gone. Per-buffer Lisp bindings now live
// in `Buffer::local_var_alist` (for LOCALIZED), `Buffer::slots[]`
// (for FORWARDED BUFFER_OBJFWD), and `Buffer::keymap` / the
// `SharedUndoState` (for the two always-present slots that don't
// match either pattern). Mirrors GNU's struct buffer layout in
// buffer.h:330-462.
use super::overlay::OverlayList;
use super::shared::SharedUndoState;
use super::text_props::TextPropertyTable;
use super::undo;
use crate::emacs_core::intern::{SymId, intern};
use crate::emacs_core::value::{RuntimeBindingValue, Value, ValueKind};
use crate::gc_trace::GcTrace;
use crate::tagged::gc::with_tagged_heap;
use crate::window::WindowId;
use rustc_hash::FxHashMap;

// ---------------------------------------------------------------------------
// BUFFER_SLOT_COUNT — sized to mirror GNU's `MAX_PER_BUFFER_VARS = 50`.
// ---------------------------------------------------------------------------

/// Number of `BUFFER_OBJFWD` slots in [`Buffer::slots`]. Mirrors GNU's
/// `MAX_PER_BUFFER_VARS = 50` limit on per-buffer C-side variables
/// (`buffer.h:311`). Bumped to 64 in Phase 10D so the conditional
/// `BUFFER_OBJFWD` slots (mode-line-format, fill-column, …) have room
/// alongside the always-local set already migrated in Phase 10A-C.
/// Sized to a power of two so [`Buffer::local_flags`] (a `u64`
/// bitmap) covers exactly one bit per slot. Bump again only after a
/// careful audit — the number bounds every Buffer's memory footprint.
pub const BUFFER_SLOT_COUNT: usize = 64;

// ---------------------------------------------------------------------------
// Phase 8b slot offset constants for the four hardcoded Buffer fields
// that will migrate from direct struct fields to slot accessors in
// follow-up commits. Mirrors GNU `buffer.c:5056-5500` where each
// `DEFVAR_PER_BUFFER` assigns a stable slot index.
// ---------------------------------------------------------------------------

/// Slot index for `buffer-file-name`. Mirrors GNU's slot for the
/// `file_name_` field in `struct buffer` (`buffer.h:319`).
pub const BUFFER_SLOT_FILE_NAME: usize = 0;
/// Slot index for `buffer-auto-save-file-name`. Mirrors GNU's
/// `auto_save_file_name_` (`buffer.h:323`).
pub const BUFFER_SLOT_AUTO_SAVE_FILE_NAME: usize = 1;
/// Slot index for `buffer-read-only`. Mirrors GNU's `read_only_`
/// (`buffer.h:338`).
pub const BUFFER_SLOT_READ_ONLY: usize = 2;
/// Slot index for `enable-multibyte-characters`. Mirrors GNU's
/// `enable_multibyte_characters_` (`buffer.h:346`).
pub const BUFFER_SLOT_ENABLE_MULTIBYTE_CHARACTERS: usize = 3;
/// Slot index for `buffer-file-truename`. Mirrors GNU's
/// `file_truename_` (`buffer.h:325`).
pub const BUFFER_SLOT_FILE_TRUENAME: usize = 4;
/// Slot index for `default-directory`. Mirrors GNU's
/// `directory_` (`buffer.h:321`).
pub const BUFFER_SLOT_DEFAULT_DIRECTORY: usize = 5;
/// Slot index for `buffer-saved-size`. Mirrors GNU's `save_length_`
/// (`buffer.h:340`).
pub const BUFFER_SLOT_SAVED_SIZE: usize = 6;
/// Slot index for `buffer-backed-up`. Mirrors GNU's `backed_up_`
/// (`buffer.h:341`).
pub const BUFFER_SLOT_BACKED_UP: usize = 7;
/// Slot index for `buffer-file-format`. Mirrors GNU's
/// `file_format_` (`buffer.h:342`).
pub const BUFFER_SLOT_FILE_FORMAT: usize = 8;
/// Slot index for `buffer-auto-save-file-format`. Mirrors GNU's
/// `auto_save_file_format_` (`buffer.h:343`).
pub const BUFFER_SLOT_AUTO_SAVE_FILE_FORMAT: usize = 9;
/// Slot index for `major-mode`. Mirrors GNU's `major_mode_`
/// (`buffer.h:347`).
pub const BUFFER_SLOT_MAJOR_MODE: usize = 10;
/// Slot index for `local-minor-modes`. Mirrors GNU's
/// `local_minor_modes_` (`buffer.h:349`).
pub const BUFFER_SLOT_LOCAL_MINOR_MODES: usize = 11;
/// Slot index for `mode-name`. Mirrors GNU's `mode_name_`
/// (`buffer.h:351`).
pub const BUFFER_SLOT_MODE_NAME: usize = 12;
/// Slot index for `mark-active`. Mirrors GNU's `mark_active_`
/// (`buffer.h:381`).
pub const BUFFER_SLOT_MARK_ACTIVE: usize = 13;
/// Slot index for `point-before-scroll`. Mirrors GNU's
/// `point_before_scroll_` (`buffer.h:413`).
pub const BUFFER_SLOT_POINT_BEFORE_SCROLL: usize = 14;
/// Slot index for `buffer-display-count`. Mirrors GNU's
/// `display_count_` (`buffer.h:418`).
pub const BUFFER_SLOT_DISPLAY_COUNT: usize = 15;
/// Slot index for `buffer-display-time`. Mirrors GNU's
/// `display_time_` (`buffer.h:432`).
pub const BUFFER_SLOT_DISPLAY_TIME: usize = 16;
/// Slot index for `buffer-invisibility-spec`. Mirrors GNU's
/// `invisibility_spec_` (`buffer.h:411`).
pub const BUFFER_SLOT_INVISIBILITY_SPEC: usize = 17;

// ---------------------------------------------------------------------------
// Phase 10D conditional slot offsets. These are BUFFER_OBJFWD slots
// with `local_flags_idx >= 0`: a fresh buffer's slot mirrors the
// global default in `BufferManager::buffer_defaults` until
// `make-local-variable` or a write through the slot flips the
// per-buffer `Buffer::local_flags` bit.
//
// Mirrors GNU `buffer.c:4742-4791` where each conditional `BVAR` slot
// gets a positive index assigned in `buffer_local_flags`.
// ---------------------------------------------------------------------------

/// Slot index for `fill-column`. Mirrors GNU's `fill_column_`
/// (`buffer.h:387`). First conditional slot migrated by Phase 10D
/// step 3 — picked because the value is a simple integer with a
/// non-trivial default (70) and dense test coverage.
pub const BUFFER_SLOT_FILL_COLUMN: usize = 18;
/// Slot index for `tab-width`. Mirrors GNU's `tab_width_`
/// (`buffer.h:386`). Default 8 (`buffer.c:4848`).
pub const BUFFER_SLOT_TAB_WIDTH: usize = 19;
/// Slot index for `left-margin`. Mirrors GNU's `left_margin_`
/// (`buffer.h:388`). Default 0 (`buffer.c:4867`).
pub const BUFFER_SLOT_LEFT_MARGIN: usize = 20;
/// Slot index for `abbrev-mode`. Mirrors GNU's `abbrev_mode_`
/// (`buffer.h:368`). Default nil (`buffer.c:4835`).
pub const BUFFER_SLOT_ABBREV_MODE: usize = 21;
/// Slot index for `overwrite-mode`. Mirrors GNU's `overwrite_mode_`
/// (`buffer.h:369`). Default nil (`buffer.c:4836`).
pub const BUFFER_SLOT_OVERWRITE_MODE: usize = 22;
/// Slot index for `selective-display`. Mirrors GNU's
/// `selective_display_` (`buffer.h:373`). Default nil (`buffer.c:4838`).
pub const BUFFER_SLOT_SELECTIVE_DISPLAY: usize = 23;
/// Slot index for `selective-display-ellipses`. Mirrors GNU's
/// `selective_display_ellipses_` (`buffer.h:374`). Default t
/// (`buffer.c:4839`).
pub const BUFFER_SLOT_SELECTIVE_DISPLAY_ELLIPSES: usize = 24;
/// Slot index for `truncate-lines`. Mirrors GNU's `truncate_lines_`
/// (`buffer.h:355`). Default nil (`buffer.c:4849`).
pub const BUFFER_SLOT_TRUNCATE_LINES: usize = 25;
/// Slot index for `word-wrap`. Mirrors GNU's `word_wrap_`
/// (`buffer.h:357`). Default nil (`buffer.c:4850`).
pub const BUFFER_SLOT_WORD_WRAP: usize = 26;
/// Slot index for `ctl-arrow`. Mirrors GNU's `ctl_arrow_`
/// (`buffer.h:359`). Default t (`buffer.c:4851`).
pub const BUFFER_SLOT_CTL_ARROW: usize = 27;
/// Slot index for `auto-fill-function`. Mirrors GNU's
/// `auto_fill_function_` (`buffer.h:367`). Default nil
/// (`buffer.c:4837`).
pub const BUFFER_SLOT_AUTO_FILL_FUNCTION: usize = 28;
/// Slot index for `mode-line-format`. Default `"%-"`.
pub const BUFFER_SLOT_MODE_LINE_FORMAT: usize = 29;
/// Slot index for `header-line-format`. Default nil.
pub const BUFFER_SLOT_HEADER_LINE_FORMAT: usize = 30;
/// Slot index for `tab-line-format`. Default nil.
pub const BUFFER_SLOT_TAB_LINE_FORMAT: usize = 31;
//
// Phase 10D step 5 batch 2 — display/bidi/fringe/scroll-bar slots.
/// Slot index for `bidi-display-reordering`. Default t.
pub const BUFFER_SLOT_BIDI_DISPLAY_REORDERING: usize = 32;
/// Slot index for `bidi-paragraph-direction`. Default nil.
pub const BUFFER_SLOT_BIDI_PARAGRAPH_DIRECTION: usize = 33;
/// Slot index for `bidi-paragraph-start-re`. Default nil.
pub const BUFFER_SLOT_BIDI_PARAGRAPH_START_RE: usize = 34;
/// Slot index for `bidi-paragraph-separate-re`. Default nil.
pub const BUFFER_SLOT_BIDI_PARAGRAPH_SEPARATE_RE: usize = 35;
/// Slot index for `cursor-type`. Default t.
pub const BUFFER_SLOT_CURSOR_TYPE: usize = 36;
/// Slot index for `line-spacing`. Default nil.
pub const BUFFER_SLOT_LINE_SPACING: usize = 37;
/// Slot index for `text-conversion-style`. Default nil.
pub const BUFFER_SLOT_TEXT_CONVERSION_STYLE: usize = 38;
/// Slot index for `cursor-in-non-selected-windows`. Default t.
pub const BUFFER_SLOT_CURSOR_IN_NON_SELECTED_WINDOWS: usize = 39;
/// Slot index for `left-margin-width`. Default nil.
pub const BUFFER_SLOT_LEFT_MARGIN_WIDTH: usize = 40;
/// Slot index for `right-margin-width`. Default nil.
pub const BUFFER_SLOT_RIGHT_MARGIN_WIDTH: usize = 41;
/// Slot index for `left-fringe-width`. Default nil.
pub const BUFFER_SLOT_LEFT_FRINGE_WIDTH: usize = 42;
/// Slot index for `right-fringe-width`. Default nil.
pub const BUFFER_SLOT_RIGHT_FRINGE_WIDTH: usize = 43;
/// Slot index for `fringes-outside-margins`. Default nil.
pub const BUFFER_SLOT_FRINGES_OUTSIDE_MARGINS: usize = 44;
/// Slot index for `scroll-bar-width`. Default nil.
pub const BUFFER_SLOT_SCROLL_BAR_WIDTH: usize = 45;
/// Slot index for `scroll-bar-height`. Default nil.
pub const BUFFER_SLOT_SCROLL_BAR_HEIGHT: usize = 46;
/// Slot index for `vertical-scroll-bar`. Default t.
pub const BUFFER_SLOT_VERTICAL_SCROLL_BAR: usize = 47;
/// Slot index for `horizontal-scroll-bar`. Default t.
pub const BUFFER_SLOT_HORIZONTAL_SCROLL_BAR: usize = 48;
/// Slot index for `indicate-empty-lines`. Default nil.
pub const BUFFER_SLOT_INDICATE_EMPTY_LINES: usize = 49;
/// Slot index for `indicate-buffer-boundaries`. Default nil.
pub const BUFFER_SLOT_INDICATE_BUFFER_BOUNDARIES: usize = 50;
/// Slot index for `fringe-indicator-alist`. Default nil.
pub const BUFFER_SLOT_FRINGE_INDICATOR_ALIST: usize = 51;
/// Slot index for `fringe-cursor-alist`. Default nil.
///
/// Cursor audit Finding 14 in `drafts/cursor-audit.md`: this
/// buffer-local slot exists and is registered as a forwarder, but
/// nothing in the layout engine or wgpu renderer reads it. GNU
/// uses it in `draw_fringe_bitmap_1` /
/// `get_logical_cursor_bitmap` to map fringe indicator types to
/// cursor bitmaps. Wiring it requires the fringe bitmap resolver,
/// which is itself still mostly stubbed.
pub const BUFFER_SLOT_FRINGE_CURSOR_ALIST: usize = 52;
/// Slot index for `scroll-up-aggressively`. Default nil.
pub const BUFFER_SLOT_SCROLL_UP_AGGRESSIVELY: usize = 53;
/// Slot index for `scroll-down-aggressively`. Default nil.
pub const BUFFER_SLOT_SCROLL_DOWN_AGGRESSIVELY: usize = 54;
/// Slot index for `cache-long-scans`. Default t.
pub const BUFFER_SLOT_CACHE_LONG_SCANS: usize = 55;
/// Slot index for `local-abbrev-table`. Default nil.
pub const BUFFER_SLOT_LOCAL_ABBREV_TABLE: usize = 56;
/// Slot index for `buffer-display-table`. Default nil.
pub const BUFFER_SLOT_BUFFER_DISPLAY_TABLE: usize = 57;
/// Slot index for `buffer-file-coding-system`. Default nil
/// (permanent).
pub const BUFFER_SLOT_BUFFER_FILE_CODING_SYSTEM: usize = 58;
/// Slot index for the buffer's syntax table (`BVAR(buf, syntax_table)`
/// in GNU `buffer.h:391`). Not exposed as a Lisp variable in GNU —
/// accessed only via `(syntax-table)` / `(set-syntax-table)`. Conditional
/// per GNU `buffer.c:4758` (`PER_BUFFER_VAR_IDX(syntax_table)`).
pub const BUFFER_SLOT_SYNTAX_TABLE: usize = 59;
/// Slot index for the buffer's category table (`BVAR(buf, category_table)`
/// in GNU `buffer.h:394`). Not exposed as a Lisp variable in GNU —
/// accessed only via `(category-table)` / `(set-category-table)`. Conditional
/// per GNU `buffer.c:4760`.
pub const BUFFER_SLOT_CATEGORY_TABLE: usize = 60;
/// Slot index for the buffer's case table (combined downcase/upcase/
/// canonicalize/equivalence as extras of a single char-table —
/// NeoMacs's collapse of GNU's 4-slot design in `buffer.h:408-417`).
/// Not exposed as a Lisp variable; accessed via `(current-case-table)`
/// / `(set-case-table)`. Always-local per GNU `buffer.c:4731-4734`
/// (flag=0 means every buffer has its own value, no conditional gate).
pub const BUFFER_SLOT_CASE_TABLE: usize = 61;

// ---------------------------------------------------------------------------
// BUFFER_SLOT_INFO table — declarative metadata for every BUFFER_OBJFWD
// slot. Mirrors GNU's `buffer_local_flags` + `defvar_per_buffer` table
// in `buffer.c:5056-5500`. Used by Phase 10C dispatch in `set_buffer_local`,
// `get_buffer_local`, `get_buffer_local_binding`, `ordered_buffer_local_*`
// and by the install loop in `Context::new` that flips the symbols to
// `SymbolRedirect::Forwarded`.
// ---------------------------------------------------------------------------

/// Default-value descriptor. Stored in the const table because
/// `Value::string` and `Value::fixnum` aren't `const`-friendly.
/// Materialised once at startup via [`SlotDefault::to_value`].
#[derive(Copy, Clone, Debug)]
pub enum SlotDefault {
    /// Use a const `Value` (NIL, T).
    Const(crate::emacs_core::value::Value),
    /// Encode an integer fixnum at install time.
    LazyFixnum(i64),
    /// Allocate a multibyte Lisp string at install time.
    LazyString(&'static str),
    /// Allocate a unibyte Lisp string at install time. Mirrors GNU's
    /// `make_unibyte_string` for `default-directory` during dump.
    LazyUnibyte(&'static str),
    /// Resolve to an interned symbol at install time.
    LazySymbol(&'static str),
    /// Resolve to the process's current working directory as a
    /// unibyte string with a trailing slash. Mirrors GNU
    /// `init_buffer_once`'s setup of `default-directory`
    /// (`buffer.c:5381`).
    LazyCwd,
}

impl SlotDefault {
    /// Materialise the default into a runtime [`Value`]. Called once at
    /// startup; the produced Value is GC-rooted by the buffer slot
    /// table from then on.
    pub fn to_value(self) -> crate::emacs_core::value::Value {
        use crate::emacs_core::value::Value;
        match self {
            SlotDefault::Const(v) => v,
            SlotDefault::LazyFixnum(n) => Value::fixnum(n),
            SlotDefault::LazyString(s) => Value::string(s),
            SlotDefault::LazyUnibyte(s) => Value::unibyte_string(s),
            SlotDefault::LazySymbol(s) => Value::symbol(s),
            SlotDefault::LazyCwd => {
                let mut s = std::env::current_dir()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| "/".to_string());
                if !s.ends_with('/') {
                    s.push('/');
                }
                Value::unibyte_string(s)
            }
        }
    }
}

/// Per-slot metadata. Mirrors a GNU `defvar_per_buffer` entry.
#[derive(Copy, Clone, Debug)]
pub struct BufferSlotInfo {
    /// Lisp variable name (also used as the obarray symbol name).
    pub name: &'static str,
    /// Index into [`Buffer::slots`].
    pub offset: usize,
    /// Default value installed into every fresh buffer's slot.
    pub default: SlotDefault,
    /// Predicate symbol checked by `store_symval_forwarding` on
    /// write. `""` for "no check" (mirrors GNU's `Qnil` predicate
    /// slot).
    pub predicate: &'static str,
    /// Whether `kill-all-local-variables` resets this *always-local*
    /// slot back to its default. Mirrors the explicit reset block at
    /// the top of GNU's `reset_buffer_local_variables`
    /// (`buffer.c:1143-1158`), which sets `bset_major_mode`,
    /// `bset_mode_name`, `bset_invisibility_spec`, the case tables,
    /// and the keymap. Other always-local slots
    /// (`buffer-file-name`, `default-directory`, etc.) are NOT
    /// reset and this flag stays `false` for them.
    ///
    /// **For conditional slots (`local_flags_idx >= 0`), use
    /// `permanent_local` instead** — conditional slots are reset by
    /// default and `permanent_local: true` opts them out.
    pub reset_on_kill: bool,
    /// Whether this *conditional* slot should be preserved across
    /// `kill-all-local-variables`. Mirrors GNU's
    /// `buffer_permanent_local_flags[idx]` table
    /// (`buffer.c:109,4751,4767`). Only `truncate-lines` and
    /// `buffer-file-coding-system` are marked permanent in upstream
    /// GNU; both survive the major-mode change.
    ///
    /// For always-local slots this field is ignored — always-local
    /// slots are governed by `reset_on_kill`.
    pub permanent_local: bool,
    /// GNU `buffer_local_flags` index. Mirrors `buffer.c:4703-4791`:
    /// - `-1`: always-local — every buffer has its own value, the
    ///   slot is authoritative without consulting `local_flags`.
    /// - `>= 0`: conditional — the slot only holds a per-buffer
    ///   value when the corresponding bit in
    ///   [`Buffer::local_flags`] is set; otherwise reads fall
    ///   through to [`BufferManager::buffer_defaults`].
    ///
    /// Phase 10A-C only used the always-local arm; Phase 10D adds
    /// conditional slots. The numeric index also serves as the bit
    /// position in `Buffer::local_flags` (NeoMacs collapses GNU's
    /// separate offset and `local_flags_idx` to keep dispatch a
    /// single bit shift).
    pub local_flags_idx: i16,
    /// Whether `install_buffer_objfwd` should install a FORWARDED
    /// symbol for this slot's `name`. GNU's DEFVAR_PER_BUFFER entries
    /// all become forwarded symbols (`syntax-table` / `category-table`
    /// / case tables are NOT DEFVAR_PER_BUFFER — they live in the
    /// BVAR slot block but are only accessible through builtins like
    /// `Fsyntax_table`). Setting this to `false` keeps the slot in
    /// the BVAR block (same storage, GC tracing, pdump round-trip)
    /// but leaves the symbol of that name untouched, matching GNU.
    pub install_as_forwarder: bool,
}

/// The complete table of `BUFFER_OBJFWD`-style slots. Phase 10C started
/// with the four names that Phase 8b moved to slots; subsequent Phase
/// 10C commits will add more entries as additional always-local
/// variables migrate from BufferLocals into the slot table.
pub const BUFFER_SLOT_INFO: &[BufferSlotInfo] = &[
    BufferSlotInfo {
        name: "buffer-file-name",
        offset: BUFFER_SLOT_FILE_NAME,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "stringp",
        reset_on_kill: false,
        local_flags_idx: -1,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        name: "buffer-auto-save-file-name",
        offset: BUFFER_SLOT_AUTO_SAVE_FILE_NAME,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "stringp",
        reset_on_kill: false,
        local_flags_idx: -1,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        name: "buffer-read-only",
        offset: BUFFER_SLOT_READ_ONLY,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "booleanp",
        reset_on_kill: false,
        local_flags_idx: -1,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        name: "enable-multibyte-characters",
        offset: BUFFER_SLOT_ENABLE_MULTIBYTE_CHARACTERS,
        default: SlotDefault::Const(crate::emacs_core::value::Value::T),
        predicate: "booleanp",
        reset_on_kill: false,
        local_flags_idx: -1,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        name: "buffer-file-truename",
        offset: BUFFER_SLOT_FILE_TRUENAME,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "stringp",
        reset_on_kill: false,
        local_flags_idx: -1,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU buffer.c:5381 — default-directory defaults to the
        // process cwd resolved at startup. The slot table can't
        // compute that at const time so we use SlotDefault::LazyCwd
        // which calls std::env::current_dir() at install time.
        name: "default-directory",
        offset: BUFFER_SLOT_DEFAULT_DIRECTORY,
        default: SlotDefault::LazyCwd,
        predicate: "stringp",
        reset_on_kill: false,
        local_flags_idx: -1,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        name: "buffer-saved-size",
        offset: BUFFER_SLOT_SAVED_SIZE,
        default: SlotDefault::LazyFixnum(0),
        predicate: "integerp",
        reset_on_kill: false,
        local_flags_idx: -1,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        name: "buffer-backed-up",
        offset: BUFFER_SLOT_BACKED_UP,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "booleanp",
        reset_on_kill: false,
        local_flags_idx: -1,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        name: "buffer-file-format",
        offset: BUFFER_SLOT_FILE_FORMAT,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "listp",
        reset_on_kill: false,
        local_flags_idx: -1,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        name: "buffer-auto-save-file-format",
        offset: BUFFER_SLOT_AUTO_SAVE_FILE_FORMAT,
        default: SlotDefault::Const(crate::emacs_core::value::Value::T),
        predicate: "listp",
        reset_on_kill: false,
        local_flags_idx: -1,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        name: "major-mode",
        offset: BUFFER_SLOT_MAJOR_MODE,
        default: SlotDefault::LazySymbol("fundamental-mode"),
        predicate: "symbolp",
        reset_on_kill: true,
        local_flags_idx: -1,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        name: "local-minor-modes",
        offset: BUFFER_SLOT_LOCAL_MINOR_MODES,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "listp",
        reset_on_kill: false,
        local_flags_idx: -1,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        name: "mode-name",
        offset: BUFFER_SLOT_MODE_NAME,
        default: SlotDefault::LazyString("Fundamental"),
        predicate: "",
        reset_on_kill: true,
        local_flags_idx: -1,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        name: "mark-active",
        offset: BUFFER_SLOT_MARK_ACTIVE,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: -1,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        name: "point-before-scroll",
        offset: BUFFER_SLOT_POINT_BEFORE_SCROLL,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: -1,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        name: "buffer-display-count",
        offset: BUFFER_SLOT_DISPLAY_COUNT,
        default: SlotDefault::LazyFixnum(0),
        predicate: "integerp",
        reset_on_kill: false,
        local_flags_idx: -1,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        name: "buffer-display-time",
        offset: BUFFER_SLOT_DISPLAY_TIME,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: -1,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU sets this to t (a magic-bag value), not nil. The
        // legacy ALWAYS_LOCAL_BUFFER_LOCAL_NAMES table also used
        // Value::T, matching `init_buffer_once`.
        name: "buffer-invisibility-spec",
        offset: BUFFER_SLOT_INVISIBILITY_SPEC,
        default: SlotDefault::Const(crate::emacs_core::value::Value::T),
        predicate: "",
        reset_on_kill: true,
        local_flags_idx: -1,
        install_as_forwarder: true,
        permanent_local: false,
    },
    // Phase 10D conditional slots --------------------------------
    BufferSlotInfo {
        // GNU `buffer.c:4866` — fill_column defaults to 70.
        // GNU `buffer.c:4754` assigns this slot a positive index
        // in `buffer_local_flags`. NeoMacs reuses `offset` as the
        // bit index in `Buffer::local_flags`.
        name: "fill-column",
        offset: BUFFER_SLOT_FILL_COLUMN,
        default: SlotDefault::LazyFixnum(70),
        predicate: "integerp",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_FILL_COLUMN as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4848` — tab-width defaults to 8.
        name: "tab-width",
        offset: BUFFER_SLOT_TAB_WIDTH,
        default: SlotDefault::LazyFixnum(8),
        predicate: "integerp",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_TAB_WIDTH as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4867` — left-margin defaults to 0.
        name: "left-margin",
        offset: BUFFER_SLOT_LEFT_MARGIN,
        default: SlotDefault::LazyFixnum(0),
        predicate: "integerp",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_LEFT_MARGIN as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4835` — abbrev-mode defaults to nil.
        name: "abbrev-mode",
        offset: BUFFER_SLOT_ABBREV_MODE,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_ABBREV_MODE as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4836` — overwrite-mode defaults to nil.
        name: "overwrite-mode",
        offset: BUFFER_SLOT_OVERWRITE_MODE,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_OVERWRITE_MODE as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4838` — selective-display defaults to nil.
        name: "selective-display",
        offset: BUFFER_SLOT_SELECTIVE_DISPLAY,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_SELECTIVE_DISPLAY as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4839` — selective-display-ellipses defaults to t.
        name: "selective-display-ellipses",
        offset: BUFFER_SLOT_SELECTIVE_DISPLAY_ELLIPSES,
        default: SlotDefault::Const(crate::emacs_core::value::Value::T),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_SELECTIVE_DISPLAY_ELLIPSES as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4849` — truncate-lines defaults to nil.
        // GNU `buffer.c:4751` flags this as `permanent_local`; the
        // `permanent_local` semantics aren't yet wired (Phase 10D
        // step 5+ will add a dedicated field), so for now we leave
        // `reset_on_kill` false to mirror the most common path.
        name: "truncate-lines",
        offset: BUFFER_SLOT_TRUNCATE_LINES,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_TRUNCATE_LINES as i16,
        install_as_forwarder: true,
        permanent_local: true,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4850` — word-wrap defaults to nil.
        name: "word-wrap",
        offset: BUFFER_SLOT_WORD_WRAP,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_WORD_WRAP as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4851` — ctl-arrow defaults to t.
        name: "ctl-arrow",
        offset: BUFFER_SLOT_CTL_ARROW,
        default: SlotDefault::Const(crate::emacs_core::value::Value::T),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_CTL_ARROW as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4837` — auto-fill-function defaults to nil.
        name: "auto-fill-function",
        offset: BUFFER_SLOT_AUTO_FILL_FUNCTION,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_AUTO_FILL_FUNCTION as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4832` — mode-line-format defaults to "%-".
        // Layout engine reads via `effective_buffer_value`, which
        // was updated to consult the slot table directly.
        name: "mode-line-format",
        offset: BUFFER_SLOT_MODE_LINE_FORMAT,
        default: SlotDefault::LazyString("%-"),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_MODE_LINE_FORMAT as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4833` — header-line-format defaults to nil.
        name: "header-line-format",
        offset: BUFFER_SLOT_HEADER_LINE_FORMAT,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_HEADER_LINE_FORMAT as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4834` — tab-line-format defaults to nil.
        name: "tab-line-format",
        offset: BUFFER_SLOT_TAB_LINE_FORMAT,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_TAB_LINE_FORMAT as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    //
    // Phase 10D step 5 batch 2 — display/bidi/fringe/scroll-bar slots.
    BufferSlotInfo {
        // GNU `buffer.c:4852` — bidi-display-reordering defaults to t.
        name: "bidi-display-reordering",
        offset: BUFFER_SLOT_BIDI_DISPLAY_REORDERING,
        default: SlotDefault::Const(crate::emacs_core::value::Value::T),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_BIDI_DISPLAY_REORDERING as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4853` — bidi-paragraph-direction defaults to nil.
        name: "bidi-paragraph-direction",
        offset: BUFFER_SLOT_BIDI_PARAGRAPH_DIRECTION,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_BIDI_PARAGRAPH_DIRECTION as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4854` — bidi-paragraph-start-re defaults to nil.
        name: "bidi-paragraph-start-re",
        offset: BUFFER_SLOT_BIDI_PARAGRAPH_START_RE,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_BIDI_PARAGRAPH_START_RE as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4855` — bidi-paragraph-separate-re defaults to nil.
        name: "bidi-paragraph-separate-re",
        offset: BUFFER_SLOT_BIDI_PARAGRAPH_SEPARATE_RE,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_BIDI_PARAGRAPH_SEPARATE_RE as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4856` — cursor-type defaults to t.
        name: "cursor-type",
        offset: BUFFER_SLOT_CURSOR_TYPE,
        default: SlotDefault::Const(crate::emacs_core::value::Value::T),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_CURSOR_TYPE as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4857` — extra-line-spacing defaults to nil.
        name: "line-spacing",
        offset: BUFFER_SLOT_LINE_SPACING,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_LINE_SPACING as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4861` — text-conversion-style defaults to nil.
        name: "text-conversion-style",
        offset: BUFFER_SLOT_TEXT_CONVERSION_STYLE,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_TEXT_CONVERSION_STYLE as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4862` — cursor-in-non-selected-windows defaults to t.
        name: "cursor-in-non-selected-windows",
        offset: BUFFER_SLOT_CURSOR_IN_NON_SELECTED_WINDOWS,
        default: SlotDefault::Const(crate::emacs_core::value::Value::T),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_CURSOR_IN_NON_SELECTED_WINDOWS as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4871` — left-margin-cols defaults to 0.
        name: "left-margin-width",
        offset: BUFFER_SLOT_LEFT_MARGIN_WIDTH,
        default: SlotDefault::LazyFixnum(0),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_LEFT_MARGIN_WIDTH as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4872` — right-margin-cols defaults to 0.
        name: "right-margin-width",
        offset: BUFFER_SLOT_RIGHT_MARGIN_WIDTH,
        default: SlotDefault::LazyFixnum(0),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_RIGHT_MARGIN_WIDTH as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4873` — left-fringe-width defaults to nil.
        name: "left-fringe-width",
        offset: BUFFER_SLOT_LEFT_FRINGE_WIDTH,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_LEFT_FRINGE_WIDTH as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4874` — right-fringe-width defaults to nil.
        name: "right-fringe-width",
        offset: BUFFER_SLOT_RIGHT_FRINGE_WIDTH,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_RIGHT_FRINGE_WIDTH as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4875` — fringes-outside-margins defaults to nil.
        name: "fringes-outside-margins",
        offset: BUFFER_SLOT_FRINGES_OUTSIDE_MARGINS,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_FRINGES_OUTSIDE_MARGINS as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4876` — scroll-bar-width defaults to nil.
        name: "scroll-bar-width",
        offset: BUFFER_SLOT_SCROLL_BAR_WIDTH,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_SCROLL_BAR_WIDTH as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4877` — scroll-bar-height defaults to nil.
        name: "scroll-bar-height",
        offset: BUFFER_SLOT_SCROLL_BAR_HEIGHT,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_SCROLL_BAR_HEIGHT as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4878` — vertical-scroll-bar defaults to t.
        name: "vertical-scroll-bar",
        offset: BUFFER_SLOT_VERTICAL_SCROLL_BAR,
        default: SlotDefault::Const(crate::emacs_core::value::Value::T),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_VERTICAL_SCROLL_BAR as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4879` — horizontal-scroll-bar defaults to t.
        name: "horizontal-scroll-bar",
        offset: BUFFER_SLOT_HORIZONTAL_SCROLL_BAR,
        default: SlotDefault::Const(crate::emacs_core::value::Value::T),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_HORIZONTAL_SCROLL_BAR as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4880` — indicate-empty-lines defaults to nil.
        name: "indicate-empty-lines",
        offset: BUFFER_SLOT_INDICATE_EMPTY_LINES,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_INDICATE_EMPTY_LINES as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4881` — indicate-buffer-boundaries defaults to nil.
        name: "indicate-buffer-boundaries",
        offset: BUFFER_SLOT_INDICATE_BUFFER_BOUNDARIES,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_INDICATE_BUFFER_BOUNDARIES as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4882` — fringe-indicator-alist defaults to nil.
        name: "fringe-indicator-alist",
        offset: BUFFER_SLOT_FRINGE_INDICATOR_ALIST,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_FRINGE_INDICATOR_ALIST as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4883` — fringe-cursor-alist defaults to nil.
        name: "fringe-cursor-alist",
        offset: BUFFER_SLOT_FRINGE_CURSOR_ALIST,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_FRINGE_CURSOR_ALIST as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4884` — scroll-up-aggressively defaults to nil.
        name: "scroll-up-aggressively",
        offset: BUFFER_SLOT_SCROLL_UP_AGGRESSIVELY,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_SCROLL_UP_AGGRESSIVELY as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4885` — scroll-down-aggressively defaults to nil.
        name: "scroll-down-aggressively",
        offset: BUFFER_SLOT_SCROLL_DOWN_AGGRESSIVELY,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_SCROLL_DOWN_AGGRESSIVELY as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4868` — cache-long-scans defaults to t.
        name: "cache-long-scans",
        offset: BUFFER_SLOT_CACHE_LONG_SCANS,
        default: SlotDefault::Const(crate::emacs_core::value::Value::T),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_CACHE_LONG_SCANS as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4840` — abbrev-table defaults to nil.
        name: "local-abbrev-table",
        offset: BUFFER_SLOT_LOCAL_ABBREV_TABLE,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_LOCAL_ABBREV_TABLE as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4841` — display-table defaults to nil.
        name: "buffer-display-table",
        offset: BUFFER_SLOT_BUFFER_DISPLAY_TABLE,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_BUFFER_DISPLAY_TABLE as i16,
        install_as_forwarder: true,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.c:4865` — buffer-file-coding-system defaults to nil.
        // GNU buffer.c:4767 flags this as `permanent_local`; the
        // permanent semantics are deferred until step 5+ adds the
        // dedicated field.
        name: "buffer-file-coding-system",
        offset: BUFFER_SLOT_BUFFER_FILE_CODING_SYSTEM,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_BUFFER_FILE_CODING_SYSTEM as i16,
        install_as_forwarder: true,
        permanent_local: true,
    },
    // ---------- Internal-only slots (not exposed as Lisp variables) ----------
    //
    // These slots mirror GNU BVAR fields that are NOT `DEFVAR_PER_BUFFER`'d.
    // The slot offset lives in `Buffer::slots[]` so the storage, GC tracing,
    // pdump round-trip, and local_flags machinery all work uniformly, but the
    // `install_as_forwarder: false` flag tells the install loop to leave the
    // corresponding symbol alone — matching GNU where `(symbol-value
    // 'syntax-table)` signals void-variable.
    BufferSlotInfo {
        // GNU `buffer.h:391` `syntax_table_` + `buffer.c:4758` conditional
        // local_flags entry. Read via `Fsyntax_table` / written via
        // `Fset_syntax_table` (which also `SET_PER_BUFFER_VALUE_P`).
        name: "syntax-table",
        offset: BUFFER_SLOT_SYNTAX_TABLE,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_SYNTAX_TABLE as i16,
        install_as_forwarder: false,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.h:394` `category_table_` + `buffer.c:4760` conditional
        // local_flags entry. Read via `Fcategory_table` / written via
        // `Fset_category_table`.
        name: "category-table",
        offset: BUFFER_SLOT_CATEGORY_TABLE,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: BUFFER_SLOT_CATEGORY_TABLE as i16,
        install_as_forwarder: false,
        permanent_local: false,
    },
    BufferSlotInfo {
        // GNU `buffer.h:408-417` `downcase_table_` / `upcase_table_` /
        // `case_canon_table_` / `case_eqv_table_` + `buffer.c:4731-4734`
        // always-local (flag=0) entries. NeoMacs collapses the four GNU
        // slots into a single downcase char-table whose extras[0..2] hold
        // the upcase / canonicalize / equivalence subsidiary tables —
        // the same value shape `Fcurrent_case_table` returns. Read via
        // `Fcurrent_case_table` / written via `Fset_case_table`.
        name: "case-table",
        offset: BUFFER_SLOT_CASE_TABLE,
        default: SlotDefault::Const(crate::emacs_core::value::Value::NIL),
        predicate: "",
        reset_on_kill: false,
        local_flags_idx: -1,
        install_as_forwarder: false,
        permanent_local: false,
    },
];

/// Look up a [`BufferSlotInfo`] by Lisp variable name. Returns `None`
/// for non-slot-backed names.
///
/// Only returns entries with `install_as_forwarder: true` — the
/// Lisp-visible slots. Internal BVAR-only slots (`syntax-table`,
/// `category-table`, `case-table`) are addressed by their
/// dedicated slot offset constants instead of by name, matching
/// GNU where those symbols signal void-variable if read as Lisp
/// variables.
pub fn lookup_buffer_slot(name: &str) -> Option<&'static BufferSlotInfo> {
    static BUFFER_SLOT_NAME_MAP: OnceLock<FxHashMap<&'static str, &'static BufferSlotInfo>> =
        OnceLock::new();
    BUFFER_SLOT_NAME_MAP
        .get_or_init(|| {
            let mut map = FxHashMap::default();
            for info in BUFFER_SLOT_INFO {
                if info.install_as_forwarder {
                    map.insert(info.name, info);
                }
            }
            map
        })
        .get(name)
        .copied()
}

fn buffer_slot_sym_map() -> &'static [Option<&'static BufferSlotInfo>] {
    static BUFFER_SLOT_SYM_MAP: OnceLock<Box<[Option<&'static BufferSlotInfo>]>> = OnceLock::new();
    BUFFER_SLOT_SYM_MAP
        .get_or_init(|| {
            let mut entries: Vec<Option<&'static BufferSlotInfo>> = Vec::new();
            for info in BUFFER_SLOT_INFO {
                if !info.install_as_forwarder {
                    continue;
                }
                let sym_id = intern(info.name);
                let index = sym_id.0 as usize;
                if entries.len() <= index {
                    entries.resize(index + 1, None);
                }
                entries[index] = Some(info);
            }
            entries.into_boxed_slice()
        })
        .as_ref()
}

pub fn lookup_buffer_slot_by_sym_id(sym_id: SymId) -> Option<&'static BufferSlotInfo> {
    buffer_slot_sym_map()
        .get(sym_id.0 as usize)
        .and_then(|slot| *slot)
}

fn buffer_undo_list_sym() -> SymId {
    static SYM: OnceLock<SymId> = OnceLock::new();
    *SYM.get_or_init(|| intern("buffer-undo-list"))
}

/// Coerce a write value to fit a slot's predicate. Mirrors GNU's
/// `store_symval_forwarding` predicate path:
///
///  - `"stringp"`: accept strings or nil; reject everything else by
///    keeping the previous slot value (a real GNU build would signal
///    `wrong-type-argument`; we conservatively no-op so legacy tests
///    that wrote `t` or numbers don't blow up here — the assign hot
///    path will eventually reject them once predicate dispatch is
///    formalised).
///  - `"booleanp"`: canonicalise any truthy value to `Value::T` and
///    any nil to `Value::NIL`. Used for slots like `buffer-read-only`
///    and `enable-multibyte-characters` whose GNU equivalents are
///    declared `BVAR_PER_BUFFER_TYPE_BOOL`.
///  - Anything else (including `""`): store as-is.
/// Look up `key` in a buffer-local alist. Returns the cdr of the
/// matching `(key . val)` pair, or `None` if absent. Mirrors GNU
/// `assq_no_quit` (`fns.c:1520-1543`) used by `Flocal_variable_p`
/// at `data.c:2409`.
pub(crate) fn find_local_var_alist_entry(
    alist: crate::emacs_core::value::Value,
    key: crate::emacs_core::value::Value,
) -> Option<crate::emacs_core::value::Value> {
    let mut cursor = alist;
    while cursor.is_cons() {
        let entry = cursor.cons_car();
        cursor = cursor.cons_cdr();
        if entry.is_cons() && crate::emacs_core::value::eq_value(&entry.cons_car(), &key) {
            return Some(entry.cons_cdr());
        }
    }
    None
}

/// Set `key` to `value` in a buffer-local alist. If `key` already
/// has an entry, mutate its cdr in place so any BLV valcell
/// pointing at the cell sees the new value without re-swapping.
/// Otherwise prepend a fresh `(key . value)` cons to the alist.
/// Mirrors the SYMBOL_LOCALIZED arm of GNU `set_internal` at
/// `data.c:1687-1762`.
pub(crate) fn set_local_var_alist_entry(
    alist: &mut crate::emacs_core::value::Value,
    key: crate::emacs_core::value::Value,
    value: crate::emacs_core::value::Value,
) {
    use crate::emacs_core::value::{Value, eq_value};
    let mut cursor = *alist;
    while cursor.is_cons() {
        let entry = cursor.cons_car();
        cursor = cursor.cons_cdr();
        if entry.is_cons() && eq_value(&entry.cons_car(), &key) {
            entry.set_cdr(value);
            return;
        }
    }
    let cell = Value::cons(key, value);
    *alist = Value::cons(cell, *alist);
}

/// Remove `key` from a buffer-local alist in place. Mirrors GNU's
/// `Fdelq`-over-`Fassq` pattern in `Fkill_local_variable`
/// (`data.c:2349-2378`).
pub(crate) fn remove_local_var_alist_entry(
    alist: &mut crate::emacs_core::value::Value,
    key: crate::emacs_core::value::Value,
) {
    use crate::emacs_core::value::{Value, eq_value};
    let mut head = *alist;
    let mut prev: Option<Value> = None;
    let mut cursor = head;
    while cursor.is_cons() {
        let entry = cursor.cons_car();
        let next = cursor.cons_cdr();
        if entry.is_cons() && eq_value(&entry.cons_car(), &key) {
            match prev {
                Some(p) => p.set_cdr(next),
                None => head = next,
            }
        } else {
            prev = Some(cursor);
        }
        cursor = next;
    }
    *alist = head;
}

/// Filter a `(perm-hook ...)` value to keep only entries that
/// are themselves `permanent-local-hook` per their symbol property,
/// plus the `t` element if present. Mirrors GNU
/// `reset_buffer_local_variables`'s permanent-local-hook handling
/// at `buffer.c:1308-1335`. Used by [`Buffer::kill_all_local_variables`]
/// when walking `local_var_alist` for LOCALIZED hook bindings.
pub(crate) fn preserve_partial_permanent_local_hook_value(
    obarray: &crate::emacs_core::symbol::Obarray,
    value: crate::emacs_core::value::Value,
) -> crate::emacs_core::value::Value {
    use crate::emacs_core::value::Value;
    if !value.is_cons() {
        return value;
    }
    let mut preserved = Vec::new();
    let mut cursor = value;
    while cursor.is_cons() {
        let elt = cursor.cons_car();
        cursor = cursor.cons_cdr();
        if elt.is_symbol_named("t")
            || elt.as_symbol_name().is_some_and(|name| {
                obarray
                    .get_property(name, "permanent-local-hook")
                    .is_some_and(|prop| !prop.is_nil())
            })
        {
            preserved.push(elt);
        }
    }
    Value::list(preserved)
}

pub(crate) fn coerce_to_slot(
    info: &BufferSlotInfo,
    value: crate::emacs_core::value::Value,
    current: crate::emacs_core::value::Value,
) -> crate::emacs_core::value::Value {
    use crate::emacs_core::value::{Value, ValueKind};
    match info.predicate {
        "stringp" => match value.kind() {
            ValueKind::String => value,
            ValueKind::Nil => Value::NIL,
            _ => current,
        },
        "booleanp" => {
            if value.is_truthy() {
                Value::T
            } else {
                Value::NIL
            }
        }
        _ => value,
    }
}

// ---------------------------------------------------------------------------
// BufferId
// ---------------------------------------------------------------------------

/// Opaque, cheaply-copyable identifier for a buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BufferId(pub u64);

// ---------------------------------------------------------------------------
// InsertionType
// ---------------------------------------------------------------------------

/// Controls whether a marker advances when text is inserted exactly at its
/// position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertionType {
    /// Marker stays before the new text (does NOT advance).
    Before,
    /// Marker moves after the new text (advances).
    After,
}

// ---------------------------------------------------------------------------
// BufferStateMarkers
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct BufferStateMarkers {
    pub pt_marker: u64,
    pub begv_marker: u64,
    pub zv_marker: u64,
    /// Non-Lisp-visible MarkerObj pointers for the three state markers.
    /// Allocated once per buffer in `ensure_buffer_state_markers` and
    /// reused on every `record_buffer_state_markers` re-registration
    /// (via `chain_unlink` + `register_marker`) so the intrusive chain
    /// precondition is upheld. These pointers are rooted in GC via the
    /// `TaggedHeap::marker_ptrs` registry (all allocations land there).
    pub pt_marker_ptr: *mut crate::tagged::header::MarkerObj,
    pub begv_marker_ptr: *mut crate::tagged::header::MarkerObj,
    pub zv_marker_ptr: *mut crate::tagged::header::MarkerObj,
}

impl PartialEq for BufferStateMarkers {
    fn eq(&self, other: &Self) -> bool {
        self.pt_marker == other.pt_marker
            && self.begv_marker == other.begv_marker
            && self.zv_marker == other.zv_marker
    }
}
impl Eq for BufferStateMarkers {}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LabeledRestrictionLabel {
    Outermost,
    User(Value),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LabeledRestriction {
    pub label: LabeledRestrictionLabel,
    pub beg_marker: u64,
    pub end_marker: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SavedRestrictionKind {
    None,
    Markers { beg_marker: u64, end_marker: u64 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct SavedRestrictionState {
    pub buffer_id: BufferId,
    pub restriction: SavedRestrictionKind,
    pub labeled_restrictions: Option<Vec<LabeledRestriction>>,
}

impl SavedRestrictionState {
    pub fn trace_roots(&self, roots: &mut Vec<Value>) {
        if let Some(restrictions) = &self.labeled_restrictions {
            for restriction in restrictions {
                if let LabeledRestrictionLabel::User(label) = restriction.label {
                    roots.push(label);
                }
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutermostRestrictionResetState {
    pub affected_buffers: Vec<BufferId>,
}

// ---------------------------------------------------------------------------
// Buffer
// ---------------------------------------------------------------------------

/// A single text buffer with point, mark, narrowing, markers, and local vars.
#[derive(Clone)]
pub struct Buffer {
    /// Unique identifier.
    pub id: BufferId,
    /// Buffer name (e.g. `"*scratch*"`). Mirrors GNU `struct buffer.name_`.
    pub name: Value,
    /// Base buffer when this is an indirect buffer.
    pub base_buffer: Option<BufferId>,
    /// The underlying text storage.
    pub text: BufferText,
    /// Point — the current cursor character position.
    pub pt: usize,
    /// Point — the current cursor byte position.
    pub pt_byte: usize,
    /// Mark — optional character position for region operations.
    pub mark: Option<usize>,
    /// Mark — optional byte position for region operations.
    pub mark_byte: Option<usize>,
    /// Beginning of accessible (narrowed) portion (char pos, inclusive).
    pub begv: usize,
    /// Beginning of accessible (narrowed) portion (byte pos, inclusive).
    pub begv_byte: usize,
    /// End of accessible (narrowed) portion (char pos, exclusive).
    pub zv: usize,
    /// End of accessible (narrowed) portion (byte pos, exclusive).
    pub zv_byte: usize,
    /// GNU `BUF_AUTOSAVE_MODIFF`: recent auto-save state is
    /// `save_modiff < autosave_modified_tick`.
    pub autosave_modified_tick: i64,
    /// GNU `last_window_start`: start position of the most recently
    /// disconnected window that showed this buffer.
    pub last_window_start: usize,
    /// GNU `last_selected_window`: most recently selected live window showing
    /// this buffer, when known.
    pub last_selected_window: Option<WindowId>,
    /// GNU `inhibit_buffer_hooks`: suppress buffer lifecycle hooks for
    /// temporary/internal buffers.
    pub inhibit_buffer_hooks: bool,
    /// GNU-style noncurrent PT/BEGV/ZV markers for buffers that share text.
    pub state_markers: Option<BufferStateMarkers>,
    /// `local_var_alist` — list of `(SYMBOL . VALUE)` per-buffer
    /// bindings for `SYMBOL_LOCALIZED` variables. Mirrors GNU
    /// `BVAR(buffer, local_var_alist)` (`buffer.h:362`). This is
    /// the single source of truth for all Lisp-side per-buffer
    /// bindings that are not slot-backed (FORWARDED) and not the
    /// special buffer-undo-list (which has its own SharedUndoState).
    pub local_var_alist: crate::emacs_core::value::Value,
    /// `BVAR(buffer, keymap)` — the buffer's local keymap
    /// (`buffer.h:385`). `Value::NIL` when no local keymap is set.
    pub keymap: crate::emacs_core::value::Value,
    /// `BUFFER_OBJFWD` slot table — per-buffer storage for variables
    /// that are forwarded into the C-side `struct buffer` in GNU.
    /// Mirrors the union of GNU's `Lisp_Object` slot fields in
    /// `buffer.h:319-462`. Indexed by [`crate::emacs_core::forward::LispBufferObjFwd::offset`].
    ///
    /// Phase 8a of the symbol-redirect refactor adds the slot table.
    /// Phase 8b will migrate the hardcoded fields ([`Self::file_name`],
    /// [`Self::auto_save_file_name`], [`Self::read_only`],
    /// [`Self::multibyte`]) into slots and remove the duplicates.
    pub slots: [crate::emacs_core::value::Value; BUFFER_SLOT_COUNT],
    /// Per-slot "is buffer-local in this buffer" bitmap. Bit `N` is
    /// set when this buffer has its own local value for the slot at
    /// offset `N`. Mirrors GNU `b->local_flags[]` (`buffer.h:646`,
    /// `char[MAX_PER_BUFFER_VARS]`); we use a `u64` bitmap because
    /// `BUFFER_SLOT_COUNT == 64`.
    ///
    /// **Semantics** (mirrors `set_internal` SYMBOL_FORWARDED arm at
    /// `data.c:1764-1791`):
    /// - Always-local slots (`local_flags_idx == -1`) ignore this
    ///   bitmap entirely; the slot is authoritative.
    /// - Conditional slots (`local_flags_idx >= 0`): a read returns
    ///   `slots[N]` iff bit `N` is set, otherwise the global default
    ///   from `Context::buffer_defaults[N]`. A write sets the bit
    ///   and writes the slot.
    ///
    /// Phase 10D wires the bitmap up; Phase 10A-C only used the
    /// always-local arm.
    pub local_flags: u64,
    /// Overlays attached to the buffer.
    pub overlays: OverlayList,
    /// Shared undo owner for this text.
    pub undo_state: SharedUndoState,
}

impl Buffer {
    /// Return the chartable Value stored in this buffer's syntax-table
    /// slot. Mirrors GNU `BVAR (buf, syntax_table)` — reading directly
    /// from `buffer->syntax_table` without any compiled shadow form.
    /// Falls back to `Value::NIL` for fresh buffers; callers that need
    /// the standard defaults should go through
    /// `current_buffer_syntax_table_object_in_buffers`, which seeds
    /// the slot on first access.
    pub fn syntax_chartable(&self) -> Value {
        self.slots[BUFFER_SLOT_SYNTAX_TABLE]
    }
}

impl Buffer {
    // -- Construction --------------------------------------------------------

    /// Create a new, empty buffer.
    pub fn new(id: BufferId, name: Value) -> Self {
        assert!(name.is_string(), "buffer name must be a Lisp string");
        Self {
            id,
            name,
            base_buffer: None,
            text: BufferText::new(),
            pt: 0,
            pt_byte: 0,
            mark: None,
            mark_byte: None,
            begv: 0,
            begv_byte: 0,
            zv: 0,
            zv_byte: 0,
            autosave_modified_tick: 1,
            last_window_start: 1,
            last_selected_window: None,
            inhibit_buffer_hooks: false,
            state_markers: None,
            local_var_alist: crate::emacs_core::value::Value::NIL,
            keymap: crate::emacs_core::value::Value::NIL,
            slots: {
                // Phase 10C: seed every slot from BUFFER_SLOT_INFO.
                // Mirrors GNU's `reset_buffer` (`buffer.c:1188`)
                // copying `buffer_defaults` into a fresh buffer.
                let mut s = [crate::emacs_core::value::Value::NIL; BUFFER_SLOT_COUNT];
                for info in BUFFER_SLOT_INFO {
                    s[info.offset] = info.default.to_value();
                }
                s
            },
            // Phase 10D: every fresh buffer starts with no conditional
            // local-flag bits set. Reads of conditional slots fall
            // through to `Context::buffer_defaults` until a write or
            // `make-local-variable` flips the bit.
            local_flags: 0,
            overlays: OverlayList::new(),
            undo_state: SharedUndoState::new(),
        }
    }

    pub fn name_value(&self) -> Value {
        self.name
    }

    pub fn name_runtime_string_owned(&self) -> String {
        self.name
            .as_runtime_string_owned()
            .expect("buffer name must be a Lisp string")
    }

    pub fn has_name(&self, name: &str) -> bool {
        self.name_runtime_string_owned() == name
    }

    pub fn name_starts_with_space(&self) -> bool {
        self.name_runtime_string_owned().starts_with(' ')
    }

    pub fn set_name_value(&mut self, name: Value) {
        assert!(name.is_string(), "buffer name must be a Lisp string");
        self.name = name;
    }

    pub fn set_name_runtime_string(&mut self, name: impl Into<String>) {
        self.name = Value::string(name.into());
    }

    // -- Phase 10D: per-slot local-flag bitmap accessors. Conditional
    // -- BUFFER_OBJFWD slots (those with `local_flags_idx >= 0`) only
    // -- hold buffer-local values when their bit is set in
    // -- [`Self::local_flags`]. Always-local slots ignore the bitmap.
    // -- Mirrors GNU's `PER_BUFFER_VALUE_P` / `SET_PER_BUFFER_VALUE_P`
    // -- (`buffer.h:1640-1645`).

    /// Test whether the conditional slot at `offset` has a per-buffer
    /// local value installed in this buffer. Mirrors GNU
    /// `PER_BUFFER_VALUE_P` (`buffer.h:1640`).
    #[inline]
    pub fn slot_local_flag(&self, offset: usize) -> bool {
        debug_assert!(offset < BUFFER_SLOT_COUNT);
        (self.local_flags >> (offset as u32)) & 1 != 0
    }

    /// Set or clear the conditional-local flag for the slot at
    /// `offset`. Mirrors GNU `SET_PER_BUFFER_VALUE_P` (`buffer.h:1645`).
    #[inline]
    pub fn set_slot_local_flag(&mut self, offset: usize, on: bool) {
        debug_assert!(offset < BUFFER_SLOT_COUNT);
        let bit = 1u64 << (offset as u32);
        if on {
            self.local_flags |= bit;
        } else {
            self.local_flags &= !bit;
        }
    }

    // -- Slot accessors for the four hardcoded fields targeted by
    // -- Phase 8b of the symbol-redirect refactor. `file_name` now
    // -- lives in [`Self::slots`] at [`BUFFER_SLOT_FILE_NAME`],
    // -- mirroring GNU's `BVAR(buffer, filename)`. The other three
    // -- (`auto_save_file_name` / `read_only` / `multibyte`) still
    // -- have struct fields during the staggered migration.

    /// Read `buffer-file-name` as the underlying Lisp value, mirroring GNU
    /// `BVAR(buf, filename)` (`buffer.h:319`).
    pub fn file_name_value(&self) -> Value {
        self.slots[BUFFER_SLOT_FILE_NAME]
    }

    /// Clone `buffer-file-name` as an owned runtime string.
    /// This is a boundary helper for filesystem-facing code.
    pub fn file_name_runtime_string_owned(&self) -> Option<String> {
        self.slots[BUFFER_SLOT_FILE_NAME].as_runtime_string_owned()
    }

    pub fn file_name_lisp_string(&self) -> Option<&'static crate::heap_types::LispString> {
        self.file_name_value().as_lisp_string()
    }

    /// Write `buffer-file-name`. Mirrors GNU `bset_filename`
    /// (`buffer.c`). The slot stores either a Lisp string or `nil`.
    pub fn set_file_name_value(&mut self, v: Value) {
        assert!(
            v.is_nil() || v.is_string(),
            "buffer-file-name must be nil or a Lisp string"
        );
        self.slots[BUFFER_SLOT_FILE_NAME] = v;
    }

    /// Read `buffer-auto-save-file-name` as the underlying Lisp value,
    /// mirroring GNU `BVAR(buf, auto_save_file_name)` (`buffer.h:323`).
    pub fn auto_save_file_name_value(&self) -> Value {
        self.slots[BUFFER_SLOT_AUTO_SAVE_FILE_NAME]
    }

    /// Clone `buffer-auto-save-file-name` as an owned runtime string.
    /// This is a boundary helper for filesystem-facing code.
    pub fn auto_save_file_name_runtime_string_owned(&self) -> Option<String> {
        self.slots[BUFFER_SLOT_AUTO_SAVE_FILE_NAME].as_runtime_string_owned()
    }

    pub fn auto_save_file_name_lisp_string(
        &self,
    ) -> Option<&'static crate::heap_types::LispString> {
        self.auto_save_file_name_value().as_lisp_string()
    }

    /// Write `buffer-auto-save-file-name`. Mirrors GNU
    /// `bset_auto_save_file_name`. The slot stores either a Lisp string or
    /// `nil`.
    pub fn set_auto_save_file_name_value(&mut self, v: Value) {
        assert!(
            v.is_nil() || v.is_string(),
            "buffer-auto-save-file-name must be nil or a Lisp string"
        );
        self.slots[BUFFER_SLOT_AUTO_SAVE_FILE_NAME] = v;
    }

    /// Read `buffer-read-only`, mirroring GNU
    /// `BVAR(buf, read_only)`. A non-nil slot maps to `true`.
    pub fn get_read_only(&self) -> bool {
        self.slots[BUFFER_SLOT_READ_ONLY].is_truthy()
    }

    /// Write `buffer-read-only`. `true` stores `Value::T`, `false`
    /// stores `Value::NIL`.
    pub fn set_read_only_value(&mut self, v: bool) {
        self.slots[BUFFER_SLOT_READ_ONLY] = if v { Value::T } else { Value::NIL };
    }

    /// Read `enable-multibyte-characters`, mirroring GNU
    /// `BVAR(buf, enable_multibyte_characters)`. A non-nil slot
    /// maps to `true`.
    pub fn get_multibyte(&self) -> bool {
        self.slots[BUFFER_SLOT_ENABLE_MULTIBYTE_CHARACTERS].is_truthy()
    }

    /// Write `enable-multibyte-characters`. `true` stores
    /// `Value::T`, `false` stores `Value::NIL`.
    pub fn set_multibyte_value(&mut self, v: bool) {
        self.text.set_multibyte(v);
        self.slots[BUFFER_SLOT_ENABLE_MULTIBYTE_CHARACTERS] = if v { Value::T } else { Value::NIL };
    }

    // -- Point queries -------------------------------------------------------

    /// Current point as an Emacs byte position.
    pub fn point_byte(&self) -> usize {
        self.pt_byte
    }

    /// Legacy point accessor retained while buffer internals are byte-only.
    pub fn point(&self) -> usize {
        self.point_byte()
    }

    /// Current point converted to a character position.
    pub fn point_char(&self) -> usize {
        self.pt
    }

    /// Beginning of the accessible portion (Emacs byte position).
    pub fn point_min_byte(&self) -> usize {
        self.begv_byte
    }

    /// Beginning of the accessible portion (character position).
    pub fn point_min_char(&self) -> usize {
        self.begv
    }

    /// Legacy narrowing accessor retained while buffer internals are byte-only.
    pub fn point_min(&self) -> usize {
        self.point_min_byte()
    }

    /// End of the accessible portion (Emacs byte position).
    pub fn point_max_byte(&self) -> usize {
        self.zv_byte
    }

    /// End of the accessible portion (character position).
    pub fn point_max_char(&self) -> usize {
        self.zv
    }

    /// Total number of characters in the buffer text.
    pub fn total_chars(&self) -> usize {
        self.text.char_count()
    }

    /// Total number of Emacs bytes in the buffer text.
    pub fn total_bytes(&self) -> usize {
        self.text.emacs_byte_len()
    }

    /// Convert a 0-based character position to an Emacs byte position,
    /// clamping to the buffer text length.
    pub fn char_to_byte_clamped(&self, char_pos: usize) -> usize {
        self.text
            .char_to_emacs_byte(char_pos.min(self.total_chars()))
    }

    /// Convert a 1-based Lisp character position to a byte position, clamping
    /// to the full buffer.
    pub fn lisp_pos_to_byte(&self, lisp_pos: i64) -> usize {
        let char_pos = if lisp_pos > 0 {
            lisp_pos as usize - 1
        } else {
            0
        };
        self.char_to_byte_clamped(char_pos)
    }

    /// Convert a 1-based Lisp character position to a byte position, clamping
    /// to the accessible region.
    pub fn lisp_pos_to_accessible_byte(&self, lisp_pos: i64) -> usize {
        let char_pos = if lisp_pos > 0 {
            lisp_pos as usize - 1
        } else {
            0
        };
        let clamped_char = char_pos.clamp(self.point_min_char(), self.point_max_char());
        self.text.char_to_emacs_byte(clamped_char)
    }

    /// Convert a 1-based Lisp character position to a byte position, clamping
    /// to the *full* buffer range (ignoring narrowing).
    ///
    /// GNU Emacs: `set-marker` clamps to the full buffer, not the narrowed
    /// region, so markers can be placed outside the accessible range.
    pub fn lisp_pos_to_full_buffer_byte(&self, lisp_pos: i64) -> usize {
        let char_pos = if lisp_pos > 0 {
            lisp_pos as usize - 1
        } else {
            0
        };
        let clamped_char = char_pos.min(self.total_chars());
        self.text.char_to_emacs_byte(clamped_char)
    }

    /// Legacy narrowing accessor retained for Lisp-facing callers.
    pub fn point_max(&self) -> usize {
        self.point_max_byte()
    }

    // -- Point movement ------------------------------------------------------

    /// Set point in Emacs bytes, clamping to the accessible region `[begv, zv]`.
    pub fn goto_byte(&mut self, pos: usize) {
        self.pt_byte = pos.clamp(self.begv_byte, self.zv_byte);
        self.pt = if self.pt_byte == self.begv_byte {
            self.begv
        } else if self.pt_byte == self.zv_byte {
            self.zv
        } else {
            self.text.emacs_byte_to_char(self.pt_byte)
        };
    }

    /// Legacy point setter retained while buffer internals are byte-only.
    pub fn goto_char(&mut self, pos: usize) {
        self.goto_byte(pos);
    }

    // -- Undo helpers --------------------------------------------------------

    /// Get the current `buffer-undo-list` value from buffer-local properties.
    pub fn get_undo_list(&self) -> Value {
        self.undo_state.list()
    }

    /// Store the `buffer-undo-list` value into the shared undo
    /// state. The SharedUndoState is the single source of truth —
    /// reads of `buffer-undo-list` route through
    /// [`Self::get_undo_list`] regardless of which Buffer in an
    /// indirect-buffer chain is queried.
    pub fn set_undo_list(&mut self, value: Value) {
        self.undo_state.set_list(value);
    }

    // -- Text queries --------------------------------------------------------

    pub fn emacs_byte_to_storage_byte(&self, pos: usize) -> usize {
        self.text
            .emacs_byte_to_storage_byte(pos.min(self.total_bytes()))
    }

    pub fn storage_byte_to_emacs_byte(&self, pos: usize) -> usize {
        self.text
            .storage_byte_to_emacs_byte(pos.min(self.text.len()))
    }

    pub fn copy_emacs_bytes_to(&self, start: usize, end: usize, out: &mut Vec<u8>) {
        let total = self.total_bytes();
        let s = start.min(total);
        let e = end.max(s).min(total);
        self.text.copy_emacs_bytes_to(s, e, out);
    }

    /// Return a raw Emacs-byte copy of the range `[start, end)`.
    pub fn buffer_substring_bytes(&self, start: usize, end: usize) -> Vec<u8> {
        let mut out = Vec::new();
        self.copy_emacs_bytes_to(start, end, &mut out);
        out
    }

    /// Return the range `[start, end)` as a Lisp string preserving the
    /// buffer's multibyte/unibyte semantics.
    pub fn buffer_substring_lisp_string(
        &self,
        start: usize,
        end: usize,
    ) -> crate::heap_types::LispString {
        let bytes = self.buffer_substring_bytes(start, end);
        if self.get_multibyte() {
            crate::heap_types::LispString::from_emacs_bytes(bytes)
        } else {
            crate::heap_types::LispString::from_unibyte(bytes)
        }
    }

    /// Return the range `[start, end)` as a Lisp value string.
    pub fn buffer_substring_value(&self, start: usize, end: usize) -> Value {
        Value::heap_string(self.buffer_substring_lisp_string(start, end))
    }

    /// Return a `String` copy of the Emacs-byte range `[start, end)`.
    pub fn buffer_substring(&self, start: usize, end: usize) -> String {
        let bytes = self.buffer_substring_bytes(start, end);
        crate::emacs_core::string_escape::emacs_bytes_to_storage_string(
            &bytes,
            self.get_multibyte(),
        )
    }

    /// Return the entire accessible portion of the buffer as a `String`.
    pub fn buffer_string(&self) -> String {
        self.buffer_substring(self.begv_byte, self.zv_byte)
    }

    /// Emacs-byte length of the accessible portion.
    pub fn buffer_size(&self) -> usize {
        self.zv_byte - self.begv_byte
    }

    /// Character at Emacs byte position `pos`, or `None` if out of range.
    pub fn char_after(&self, pos: usize) -> Option<char> {
        self.char_code_after(pos).and_then(char::from_u32)
    }

    /// Emacs character code at Emacs byte position `pos`, or `None` if out of range.
    pub fn char_code_after(&self, pos: usize) -> Option<u32> {
        if pos >= self.total_bytes() {
            return None;
        }
        let storage_pos = self.text.emacs_byte_to_storage_byte(pos);
        self.text.char_code_at(storage_pos)
    }

    /// Character immediately before Emacs byte position `pos`, or `None`.
    pub fn char_before(&self, pos: usize) -> Option<char> {
        self.char_code_before(pos).and_then(char::from_u32)
    }

    /// Emacs character code immediately before Emacs byte position `pos`, or `None`.
    pub fn char_code_before(&self, pos: usize) -> Option<u32> {
        if pos == 0 || pos > self.total_bytes() {
            return None;
        }
        let prior_char = self.text.emacs_byte_to_char(pos);
        if prior_char == 0 {
            return None;
        }
        let prior_byte = self.text.char_to_emacs_byte(prior_char - 1);
        let storage_pos = self.text.emacs_byte_to_storage_byte(prior_byte);
        self.text.char_code_at(storage_pos)
    }

    /// Storage-byte width of the character starting at Emacs byte position `pos`.
    pub fn char_after_storage_len(&self, pos: usize) -> Option<usize> {
        if pos >= self.total_bytes() {
            return None;
        }
        let storage_pos = self.text.emacs_byte_to_storage_byte(pos);
        let char_idx = self.text.emacs_byte_to_char(pos);
        Some(self.text.char_to_byte(char_idx + 1) - storage_pos)
    }

    /// Storage-byte width of the character ending at Emacs byte position `pos`.
    pub fn char_before_storage_len(&self, pos: usize) -> Option<usize> {
        if pos == 0 || pos > self.total_bytes() {
            return None;
        }
        let prior_char = self.text.emacs_byte_to_char(pos);
        if prior_char == 0 {
            return None;
        }
        let prior_byte = self.text.char_to_byte(prior_char - 1);
        let storage_pos = self.text.emacs_byte_to_storage_byte(pos);
        Some(storage_pos - prior_byte)
    }

    /// Emacs-byte width of the character starting at `pos`.
    pub fn char_after_emacs_len(&self, pos: usize) -> Option<usize> {
        if pos >= self.total_bytes() {
            return None;
        }
        let char_idx = self.text.emacs_byte_to_char(pos);
        Some(self.text.char_to_emacs_byte(char_idx + 1) - pos)
    }

    /// Emacs-byte width of the character ending at `pos`.
    pub fn char_before_emacs_len(&self, pos: usize) -> Option<usize> {
        if pos == 0 || pos > self.total_bytes() {
            return None;
        }
        let prior_char = self.text.emacs_byte_to_char(pos);
        if prior_char == 0 {
            return None;
        }
        let prior_byte = self.text.char_to_emacs_byte(prior_char - 1);
        Some(pos - prior_byte)
    }

    // -- Narrowing -----------------------------------------------------------

    /// Restrict the accessible portion to the Emacs-byte range `[start, end)`.
    pub fn narrow_to_byte_region(&mut self, start: usize, end: usize) {
        let total = self.total_bytes();
        let s = start.min(total);
        let e = end.clamp(s, total);
        let total_chars = self.text.char_count();
        self.begv_byte = s;
        self.begv = self.text.emacs_byte_to_char(s);
        self.zv_byte = e;
        self.zv = if e == total {
            total_chars
        } else {
            self.text.emacs_byte_to_char(e)
        };
        // Clamp point into the new accessible region.
        self.goto_byte(self.pt_byte);
    }

    /// Legacy narrowing API retained while buffer internals are byte-only.
    pub fn narrow_to_region(&mut self, start: usize, end: usize) {
        self.narrow_to_byte_region(start, end);
    }

    /// Remove narrowing — make the entire buffer accessible again.
    pub fn widen(&mut self) {
        self.narrow_to_byte_region(0, self.total_bytes());
    }

    pub fn register_marker(
        &mut self,
        marker_ptr: *mut crate::tagged::header::MarkerObj,
        marker_id: u64,
        pos: usize,
        insertion_type: InsertionType,
    ) {
        let clamped = pos.min(self.total_bytes());
        let char_pos = if clamped == self.begv_byte {
            self.begv
        } else if clamped == self.zv_byte {
            self.zv
        } else {
            self.text.emacs_byte_to_char(clamped)
        };
        self.text.register_marker(
            marker_ptr,
            self.id,
            marker_id,
            clamped,
            char_pos,
            insertion_type,
        );
    }

    pub fn remove_marker_entry(&mut self, marker_id: u64) {
        self.text.remove_marker(marker_id);
    }

    pub fn update_marker_insertion_type(&mut self, marker_id: u64, insertion_type: InsertionType) {
        self.text
            .update_marker_insertion_type(marker_id, insertion_type);
    }

    pub fn advance_markers_at(&mut self, pos: usize, byte_len: usize, char_len: usize) {
        self.text.advance_markers_at(pos, byte_len, char_len);
    }

    pub fn clear_marker_entries(&mut self) {
        self.text.clear_markers();
    }

    // -- Mark ----------------------------------------------------------------

    /// Set the mark to the byte position `pos`.
    pub fn set_mark_byte(&mut self, pos: usize) {
        let clamped = pos.clamp(self.begv_byte, self.zv_byte);
        let char_pos = if clamped == self.begv_byte {
            self.begv
        } else if clamped == self.zv_byte {
            self.zv
        } else {
            self.text.emacs_byte_to_char(clamped)
        };
        self.mark = Some(char_pos);
        self.mark_byte = Some(clamped);
    }

    /// Legacy mark setter retained while buffer internals are byte-only.
    pub fn set_mark(&mut self, pos: usize) {
        self.set_mark_byte(pos);
    }

    /// Return the mark, if set.
    pub fn mark_byte(&self) -> Option<usize> {
        self.mark_byte
    }

    /// Return the mark character position, if set.
    pub fn mark_char(&self) -> Option<usize> {
        self.mark
    }

    /// Legacy mark accessor retained while buffer internals are byte-only.
    pub fn mark(&self) -> Option<usize> {
        self.mark_byte()
    }

    // -- Modified flag -------------------------------------------------------

    pub fn modified_tick(&self) -> i64 {
        self.text.modified_tick()
    }

    pub fn chars_modified_tick(&self) -> i64 {
        self.text.chars_modified_tick()
    }

    pub fn save_modified_tick(&self) -> i64 {
        self.text.save_modified_tick()
    }

    pub fn is_modified(&self) -> bool {
        self.save_modified_tick() < self.modified_tick()
    }

    pub fn modified_state_value(&self) -> Value {
        if self.save_modified_tick() < self.modified_tick() {
            if self.autosave_modified_tick == self.modified_tick() {
                Value::symbol("autosaved")
            } else {
                Value::T
            }
        } else {
            Value::NIL
        }
    }

    pub fn recent_auto_save_p(&self) -> bool {
        self.save_modified_tick() < self.autosave_modified_tick
    }

    pub fn set_modified(&mut self, flag: bool) {
        if flag {
            if self.save_modified_tick() >= self.modified_tick() {
                self.text.increment_modified_tick(1);
            }
        } else {
            self.text.set_save_modified_tick(self.modified_tick());
        }
    }

    pub fn restore_modified_state(&mut self, flag: Value) -> Value {
        if flag.is_nil() {
            self.text.set_save_modified_tick(self.modified_tick());
        } else {
            if self.save_modified_tick() >= self.modified_tick() {
                self.text.increment_modified_tick(1);
            }
            if flag == Value::symbol("autosaved") {
                self.autosave_modified_tick = self.modified_tick();
            }
        }
        flag
    }

    pub fn mark_auto_saved(&mut self) {
        self.autosave_modified_tick = self.modified_tick();
    }

    // -- Buffer-local variables ----------------------------------------------

    /// Write a per-buffer binding. Mirrors GNU `set_internal`
    /// SYMBOL_FORWARDED arm (`data.c:1774-1786`) for slot-backed
    /// names and the SYMBOL_LOCALIZED arm for everything else:
    ///
    /// * Slot-backed (BUFFER_OBJFWD) — write
    ///   `slots[offset]`, setting the per-buffer local-flags bit
    ///   for conditional slots (`SET_PER_BUFFER_VALUE_P`).
    /// * `buffer-undo-list` — writes to [`SharedUndoState`], which
    ///   is the single source of truth shared across indirect
    ///   buffers.
    /// * Everything else — intern `name` to a SymId and store the
    ///   binding in [`Self::local_var_alist`]. Existing entries
    ///   are mutated in place so any [`LispBufferLocalValue`]
    ///   `valcell` still points at the same cons. New entries are
    ///   prepended to the alist.
    pub fn set_buffer_local(&mut self, name: &str, value: Value) {
        self.set_buffer_local_by_sym_id(intern(name), value);
    }

    pub fn set_buffer_local_by_sym_id(&mut self, sym_id: SymId, value: Value) {
        if let Some(info) = lookup_buffer_slot_by_sym_id(sym_id) {
            self.slots[info.offset] = coerce_to_slot(info, value, self.slots[info.offset]);
            if info.local_flags_idx >= 0 {
                self.set_slot_local_flag(info.offset, true);
            }
            return;
        }
        if sym_id == buffer_undo_list_sym() {
            self.undo_state.set_list(value);
            if value.is_nil() {
                self.undo_state.set_recorded_first_change(false);
            }
            return;
        }
        set_local_var_alist_entry(&mut self.local_var_alist, Value::from_sym_id(sym_id), value);
    }

    /// Mark a per-buffer binding as void. Slot-backed names reset
    /// to nil; `buffer-undo-list` clears the undo state; all other
    /// names drop their entry from `local_var_alist` entirely.
    /// GNU doesn't have a true "void per-buffer binding" — removing
    /// the alist entry is the closest equivalent.
    pub fn set_buffer_local_void(&mut self, name: &str) {
        self.set_buffer_local_void_by_sym_id(intern(name));
    }

    pub fn set_buffer_local_void_by_sym_id(&mut self, sym_id: SymId) {
        if let Some(info) = lookup_buffer_slot_by_sym_id(sym_id) {
            self.slots[info.offset] = Value::NIL;
            return;
        }
        if sym_id == buffer_undo_list_sym() {
            self.undo_state.set_list(Value::NIL);
            self.undo_state.set_recorded_first_change(false);
            return;
        }
        remove_local_var_alist_entry(&mut self.local_var_alist, Value::from_sym_id(sym_id));
    }

    /// Drop a per-buffer binding. Returns the previous binding if
    /// one existed. Mirrors the non-special path of GNU
    /// `Fkill_local_variable` (`data.c:2314-2378`).
    pub fn kill_buffer_local(&mut self, name: &str) -> Option<RuntimeBindingValue> {
        self.kill_buffer_local_by_sym_id(intern(name))
    }

    pub fn kill_buffer_local_by_sym_id(&mut self, sym_id: SymId) -> Option<RuntimeBindingValue> {
        if sym_id == buffer_undo_list_sym() {
            return None;
        }
        let key = Value::from_sym_id(sym_id);
        let existing = find_local_var_alist_entry(self.local_var_alist, key)?;
        remove_local_var_alist_entry(&mut self.local_var_alist, key);
        Some(RuntimeBindingValue::Bound(existing))
    }

    pub fn kill_all_local_variables(
        &mut self,
        obarray: &mut crate::emacs_core::symbol::Obarray,
        kill_permanent: bool,
        buffer_defaults: &[crate::emacs_core::value::Value; BUFFER_SLOT_COUNT],
    ) {
        // Mirrors GNU `reset_buffer_local_variables'
        // (`buffer.c:1135-1234'). Three things happen:
        //
        //   1. Specific always-local slots get reset (major-mode,
        //      mode-name, invisibility-spec, the case tables, the
        //      keymap). GNU does these explicitly at the top of
        //      `reset_buffer_local_variables'. Neomacs encodes them
        //      via `BufferSlotInfo.reset_on_kill = true' for the
        //      slot-backed ones; the keymap is reset at the end.
        //
        //   2. Conditional slots are reset by clearing
        //      `local_flags[idx]', UNLESS `permanent_local' is set
        //      (matches GNU's `buffer_permanent_local_flags' table
        //      at `buffer.c:109,4751,4767'). Permanent conditional
        //      slots in upstream GNU are `truncate-lines' and
        //      `buffer-file-coding-system' -- both survive
        //      kill-all-local-variables.
        //
        //   3. The LOCALIZED `local_var_alist' is walked and
        //      non-`permanent-local' entries are spliced out
        //      (`buffer.c:1163-1228'). The `permanent-local-hook'
        //      partial-preserve filter runs in-place. See the
        //      walking loop below.
        //
        // Always-local slots that GNU does NOT explicitly reset
        // (`buffer-file-name', `default-directory', `mark-active',
        // `point-before-scroll', `buffer-display-count',
        // `buffer-display-time', `buffer-read-only', etc.) are
        // left untouched here. They have `reset_on_kill: false'.
        for info in BUFFER_SLOT_INFO {
            if info.local_flags_idx >= 0 {
                // Conditional slot. Skip if permanent (matches
                // GNU's `buffer_permanent_local_flags[idx] != 0'
                // gate at `buffer.c:1232'). The `kill_permanent'
                // flag overrides permanence -- it's used by
                // internal callers like `reset_buffer_local_variables(b, 1)'
                // for buffer creation/deletion. Ordinary
                // `kill-all-local-variables' calls pass
                // `kill_permanent = false' so permanent slots
                // survive.
                if info.permanent_local && !kill_permanent {
                    continue;
                }
                self.set_slot_local_flag(info.offset, false);
                // GNU `buffer.c:1242` — `set_per_buffer_value(b, offset,
                // per_buffer_default(offset))`. The reset target is the
                // CURRENT runtime buffer-defaults slot, NOT the
                // install-time `BufferSlotInfo::default` seed. The
                // distinction matters for any slot whose default got
                // updated by `setq-default` (e.g. bindings.el sets the
                // rich `mode-line-format` list — before this fix, the
                // reset here would clobber it back to the install-time
                // "%-" seed after any kill-all-local-variables call,
                // leaving the layout engine to render only the buffer
                // name).
                self.slots[info.offset] = buffer_defaults[info.offset];
            } else if info.reset_on_kill {
                // Always-local slot in GNU's explicit reset list
                // (major-mode, mode-name, invisibility-spec). These are
                // hardcoded resets in GNU (Qfundamental_mode, QSFundamental,
                // Qt) that don't participate in buffer-defaults, so the
                // install-time seed is the right value here.
                self.slots[info.offset] = info.default.to_value();
            }
        }

        // Phase 10E: walk `local_var_alist` and remove non-permanent
        // entries IN PLACE. Mirrors GNU `reset_buffer_local_variables`
        // at `buffer.c:1296-1335` which uses `Fdelq`-style splice.
        //
        // Permanent locals (`(get sym 'permanent-local)`) survive
        // unconditionally. Permanent-local-hook variables get their
        // hook list filtered to keep only the permanent entries.
        // The filter MUTATES the existing cell's cdr in place so
        // any BLV whose valcell points at the cell still observes
        // the filtered value without needing a re-swap.
        //
        // Removed entries trigger a BLV cache reset so the next
        // read for that LOCALIZED variable falls through to the
        // global default.
        {
            use crate::emacs_core::value::Value;
            let mut new_head = Value::NIL;
            let mut new_tail: Option<Value> = None;
            let mut alist = self.local_var_alist;
            while alist.is_cons() {
                let next_pair = alist.cons_cdr();
                let entry = alist.cons_car();
                if !entry.is_cons() {
                    alist = next_pair;
                    continue;
                }
                let sym_val = entry.cons_car();
                let Some(name) = sym_val.as_symbol_name() else {
                    alist = next_pair;
                    continue;
                };
                let mut keep = false;
                if kill_permanent {
                    // Drop all entries.
                } else {
                    let prop = obarray
                        .get_property(name, "permanent-local")
                        .filter(|v| !v.is_nil());
                    if let Some(prop) = prop {
                        if prop.is_symbol_named("permanent-local-hook") {
                            // Partial-preserve: filter the value and
                            // mutate the existing cell's cdr in place.
                            let value = entry.cons_cdr();
                            let preserved =
                                preserve_partial_permanent_local_hook_value(obarray, value);
                            entry.set_cdr(preserved);
                        }
                        keep = true;
                    }
                }
                if keep {
                    // Append this `alist` cons to the new chain.
                    if new_tail.is_none() {
                        new_head = alist;
                    } else {
                        new_tail.unwrap().set_cdr(alist);
                    }
                    new_tail = Some(alist);
                } else {
                    // Drop. Reset the BLV cache for this LOCALIZED
                    // variable so subsequent reads re-swap to the
                    // global default. Mirrors GNU's
                    // `swap_in_global_binding`.
                    let id = crate::emacs_core::intern::intern(name);
                    if let Some(blv) = obarray.blv_mut(id) {
                        blv.where_buf = Value::NIL;
                        blv.found = false;
                        blv.valcell = blv.defcell;
                    }
                }
                alist = next_pair;
            }
            // Terminate the new chain.
            if let Some(tail) = new_tail {
                tail.set_cdr(Value::NIL);
            }
            self.local_var_alist = new_head;
        }

        // GNU `reset_buffer_local_variables` also clears the
        // buffer's local keymap (`buffer.c:1337`).
        self.keymap = Value::NIL;
    }

    pub fn get_buffer_local(&self, name: &str) -> Option<Value> {
        self.get_buffer_local_by_sym_id(intern(name))
    }

    pub fn get_buffer_local_by_sym_id(&self, sym_id: SymId) -> Option<Value> {
        // Slot-backed names resolve to the live slot value, mirroring
        // GNU's `BVAR(buf, …)` accessor. Conditional slots only
        // report a per-buffer binding when the local-flags bit is
        // set; the caller falls through to the global default at a
        // higher layer that has access to `BufferManager::buffer_defaults`.
        if let Some(info) = lookup_buffer_slot_by_sym_id(sym_id) {
            if info.local_flags_idx >= 0 && !self.slot_local_flag(info.offset) {
                return None;
            }
            return Some(self.slots[info.offset]);
        }
        // `buffer-undo-list` reads through `SharedUndoState` so
        // indirect buffers see the root buffer's undo state.
        if sym_id == buffer_undo_list_sym() {
            return Some(self.get_undo_list());
        }
        // Everything else: walk `local_var_alist`. Mirrors GNU's
        // `assq_no_quit (var, BVAR (buf, local_var_alist))` at
        // `data.c:2409`. A `Qunbound` cdr is a "local but void"
        // marker — report it as absent for this read-style API,
        // since callers want a readable value. Use
        // `get_buffer_local_binding` when the Bound/Void/absent
        // distinction matters.
        find_local_var_alist_entry(self.local_var_alist, Value::from_sym_id(sym_id))
            .filter(|v| !v.is_unbound())
    }

    /// Walk this buffer's `local_var_alist` for an `(sym . val)`
    /// pair whose car matches `key`. Returns the cdr if found.
    /// Mirrors GNU's `assq_no_quit (variable, BVAR (buf, local_var_alist))`
    /// at `data.c:2409`.
    ///
    /// Used by Phase 10E callers that need to look up per-buffer
    /// values for LOCALIZED symbols without going through the
    /// obarray's BLV swap-in.
    pub fn find_in_local_var_alist(&self, key: Value) -> Option<Value> {
        let mut alist = self.local_var_alist;
        while alist.is_cons() {
            let entry = alist.cons_car();
            if entry.is_cons() && crate::emacs_core::value::eq_value(&entry.cons_car(), &key) {
                return Some(entry.cons_cdr());
            }
            alist = alist.cons_cdr();
        }
        None
    }

    pub fn get_buffer_local_binding(&self, name: &str) -> Option<RuntimeBindingValue> {
        self.get_buffer_local_binding_by_sym_id(intern(name))
    }

    pub fn get_buffer_local_binding_by_sym_id(&self, sym_id: SymId) -> Option<RuntimeBindingValue> {
        // BUFFER_OBJFWD slots are always live and bypass any
        // "present/absent" short-circuit. They never go void in
        // GNU — a nil slot still resolves as Bound(nil).
        // Conditional slots (`local_flags_idx >= 0`) only report a
        // per-buffer binding when the local-flag bit is set;
        // otherwise the caller falls through to the global default.
        if let Some(info) = lookup_buffer_slot_by_sym_id(sym_id) {
            if info.local_flags_idx >= 0 && !self.slot_local_flag(info.offset) {
                return None;
            }
            return Some(RuntimeBindingValue::Bound(self.slots[info.offset]));
        }
        if sym_id == buffer_undo_list_sym() {
            return Some(RuntimeBindingValue::Bound(self.get_undo_list()));
        }
        // An UNBOUND cdr in the alist marks a void per-buffer
        // binding — the variable IS local (Some) but has no
        // value (Void). Mirrors GNU's `(var . Qunbound)` alist
        // entries created by `Fmake_local_variable` on a void
        // symbol at `data.c:2285-2289`.
        find_local_var_alist_entry(self.local_var_alist, Value::from_sym_id(sym_id)).map(|v| {
            if v.is_unbound() {
                RuntimeBindingValue::Void
            } else {
                RuntimeBindingValue::Bound(v)
            }
        })
    }

    pub fn has_buffer_local(&self, name: &str) -> bool {
        self.has_buffer_local_by_sym_id(intern(name))
    }

    pub fn has_buffer_local_by_sym_id(&self, sym_id: SymId) -> bool {
        // BUFFER_OBJFWD-style names are conceptually always
        // per-buffer (mirrors GNU's `local-variable-p` returning t
        // for DEFVAR_PER_BUFFER variables regardless of whether the
        // user explicitly called `make-local-variable`).
        // Conditional slots only count as local when the per-buffer
        // flag bit is set — mirrors GNU `local-variable-p`
        // dispatching through `PER_BUFFER_VALUE_P` at
        // `data.c:2347-2380`.
        if let Some(info) = lookup_buffer_slot_by_sym_id(sym_id) {
            if info.local_flags_idx >= 0 {
                return self.slot_local_flag(info.offset);
            }
            return true;
        }
        // `buffer-undo-list` is always present (its SharedUndoState
        // is unconditionally allocated; there's no "unset" state).
        if sym_id == buffer_undo_list_sym() {
            return true;
        }
        find_local_var_alist_entry(self.local_var_alist, Value::from_sym_id(sym_id)).is_some()
    }

    pub fn local_map(&self) -> Value {
        self.keymap
    }

    pub fn set_local_map(&mut self, keymap: Value) {
        self.keymap = keymap;
    }

    /// Mirror of GNU `buffer_local_value` at `buffer.c:1359-1413`.
    ///
    /// For `SYMBOL_FORWARDED` BUFFER_OBJFWD vars (our `BufferSlotInfo`
    /// slot-backed names), GNU unconditionally reads
    /// `per_buffer_value(buf, offset)` at `buffer.c:1405` — it does NOT
    /// check `PER_BUFFER_VALUE_P`. The flag only distinguishes "this
    /// buffer has a local override" from "this buffer uses the
    /// runtime default"; the slot itself is always populated.
    ///
    /// `get_buffer_local_binding` returns `None` when the flag is
    /// clear (it's the lower-level "is there a local override?"
    /// primitive), which is correct for callers like `local-variable-p`.
    /// But `buffer_local_value` should NOT return `None` in that
    /// case — it should return the slot value. Before this fix, the
    /// layout engine's mode-line read was getting `None` for every
    /// conditional slot in its "virgin" state, falling back to the
    /// obarray value cell (which for forwarded vars is `nil`), and
    /// therefore asking `format-mode-line` to render `nil`, which
    /// produced an empty mode-line containing only the buffer name.
    pub fn buffer_local_value(&self, name: &str) -> Option<Value> {
        if let Some(info) = lookup_buffer_slot(name) {
            return Some(self.slots[info.offset]);
        }
        if name == "buffer-undo-list" {
            return Some(self.get_undo_list());
        }
        match self.get_buffer_local_binding(name) {
            Some(RuntimeBindingValue::Bound(value)) => Some(value),
            Some(RuntimeBindingValue::Void) | None => None,
        }
    }

    pub fn ordered_buffer_local_bindings(&self) -> Vec<(SymId, RuntimeBindingValue)> {
        // Returns entries in REVERSED GNU order so the caller can
        // `.rev()' to get GNU's prepend-based final order.
        //
        // GNU `Fbuffer_local_variables' (`buffer.c:1471-1502'):
        //
        //   1. `buffer_lisp_local_variables(buf, 0)' walks
        //      `local_var_alist' forward, prepending each entry.
        //      Result: alist entries in REVERSE iteration order.
        //
        //   2. `FOR_EACH_PER_BUFFER_OBJECT_AT (offset)' walks slot
        //      offsets forward, prepending each applicable slot
        //      entry. Result so far: slot entries (reversed) at the
        //      FRONT of the alist entries.
        //
        //   3. Finally prepends the special `undo_list' slot via
        //      `buffer_local_variables_1(buf, ..., Qbuffer_undo_list)'.
        //
        // Final GNU order:
        //
        //     [undo_list,
        //      slot_N_rev, slot_N-1_rev, ..., slot_0_rev,
        //      alist_N_rev, alist_N-1_rev, ..., alist_0_rev]
        //
        // This function returns the REVERSE of that:
        //
        //     [alist_0, alist_1, ..., alist_N,
        //      slot_0, slot_1, ..., slot_N,
        //      undo_list]
        //
        // so `.rev()' in `builtin_buffer_local_variables' yields
        // GNU's exact order. The bare-symbol-vs-cons mapping for
        // `Qunbound' values happens at the caller's `.map()' step.
        //
        // Slot filter mirrors GNU's `buffer_local_variables_1':
        // emit when `local_flags_idx == -1' (always-local) OR
        // `PER_BUFFER_VALUE_P (buf, idx)' (the local-flag bit is
        // set). Internal-only slots (`install_as_forwarder: false')
        // are omitted because GNU skips slots with no Lisp variable
        // name (syntax_table_ etc.).
        let mut out: Vec<(SymId, RuntimeBindingValue)> = Vec::new();

        // Step 1: alist entries, walked forward, used UNREVERSED so
        // that `.rev()' in the caller flips them to match GNU's
        // `buffer_lisp_local_variables' prepend-based reversal.
        let mut cursor = self.local_var_alist;
        while cursor.is_cons() {
            let entry = cursor.cons_car();
            cursor = cursor.cons_cdr();
            if !entry.is_cons() {
                continue;
            }
            if let Some(sym_id) = entry.cons_car().as_symbol_id() {
                let cdr = entry.cons_cdr();
                let binding = if cdr.is_unbound() {
                    RuntimeBindingValue::Void
                } else {
                    RuntimeBindingValue::Bound(cdr)
                };
                out.push((sym_id, binding));
            }
        }

        // Step 2: BUFFER_OBJFWD slots in declaration order. Same
        // forward iteration; the `.rev()' in the caller flips them
        // to match GNU's prepend reversal.
        for info in BUFFER_SLOT_INFO {
            if !info.install_as_forwarder {
                continue;
            }
            // GNU's filter: emit only when always-local
            // (local_flags_idx == -1) or the per-buffer flag bit is
            // set. Always-local slots in GNU correspond to neomacs
            // slots with `local_flags_idx < 0'.
            if info.local_flags_idx >= 0 && !self.slot_local_flag(info.offset) {
                continue;
            }
            out.push((
                intern(info.name),
                RuntimeBindingValue::Bound(self.slots[info.offset]),
            ));
        }

        // Step 3: `buffer-undo-list' last in this Vec so `.rev()'
        // puts it FIRST in the final list, matching GNU's special
        // tail-prepend at `buffer.c:1496-1499'.
        out.push((
            buffer_undo_list_sym(),
            RuntimeBindingValue::Bound(self.get_undo_list()),
        ));

        out
    }

    pub fn ordered_buffer_local_names(&self) -> Vec<SymId> {
        self.ordered_buffer_local_bindings()
            .into_iter()
            .map(|(sym_id, _)| sym_id)
            .collect()
    }

    /// Walk the buffer's `local_var_alist` yielding a mutable
    /// reference into each entry's value via the cons cell's cdr
    /// field. Used by the GC-root visitor pipeline to avoid
    /// traversing the alist twice.
    pub fn bound_buffer_local_values_mut(&mut self) -> impl Iterator<Item = &mut Value> {
        // In the new design, local_var_alist IS the source of truth
        // for non-slot per-buffer bindings. Its cons cells are
        // already GC-traced via `Buffer::trace_roots` walking
        // `self.local_var_alist`, so nothing else needs a mutable
        // visitor. Keep the method signature for API compatibility
        // but return an empty iterator — the roots are reached
        // through the alist walk in `trace_roots`.
        std::iter::empty()
    }
}

impl Buffer {
    pub fn buffer_local_bound_p(&self, name: &str) -> bool {
        matches!(
            self.get_buffer_local_binding(name),
            Some(RuntimeBindingValue::Bound(_))
        )
    }

    pub fn buffer_local_void_p(&self, name: &str) -> bool {
        matches!(
            self.get_buffer_local_binding(name),
            Some(RuntimeBindingValue::Void)
        )
    }
}

// ---------------------------------------------------------------------------
// BufferManager
// ---------------------------------------------------------------------------

/// Owns every live buffer, tracks the current buffer, and hands out ids.
#[derive(Clone)]
pub struct BufferManager {
    buffers: HashMap<BufferId, Buffer>,
    current: Option<BufferId>,
    next_id: u64,
    next_marker_id: u64,
    labeled_restrictions: HashMap<BufferId, Vec<LabeledRestriction>>,
    dead_buffer_last_names: HashMap<BufferId, Value>,
    /// Global default values for `BUFFER_OBJFWD` slots. Mirrors GNU's
    /// `buffer_defaults` (`buffer.c:84-90`), which is itself a
    /// sentinel `struct buffer` whose fields hold the global default
    /// for every per-buffer variable. Reads of a conditional slot
    /// (`local_flags_idx >= 0`) fall through here when the per-buffer
    /// `Buffer::local_flags` bit is clear; `setq-default` writes
    /// here directly. Phase 10D wires this in.
    pub buffer_defaults: [crate::emacs_core::value::Value; BUFFER_SLOT_COUNT],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UndoExecutionResult {
    pub had_any_records: bool,
    pub had_boundary: bool,
    pub applied_any: bool,
    pub skipped_apply: bool,
}

impl BufferManager {
    /// Create a new `BufferManager` pre-populated with a `*scratch*` buffer.
    pub fn new() -> Self {
        // Phase 10D: seed `buffer_defaults` from `BUFFER_SLOT_INFO`,
        // mirroring GNU `init_buffer_once` which materializes
        // `buffer_defaults` from the per-slot `default` literals
        // (`buffer.c:4828-4889`). Slots that are not in
        // BUFFER_SLOT_INFO start as `Value::NIL`.
        let mut buffer_defaults = [crate::emacs_core::value::Value::NIL; BUFFER_SLOT_COUNT];
        for info in BUFFER_SLOT_INFO {
            buffer_defaults[info.offset] = info.default.to_value();
        }
        let mut mgr = Self {
            buffers: HashMap::new(),
            current: None,
            next_id: 1,
            next_marker_id: 1,
            labeled_restrictions: HashMap::new(),
            dead_buffer_last_names: HashMap::new(),
            buffer_defaults,
        };
        let scratch = mgr.create_buffer("*scratch*");
        mgr.current = Some(scratch);
        mgr
    }

    /// Allocate a new buffer with the given name and return its id.
    pub fn create_buffer(&mut self, name: &str) -> BufferId {
        self.create_buffer_with_hook_inhibition(name, false)
    }

    /// Allocate a new buffer with the given name and hook-inhibition state.
    pub fn create_buffer_with_hook_inhibition(
        &mut self,
        name: &str,
        inhibit_buffer_hooks: bool,
    ) -> BufferId {
        let id = BufferId(self.next_id);
        self.next_id += 1;
        let mut buf = Buffer::new(id, Value::string(name));
        // Phase 10D: seed every conditional slot from
        // `BufferManager::buffer_defaults` so a buffer created
        // *after* a `setq-default`/`set-default` observes the live
        // global default rather than the static `BufferSlotInfo`
        // seed. Always-local slots (-1) keep their per-buffer
        // initial value (the static seed already populated them).
        for info in BUFFER_SLOT_INFO {
            if info.local_flags_idx >= 0 {
                buf.slots[info.offset] = self.buffer_defaults[info.offset];
            }
        }
        buf.inhibit_buffer_hooks = inhibit_buffer_hooks;
        if let Some(default_directory) = self
            .current
            .and_then(|current| self.buffers.get(&current))
            .and_then(|current| current.buffer_local_value("default-directory"))
        {
            buf.set_buffer_local("default-directory", default_directory);
        }
        // GNU buffer.c:667 — buffers whose names start with a space have
        // undo recording disabled by default.
        if name.starts_with(' ') {
            buf.set_buffer_local("buffer-undo-list", crate::emacs_core::value::Value::T);
        }
        self.buffers.insert(id, buf);
        id
    }

    /// Allocate a new indirect buffer that shares its root base buffer's text.
    ///
    /// This mirrors GNU Emacs's `make-indirect-buffer` C boundary:
    /// indirect buffers share the root base buffer's text object, and double
    /// indirection is flattened so every indirect points at the same root.
    pub fn create_indirect_buffer(
        &mut self,
        base_id: BufferId,
        name: &str,
        clone: bool,
    ) -> Option<BufferId> {
        self.create_indirect_buffer_with_hook_inhibition(base_id, name, clone, false)
    }

    pub fn create_indirect_buffer_with_hook_inhibition(
        &mut self,
        base_id: BufferId,
        name: &str,
        clone: bool,
        inhibit_buffer_hooks: bool,
    ) -> Option<BufferId> {
        if name.is_empty() || self.find_buffer_by_name(name).is_some() {
            return None;
        }

        let root_id = self.shared_text_root_id(base_id)?;
        let root = self.buffers.get(&root_id)?.clone();
        let shared_text = self.buffers.get(&root_id)?.text.shared_clone();

        let id = BufferId(self.next_id);
        self.next_id += 1;

        let mut indirect = if clone {
            let mut cloned = root.clone();
            cloned.id = id;
            cloned.set_name_value(Value::string(name));
            cloned
        } else {
            let mut fresh = Buffer::new(id, Value::string(name));
            if let Some(default_directory) = self
                .current
                .and_then(|current| self.buffers.get(&current))
                .and_then(|current| current.buffer_local_value("default-directory"))
            {
                fresh.set_buffer_local("default-directory", default_directory);
            }
            fresh
        };

        indirect.base_buffer = Some(root_id);
        indirect.inhibit_buffer_hooks = inhibit_buffer_hooks;
        indirect.text = shared_text;
        indirect.undo_state = root.undo_state.clone();
        indirect.narrow_to_byte_region(root.begv_byte, root.zv_byte);
        indirect.goto_byte(root.pt_byte);
        indirect.set_multibyte_value(root.get_multibyte());
        indirect.autosave_modified_tick = root.autosave_modified_tick;
        indirect.slots[BUFFER_SLOT_FILE_NAME] = Value::NIL;
        if !clone {
            indirect.overlays = OverlayList::new();
            indirect.mark = None;
            indirect.mark_byte = None;
        }

        self.buffers.insert(id, indirect);
        let _ = self.ensure_buffer_state_markers(root_id);
        let _ = self.ensure_buffer_state_markers(id);
        Some(id)
    }

    /// Immutable access to a buffer by id.
    pub fn get(&self, id: BufferId) -> Option<&Buffer> {
        self.buffers.get(&id)
    }

    /// Mutable access to a buffer by id.
    pub fn get_mut(&mut self, id: BufferId) -> Option<&mut Buffer> {
        self.buffers.get_mut(&id)
    }

    /// Immutable access to the current buffer.
    pub fn current_buffer(&self) -> Option<&Buffer> {
        self.current.and_then(|id| self.buffers.get(&id))
    }

    /// Mutable access to the current buffer.
    pub fn current_buffer_mut(&mut self) -> Option<&mut Buffer> {
        self.current.and_then(|id| self.buffers.get_mut(&id))
    }

    /// Return the current buffer id.
    pub fn current_buffer_id(&self) -> Option<BufferId> {
        self.current
    }

    pub fn buffer_hooks_inhibited(&self, id: BufferId) -> bool {
        self.buffers
            .get(&id)
            .is_some_and(|buffer| buffer.inhibit_buffer_hooks)
    }

    fn buffer_has_state_markers(&self, id: BufferId) -> bool {
        self.buffers
            .get(&id)
            .and_then(|buffer| buffer.state_markers)
            .is_some()
    }

    fn ensure_buffer_state_markers(&mut self, buffer_id: BufferId) -> Option<()> {
        if self.buffer_has_state_markers(buffer_id) {
            return Some(());
        }
        let (pt, begv, zv) = {
            let buffer = self.buffers.get(&buffer_id)?;
            (buffer.pt_byte, buffer.begv_byte, buffer.zv_byte)
        };
        let (pt_marker, pt_marker_ptr) = self.create_marker(buffer_id, pt, InsertionType::Before);
        let (begv_marker, begv_marker_ptr) =
            self.create_marker(buffer_id, begv, InsertionType::Before);
        let (zv_marker, zv_marker_ptr) = self.create_marker(buffer_id, zv, InsertionType::After);
        self.buffers.get_mut(&buffer_id)?.state_markers = Some(BufferStateMarkers {
            pt_marker,
            begv_marker,
            zv_marker,
            pt_marker_ptr,
            begv_marker_ptr,
            zv_marker_ptr,
        });
        Some(())
    }

    fn record_buffer_state_markers(&mut self, buffer_id: BufferId) -> Option<()> {
        let markers = self.buffers.get(&buffer_id)?.state_markers?;
        let (pt, begv, zv) = {
            let buffer = self.buffers.get(&buffer_id)?;
            (buffer.pt_byte, buffer.begv_byte, buffer.zv_byte)
        };
        // State markers live on this buffer's chain already; unlink before
        // re-registering so chain_splice_at_head's precondition holds.
        if let Some(buf) = self.buffers.get(&buffer_id) {
            buf.text.chain_unlink(markers.pt_marker_ptr);
            buf.text.chain_unlink(markers.begv_marker_ptr);
            buf.text.chain_unlink(markers.zv_marker_ptr);
        }
        self.register_marker_id(
            markers.pt_marker_ptr,
            buffer_id,
            markers.pt_marker,
            pt,
            InsertionType::Before,
        )?;
        self.register_marker_id(
            markers.begv_marker_ptr,
            buffer_id,
            markers.begv_marker,
            begv,
            InsertionType::Before,
        )?;
        self.register_marker_id(
            markers.zv_marker_ptr,
            buffer_id,
            markers.zv_marker,
            zv,
            InsertionType::After,
        )?;
        Some(())
    }

    fn fetch_buffer_state_markers(&mut self, buffer_id: BufferId) -> Option<()> {
        let markers = self.buffers.get(&buffer_id)?.state_markers?;
        let pt = self.marker_position(buffer_id, markers.pt_marker)?;
        let pt_char = self.marker_char_position(buffer_id, markers.pt_marker)?;
        let begv = self.marker_position(buffer_id, markers.begv_marker)?;
        let begv_char = self.marker_char_position(buffer_id, markers.begv_marker)?;
        let zv = self.marker_position(buffer_id, markers.zv_marker)?;
        let zv_char = self.marker_char_position(buffer_id, markers.zv_marker)?;
        let buffer = self.buffers.get_mut(&buffer_id)?;
        buffer.pt = pt_char;
        buffer.pt_byte = pt;
        buffer.begv = begv_char;
        buffer.begv_byte = begv;
        buffer.zv = zv_char;
        buffer.zv_byte = zv;
        Some(())
    }

    /// Switch the current buffer and run buffer-manager-owned transition work.
    ///
    /// This is the closest NeoVM equivalent of GNU Emacs's
    /// `set_buffer_internal_1/2` boundary inside the buffer subsystem.
    pub fn switch_current(&mut self, id: BufferId) -> bool {
        if !self.buffers.contains_key(&id) {
            return false;
        }
        if self.current == Some(id) {
            return true;
        }

        let old_id = self.current;
        self.current = Some(id);

        if let Some(old_id) = old_id {
            let _ = self.record_buffer_state_markers(old_id);
        }
        let _ = self.fetch_buffer_state_markers(id);
        true
    }

    /// Backwards-compatible alias while call sites migrate to `switch_current`.
    pub fn set_current(&mut self, id: BufferId) {
        let _ = self.switch_current(id);
    }

    /// Find a buffer by name, returning its id if it exists.
    pub fn find_buffer_by_name(&self, name: &str) -> Option<BufferId> {
        self.buffers
            .values()
            .find(|b| b.has_name(name))
            .map(|b| b.id)
    }

    /// Find a killed buffer by its last known name.
    pub fn find_dead_buffer_by_name(&self, name: &str) -> Option<BufferId> {
        self.dead_buffer_last_names
            .iter()
            .find_map(|(id, last_name)| {
                (last_name.as_runtime_string_owned().as_deref() == Some(name)).then_some(*id)
            })
    }

    /// Remove a buffer.  Returns `true` if the buffer existed.
    ///
    /// If the killed buffer was current, `current` is set to `None`.
    pub fn kill_buffer(&mut self, id: BufferId) -> bool {
        self.kill_buffer_collect(id).is_some()
    }

    pub fn kill_buffer_collect(&mut self, id: BufferId) -> Option<Vec<BufferId>> {
        let killed_ids = self.collect_killed_buffer_ids(id)?;
        let killed_set: HashSet<BufferId> = killed_ids.iter().copied().collect();
        let kill_root = self.buffers.get(&id)?.base_buffer.is_none();

        for killed_id in &killed_ids {
            self.replace_labeled_restrictions(*killed_id, None);
        }

        with_tagged_heap(|heap| heap.clear_markers_for_buffers(&killed_set));
        if kill_root {
            self.buffers.get(&id)?.text.clear_markers();
        } else {
            self.buffers
                .get(&id)?
                .text
                .remove_markers_for_buffers(&killed_set);
        }

        for killed_id in &killed_ids {
            let buf = self.buffers.remove(killed_id)?;
            self.dead_buffer_last_names
                .insert(*killed_id, buf.name_value());
        }

        if self
            .current
            .is_some_and(|current| killed_set.contains(&current))
        {
            self.current = None;
        }

        Some(killed_ids)
    }

    /// Return the last known name for a dead buffer id, if available.
    pub fn dead_buffer_last_name_value(&self, id: BufferId) -> Option<Value> {
        self.dead_buffer_last_names.get(&id).copied()
    }

    pub fn dead_buffer_last_name_owned(&self, id: BufferId) -> Option<String> {
        self.dead_buffer_last_name_value(id)
            .and_then(Value::as_runtime_string_owned)
    }

    /// List all live buffer ids in stable creation order.
    pub fn buffer_list(&self) -> Vec<BufferId> {
        let mut ids: Vec<BufferId> = self.buffers.keys().copied().collect();
        ids.sort_by_key(|id| id.0);
        ids
    }

    fn shared_text_root_id(&self, id: BufferId) -> Option<BufferId> {
        let buf = self.buffers.get(&id)?;
        Some(buf.base_buffer.unwrap_or(buf.id))
    }

    pub(crate) fn collect_killed_buffer_ids(&self, id: BufferId) -> Option<Vec<BufferId>> {
        let buf = self.buffers.get(&id)?;
        let mut killed_ids = vec![id];
        if buf.base_buffer.is_none() {
            let mut indirects = self
                .buffers
                .values()
                .filter_map(|buffer| (buffer.base_buffer == Some(id)).then_some(buffer.id))
                .collect::<Vec<_>>();
            indirects.sort_by_key(|buffer_id| buffer_id.0);
            killed_ids.extend(indirects);
        }
        Some(killed_ids)
    }

    fn full_buffer_bounds(&self, id: BufferId) -> Option<(usize, usize)> {
        let buf = self.buffers.get(&id)?;
        Some((0, buf.total_bytes()))
    }

    fn labeled_restriction_at(&self, id: BufferId, outermost: bool) -> Option<&LabeledRestriction> {
        let restrictions = self.labeled_restrictions.get(&id)?;
        if outermost {
            restrictions.first()
        } else {
            restrictions.last()
        }
    }

    fn labeled_restriction_bounds(&self, id: BufferId, outermost: bool) -> Option<(usize, usize)> {
        let restriction = self.labeled_restriction_at(id, outermost)?;
        let beg = self.marker_position(id, restriction.beg_marker)?;
        let end = self.marker_position(id, restriction.end_marker)?;
        Some((beg, end))
    }

    pub fn current_labeled_restriction_bounds(&self, id: BufferId) -> Option<(usize, usize)> {
        self.labeled_restriction_bounds(id, false)
    }

    pub fn current_labeled_restriction_char_bounds(&self, id: BufferId) -> Option<(usize, usize)> {
        let restriction = self.labeled_restriction_at(id, false)?;
        let beg = self.marker_char_position(id, restriction.beg_marker)?;
        let end = self.marker_char_position(id, restriction.end_marker)?;
        Some((beg, end))
    }

    pub fn current_labeled_restriction_matches_label(&self, id: BufferId, label: &Value) -> bool {
        let Some(restriction) = self.labeled_restriction_at(id, false) else {
            return false;
        };
        match restriction.label {
            LabeledRestrictionLabel::User(current) => {
                crate::emacs_core::value::eq_value(&current, label)
            }
            LabeledRestrictionLabel::Outermost => false,
        }
    }

    fn clone_marker_in_buffer(&mut self, buffer_id: BufferId, marker_id: u64) -> Option<u64> {
        let (pos, insertion_type) = {
            let buf = self.buffers.get(&buffer_id)?;
            // T7: read byte_pos / insertion_type directly from the chain
            // node instead of the deleted Vec<MarkerEntry>.
            let (bytepos, _charpos, ins_type) = buf.text.marker_chain_lookup(marker_id)?;
            (bytepos, ins_type)
        };
        let (marker_id, _marker_ptr) = self.create_marker(buffer_id, pos, insertion_type);
        Some(marker_id)
    }

    fn clone_labeled_restrictions(
        &mut self,
        buffer_id: BufferId,
    ) -> Option<Option<Vec<LabeledRestriction>>> {
        let restrictions = self.labeled_restrictions.get(&buffer_id)?.clone();
        let mut cloned = Vec::with_capacity(restrictions.len());
        for restriction in restrictions {
            let beg_marker = self.clone_marker_in_buffer(buffer_id, restriction.beg_marker)?;
            let end_marker = self.clone_marker_in_buffer(buffer_id, restriction.end_marker)?;
            cloned.push(LabeledRestriction {
                label: restriction.label,
                beg_marker,
                end_marker,
            });
        }
        Some(Some(cloned))
    }

    fn replace_labeled_restrictions(
        &mut self,
        buffer_id: BufferId,
        restrictions: Option<Vec<LabeledRestriction>>,
    ) {
        let mut live_marker_ids = std::collections::HashSet::new();
        if let Some(ref restrictions) = restrictions {
            for restriction in restrictions {
                live_marker_ids.insert(restriction.beg_marker);
                live_marker_ids.insert(restriction.end_marker);
            }
        }

        if let Some(old) = self.labeled_restrictions.remove(&buffer_id) {
            for restriction in old {
                if !live_marker_ids.contains(&restriction.beg_marker) {
                    self.remove_marker(restriction.beg_marker);
                }
                if !live_marker_ids.contains(&restriction.end_marker) {
                    self.remove_marker(restriction.end_marker);
                }
            }
        }

        if self.buffers.contains_key(&buffer_id) {
            if let Some(restrictions) = restrictions.filter(|restrictions| !restrictions.is_empty())
            {
                self.labeled_restrictions.insert(buffer_id, restrictions);
            }
        }
    }

    pub fn clear_buffer_labeled_restrictions(&mut self, buffer_id: BufferId) -> Option<()> {
        self.buffers.get(&buffer_id)?;
        self.replace_labeled_restrictions(buffer_id, None);
        Some(())
    }

    fn push_labeled_restriction_for_current_bounds(
        &mut self,
        buffer_id: BufferId,
        label: LabeledRestrictionLabel,
    ) -> Option<()> {
        let (begv, zv) = {
            let buf = self.buffers.get(&buffer_id)?;
            (buf.begv_byte, buf.zv_byte)
        };
        let (beg_marker, _) = self.create_marker(buffer_id, begv, InsertionType::Before);
        let (end_marker, _) = self.create_marker(buffer_id, zv, InsertionType::After);
        self.labeled_restrictions
            .entry(buffer_id)
            .or_default()
            .push(LabeledRestriction {
                label,
                beg_marker,
                end_marker,
            });
        Some(())
    }

    fn pop_labeled_restriction(&mut self, buffer_id: BufferId) -> Option<LabeledRestriction> {
        let restrictions = self.labeled_restrictions.get_mut(&buffer_id)?;
        let restriction = restrictions.pop()?;
        let remove_entry = restrictions.is_empty();
        if remove_entry {
            self.labeled_restrictions.remove(&buffer_id);
        }
        self.remove_marker(restriction.beg_marker);
        self.remove_marker(restriction.end_marker);
        Some(restriction)
    }

    fn widen_buffer_fully(&mut self, id: BufferId) -> Option<()> {
        let (begv, zv) = self.full_buffer_bounds(id)?;
        self.restore_buffer_restriction(id, begv, zv)
    }

    fn buffers_sharing_root_ids(&self, root_id: BufferId) -> Vec<BufferId> {
        self.buffers
            .values()
            .filter_map(|buf| (buf.base_buffer.unwrap_or(buf.id) == root_id).then_some(buf.id))
            .collect()
    }

    pub(crate) fn shared_text_buffer_ids(&self, root_id: BufferId) -> Vec<BufferId> {
        self.buffers_sharing_root_ids(root_id)
    }

    pub(crate) fn modified_state_root_id(&self, id: BufferId) -> Option<BufferId> {
        self.shared_text_root_id(id)
    }

    pub fn goto_buffer_byte(&mut self, id: BufferId, pos: usize) -> Option<usize> {
        {
            let buf = self.buffers.get_mut(&id)?;
            buf.goto_byte(pos);
        }
        let point = self.buffers.get(&id)?.point_byte();
        let _ = self.record_buffer_state_markers(id);
        Some(point)
    }

    pub fn delete_all_buffer_overlays(&mut self, id: BufferId) -> Option<()> {
        let buf = self.buffers.get_mut(&id)?;
        let ids = buf
            .overlays
            .overlays_in(buf.point_min_byte(), buf.point_max_byte());
        for ov_id in ids {
            buf.overlays.delete_overlay(ov_id);
        }
        Some(())
    }

    pub fn delete_buffer_overlay(&mut self, id: BufferId, overlay_id: Value) -> Option<()> {
        self.buffers
            .get_mut(&id)?
            .overlays
            .delete_overlay(overlay_id);
        Some(())
    }

    pub fn put_buffer_overlay_property(
        &mut self,
        id: BufferId,
        overlay_id: Value,
        name: Value,
        value: Value,
    ) -> Option<()> {
        self.buffers
            .get_mut(&id)?
            .overlays
            .overlay_put(overlay_id, name, value)
            .ok()?;
        Some(())
    }

    pub fn narrow_buffer_to_region(
        &mut self,
        id: BufferId,
        start: usize,
        end: usize,
    ) -> Option<()> {
        self.buffers.get_mut(&id)?.narrow_to_byte_region(start, end);
        let _ = self.record_buffer_state_markers(id);
        Some(())
    }

    pub fn widen_buffer(&mut self, id: BufferId) -> Option<()> {
        self.buffers.get(&id)?;
        let Some(restriction) = self.labeled_restriction_at(id, false).copied() else {
            return self.widen_buffer_fully(id);
        };
        let Some((begv, zv)) = self.labeled_restriction_bounds(id, false) else {
            self.replace_labeled_restrictions(id, None);
            return self.widen_buffer_fully(id);
        };
        self.restore_buffer_restriction(id, begv, zv)?;
        if matches!(restriction.label, LabeledRestrictionLabel::Outermost) {
            let _ = self.pop_labeled_restriction(id);
        }
        Some(())
    }

    pub fn replace_buffer_contents(&mut self, id: BufferId, text: &str) -> Option<()> {
        let len = self.buffers.get(&id)?.total_bytes();
        if len > 0 {
            self.delete_buffer_region(id, 0, len)?;
        }
        {
            let buf = self.buffers.get_mut(&id)?;
            buf.widen();
            buf.goto_byte(0);
        }
        if !text.is_empty() {
            self.insert_into_buffer(id, text)?;
            self.goto_buffer_byte(id, 0)?;
        }
        Some(())
    }

    pub fn replace_buffer_contents_lisp_string(
        &mut self,
        id: BufferId,
        text: &crate::heap_types::LispString,
    ) -> Option<()> {
        debug_assert_eq!(
            self.buffers.get(&id)?.get_multibyte(),
            text.is_multibyte(),
            "replace_buffer_contents_lisp_string expects text already converted to target buffer representation",
        );
        let len = self.buffers.get(&id)?.total_bytes();
        if len > 0 {
            self.delete_buffer_region(id, 0, len)?;
        }
        {
            let buf = self.buffers.get_mut(&id)?;
            buf.widen();
            buf.goto_byte(0);
        }
        if !text.is_empty() {
            self.insert_lisp_string_into_buffer(id, text)?;
            self.goto_buffer_byte(id, 0)?;
        }
        Some(())
    }

    pub fn clear_buffer_local_properties(
        &mut self,
        id: BufferId,
        obarray: &mut crate::emacs_core::symbol::Obarray,
        kill_permanent: bool,
    ) -> Option<()> {
        // Snapshot the runtime buffer_defaults before we take a
        // mutable borrow of the individual buffer. Mirrors GNU's
        // `reset_buffer_local_variables` at `buffer.c:1242`, which
        // reads `per_buffer_default(offset)` (the runtime default)
        // rather than the C-level static initializer.
        let defaults_snapshot = self.buffer_defaults;
        let buf = self.buffers.get_mut(&id)?;
        buf.kill_all_local_variables(obarray, kill_permanent, &defaults_snapshot);
        Some(())
    }

    pub fn put_buffer_text_property(
        &mut self,
        id: BufferId,
        start: usize,
        end: usize,
        name: Value,
        value: Value,
    ) -> Option<bool> {
        let buf = self.buffers.get_mut(&id)?;
        // Record old value for undo before changing.
        if !buf.undo_state.in_progress() && !undo::undo_list_is_disabled(&buf.get_undo_list()) {
            let old_val = buf
                .text
                .text_props_get_property(start, name)
                .unwrap_or(Value::NIL);
            let mut ul = buf.get_undo_list();
            undo::undo_list_record_property_change(&mut ul, name, old_val, start, end);
            buf.set_undo_list(ul);
        }
        Some(buf.text.text_props_put_property(start, end, name, value))
    }

    pub fn append_buffer_text_properties(
        &mut self,
        id: BufferId,
        table: &TextPropertyTable,
        byte_offset: usize,
    ) -> Option<()> {
        self.buffers
            .get_mut(&id)?
            .text
            .text_props_append_shifted(table, byte_offset);
        Some(())
    }

    pub fn remove_buffer_text_property(
        &mut self,
        id: BufferId,
        start: usize,
        end: usize,
        name: Value,
    ) -> Option<bool> {
        let buf = self.buffers.get_mut(&id)?;
        // Record old value for undo before removing.
        if !buf.undo_state.in_progress() && !undo::undo_list_is_disabled(&buf.get_undo_list()) {
            let old_val = buf
                .text
                .text_props_get_property(start, name)
                .unwrap_or(Value::NIL);
            // Only record if property actually exists.
            if !old_val.is_nil() {
                let mut ul = buf.get_undo_list();
                undo::undo_list_record_property_change(&mut ul, name, old_val, start, end);
                buf.set_undo_list(ul);
            }
        }
        Some(buf.text.text_props_remove_property(start, end, name))
    }

    pub fn clear_buffer_text_properties(
        &mut self,
        id: BufferId,
        start: usize,
        end: usize,
    ) -> Option<()> {
        self.buffers
            .get_mut(&id)?
            .text
            .text_props_remove_all(start, end);
        Some(())
    }

    pub fn set_buffer_multibyte_flag(&mut self, id: BufferId, flag: bool) -> Option<()> {
        let buf = self.buffers.get_mut(&id)?;
        buf.set_multibyte_value(flag);
        buf.set_buffer_local(
            "enable-multibyte-characters",
            if flag {
                crate::emacs_core::value::Value::T
            } else {
                crate::emacs_core::value::Value::NIL
            },
        );
        Some(())
    }

    pub fn set_buffer_modified_flag(&mut self, id: BufferId, flag: bool) -> Option<()> {
        let root_id = self.modified_state_root_id(id)?;
        self.buffers.get_mut(&root_id)?.set_modified(flag);
        Some(())
    }

    pub fn restore_buffer_modified_state(&mut self, id: BufferId, flag: Value) -> Option<Value> {
        let root_id = self.modified_state_root_id(id)?;
        let out = self.buffers.get_mut(&root_id)?.restore_modified_state(flag);
        Some(out)
    }

    pub fn set_buffer_auto_saved(&mut self, id: BufferId) -> Option<()> {
        self.buffers.get_mut(&id)?.mark_auto_saved();
        Some(())
    }

    pub fn set_buffer_modified_tick(&mut self, id: BufferId, tick: i64) -> Option<()> {
        let root_id = self.modified_state_root_id(id)?;
        let buf = self.buffers.get_mut(&root_id)?;
        buf.text.set_modified_tick(tick);
        Some(())
    }

    pub fn set_buffer_file_name(&mut self, id: BufferId, file_name: Value) -> Option<()> {
        // Phase 10D: `buffer-file-name` and `buffer-file-truename`
        // both live in the slot table (BUFFER_SLOT_FILE_NAME /
        // BUFFER_SLOT_FILE_TRUENAME). Writing through
        // `set_file_name_value` covers buffer-file-name; mirror
        // the same value into buffer-file-truename via the slot
        // path. The legacy `buf.locals.set_raw_binding` calls
        // were dual-write dead code from before Phase 8b
        // migrated these names to BUFFER_SLOT_INFO.
        debug_assert!(file_name.is_nil() || file_name.is_string());
        let buf = self.buffers.get_mut(&id)?;
        buf.set_file_name_value(file_name);
        buf.slots[BUFFER_SLOT_FILE_TRUENAME] = file_name;
        Some(())
    }

    pub fn set_buffer_name(&mut self, id: BufferId, name: Value) -> Option<()> {
        self.buffers.get_mut(&id)?.set_name_value(name);
        Some(())
    }

    pub fn set_buffer_mark(&mut self, id: BufferId, pos: usize) -> Option<()> {
        self.buffers.get_mut(&id)?.set_mark_byte(pos);
        Some(())
    }

    pub fn clear_buffer_mark(&mut self, id: BufferId) -> Option<()> {
        let buf = self.buffers.get_mut(&id)?;
        buf.mark = None;
        buf.mark_byte = None;
        Some(())
    }

    pub fn set_buffer_local_property(
        &mut self,
        id: BufferId,
        name: &str,
        value: Value,
    ) -> Option<()> {
        self.buffers.get_mut(&id)?.set_buffer_local(name, value);
        Some(())
    }

    pub fn set_buffer_local_property_by_sym_id(
        &mut self,
        id: BufferId,
        sym_id: SymId,
        value: Value,
    ) -> Option<()> {
        self.buffers
            .get_mut(&id)?
            .set_buffer_local_by_sym_id(sym_id, value);
        Some(())
    }

    pub fn buffer_local_map(&self, id: BufferId) -> Option<Value> {
        Some(self.buffers.get(&id)?.local_map())
    }

    pub fn current_local_map(&self) -> Value {
        self.current
            .and_then(|id| self.buffer_local_map(id))
            .unwrap_or(Value::NIL)
    }

    pub fn set_buffer_local_map(&mut self, id: BufferId, keymap: Value) -> Option<()> {
        self.buffers.get_mut(&id)?.set_local_map(keymap);
        Some(())
    }

    pub fn set_current_local_map(&mut self, keymap: Value) -> Option<()> {
        let id = self.current?;
        self.set_buffer_local_map(id, keymap)
    }

    pub fn set_buffer_local_void_property(&mut self, id: BufferId, name: &str) -> Option<()> {
        self.buffers.get_mut(&id)?.set_buffer_local_void(name);
        Some(())
    }

    pub fn set_buffer_local_void_property_by_sym_id(
        &mut self,
        id: BufferId,
        sym_id: SymId,
    ) -> Option<()> {
        self.buffers
            .get_mut(&id)?
            .set_buffer_local_void_by_sym_id(sym_id);
        Some(())
    }

    pub fn remove_buffer_local_property(
        &mut self,
        id: BufferId,
        name: &str,
    ) -> Option<Option<RuntimeBindingValue>> {
        let buf = self.buffers.get_mut(&id)?;
        Some(buf.kill_buffer_local(name))
    }

    pub fn add_undo_boundary(&mut self, id: BufferId) -> Option<()> {
        let buf = self.buffers.get_mut(&id)?;
        let mut ul = buf.get_undo_list();
        undo::undo_list_boundary(&mut ul);
        // Periodically truncate the undo list to avoid unbounded growth.
        // Default limits match GNU Emacs: undo-limit=160000, undo-strong-limit=240000.
        ul = undo::truncate_undo_list(ul, 160_000, 240_000);
        buf.set_undo_list(ul);
        Some(())
    }

    pub fn restore_buffer_restriction(
        &mut self,
        id: BufferId,
        begv: usize,
        zv: usize,
    ) -> Option<()> {
        self.buffers.get_mut(&id)?.narrow_to_byte_region(begv, zv);
        let _ = self.record_buffer_state_markers(id);
        Some(())
    }

    pub fn save_current_restriction_state(&mut self) -> Option<SavedRestrictionState> {
        let buffer_id = self.current_buffer_id()?;
        let (begv, zv, len) = {
            let buffer = self.get(buffer_id)?;
            (buffer.begv_byte, buffer.zv_byte, buffer.total_bytes())
        };
        let restriction = if begv == 0 && zv == len {
            SavedRestrictionKind::None
        } else {
            let (beg_marker, _) = self.create_marker(buffer_id, begv, InsertionType::Before);
            let (end_marker, _) = self.create_marker(buffer_id, zv, InsertionType::After);
            SavedRestrictionKind::Markers {
                beg_marker,
                end_marker,
            }
        };
        let labeled_restrictions = self.clone_labeled_restrictions(buffer_id).unwrap_or(None);
        Some(SavedRestrictionState {
            buffer_id,
            restriction,
            labeled_restrictions,
        })
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub fn reset_outermost_restrictions(&mut self) -> OutermostRestrictionResetState {
        let mut affected_buffers: Vec<BufferId> =
            self.labeled_restrictions.keys().copied().collect();
        affected_buffers.sort_by_key(|buffer_id| buffer_id.0);

        let mut retained_buffers = Vec::with_capacity(affected_buffers.len());
        for buffer_id in affected_buffers {
            let Some((begv, zv)) = self.labeled_restriction_bounds(buffer_id, true) else {
                self.replace_labeled_restrictions(buffer_id, None);
                continue;
            };
            if self
                .restore_buffer_restriction(buffer_id, begv, zv)
                .is_some()
            {
                retained_buffers.push(buffer_id);
            } else {
                self.replace_labeled_restrictions(buffer_id, None);
            }
        }

        OutermostRestrictionResetState {
            affected_buffers: retained_buffers,
        }
    }

    #[tracing::instrument(level = "trace", skip(self, state))]
    pub fn restore_outermost_restrictions(&mut self, state: OutermostRestrictionResetState) {
        for buffer_id in state.affected_buffers {
            if let Some((begv, zv)) = self.current_labeled_restriction_bounds(buffer_id) {
                let _ = self.restore_buffer_restriction(buffer_id, begv, zv);
            } else {
                self.replace_labeled_restrictions(buffer_id, None);
            }
        }
    }

    pub fn restore_saved_restriction_state(&mut self, saved: SavedRestrictionState) {
        let buffer_id = saved.buffer_id;
        if self.buffers.get(&buffer_id).is_none() {
            self.replace_labeled_restrictions(buffer_id, None);
            return;
        }
        self.replace_labeled_restrictions(buffer_id, saved.labeled_restrictions);
        match saved.restriction {
            SavedRestrictionKind::None => {
                let _ = self.widen_buffer_fully(buffer_id);
            }
            SavedRestrictionKind::Markers {
                beg_marker,
                end_marker,
            } => {
                let beg = self.marker_position(buffer_id, beg_marker);
                let end = self.marker_position(buffer_id, end_marker);
                if let (Some(begv), Some(zv), Some(len)) = (
                    beg,
                    end,
                    self.buffers
                        .get(&buffer_id)
                        .map(|buffer| buffer.total_bytes()),
                ) {
                    let mut restored_begv = begv.min(len);
                    let mut restored_zv = zv.min(len);
                    if restored_begv > restored_zv {
                        std::mem::swap(&mut restored_begv, &mut restored_zv);
                    }
                    let _ = self.restore_buffer_restriction(buffer_id, restored_begv, restored_zv);
                }
                self.remove_marker(beg_marker);
                self.remove_marker(end_marker);
            }
        }
    }

    pub fn internal_labeled_narrow_to_region(
        &mut self,
        buffer_id: BufferId,
        start: usize,
        end: usize,
        label: Value,
    ) -> Option<()> {
        self.buffers.get(&buffer_id)?;
        if self.labeled_restriction_at(buffer_id, false).is_none() {
            self.push_labeled_restriction_for_current_bounds(
                buffer_id,
                LabeledRestrictionLabel::Outermost,
            )?;
        }
        self.restore_buffer_restriction(buffer_id, start, end)?;
        self.push_labeled_restriction_for_current_bounds(
            buffer_id,
            LabeledRestrictionLabel::User(label),
        )?;
        Some(())
    }

    pub fn internal_labeled_widen(&mut self, buffer_id: BufferId, label: &Value) -> Option<()> {
        self.buffers.get(&buffer_id)?;
        if self.current_labeled_restriction_matches_label(buffer_id, label) {
            let _ = self.pop_labeled_restriction(buffer_id);
        }
        self.widen_buffer(buffer_id)
    }

    pub fn configure_buffer_undo_list(&mut self, id: BufferId, value: Value) -> Option<()> {
        {
            let buf = self.buffers.get_mut(&id)?;
            match value.kind() {
                ValueKind::T => {
                    buf.set_buffer_local("buffer-undo-list", Value::T);
                }
                ValueKind::Nil => {
                    buf.set_buffer_local("buffer-undo-list", Value::NIL);
                    buf.undo_state.set_recorded_first_change(false);
                }
                other => {
                    buf.set_buffer_local("buffer-undo-list", value);
                }
            }
        }
        Some(())
    }

    pub fn undo_buffer(&mut self, id: BufferId, mut count: i64) -> Option<UndoExecutionResult> {
        let (had_any_records, had_boundary, previous_undoing, groups) = {
            let buffer = self.buffers.get_mut(&id)?;
            let ul = buffer.get_undo_list();

            let had_any_records = !undo::undo_list_is_empty(&ul);
            let had_boundary = undo::undo_list_contains_boundary(&ul);
            let had_trailing_boundary = undo::undo_list_has_trailing_boundary(&ul);

            if count <= 0 && had_boundary {
                return Some(UndoExecutionResult {
                    had_any_records,
                    had_boundary,
                    applied_any: false,
                    skipped_apply: true,
                });
            }
            if count <= 0 {
                count = 1;
            }

            let previous_undoing = buffer.undo_state.in_progress();
            buffer.undo_state.set_in_progress(true);
            let groups_to_undo = if had_trailing_boundary {
                count as usize
            } else {
                (count as usize).saturating_add(1)
            };

            let mut current_ul = ul;
            let mut groups = Vec::new();
            for _ in 0..groups_to_undo {
                let group = undo::undo_list_pop_group(&mut current_ul);
                if group.is_empty() {
                    break;
                }
                groups.push(group);
            }
            buffer.set_undo_list(current_ul);

            (had_any_records, had_boundary, previous_undoing, groups)
        };

        let mut applied_any = false;
        for group in groups {
            applied_any = true;
            for entry in group {
                if let Some(pt1) = entry.as_fixnum() {
                    // Cursor position (1-indexed)
                    let pos = (pt1 - 1).max(0) as usize;
                    let clamped = self
                        .buffers
                        .get(&id)
                        .map(|buffer| pos.min(buffer.total_bytes()))?;
                    self.goto_buffer_byte(id, clamped)?;
                } else if entry.is_cons() {
                    let car = entry.cons_car();
                    let cdr = entry.cons_cdr();
                    match (car.kind(), cdr.kind()) {
                        (ValueKind::Fixnum(beg1), ValueKind::Fixnum(end1)) => {
                            // Insert record: (BEG . END) — to undo, delete [beg, end)
                            let beg = (beg1 - 1).max(0) as usize;
                            let end = (end1 - 1).max(0) as usize;
                            let clamped_end = self
                                .buffers
                                .get(&id)
                                .map(|buffer| end.min(buffer.total_bytes()))?;
                            self.delete_buffer_region(id, beg.min(clamped_end), clamped_end)?;
                        }
                        (ValueKind::String, ValueKind::Fixnum(pos1)) => {
                            // Delete record: (TEXT . POS) — to undo, re-insert text
                            let text = car
                                .as_runtime_string_owned()
                                .expect("ValueKind::String must carry LispString payload");
                            let pos = (pos1.abs() - 1).max(0) as usize;
                            let clamped = self
                                .buffers
                                .get(&id)
                                .map(|buffer| pos.min(buffer.total_bytes()))?;
                            self.goto_buffer_byte(id, clamped)?;
                            self.insert_into_buffer(id, &text)?;
                        }
                        (ValueKind::T, ValueKind::Fixnum(_)) => {
                            // First-change sentinel (t . MODTIME) — skip
                        }
                        _ => {
                            // Other cons entries (e.g. property changes) — skip
                        }
                    }
                }
                // nil entries (boundaries within a group) are skipped
            }
        }

        self.buffers
            .get_mut(&id)?
            .undo_state
            .set_in_progress(previous_undoing);
        Some(UndoExecutionResult {
            had_any_records,
            had_boundary,
            applied_any,
            skipped_apply: false,
        })
    }

    /// Generate a unique buffer name.  If `base` is not taken, returns it
    /// unchanged; otherwise appends `<2>`, `<3>`, ... until a free name is
    /// found.
    pub fn generate_new_buffer_name(&self, base: &str) -> String {
        self.generate_new_buffer_name_ignoring(base, None)
    }

    /// Generate a unique buffer name, allowing `ignore` to be reused even if
    /// a live buffer already owns that name.
    pub fn generate_new_buffer_name_ignoring(&self, base: &str, ignore: Option<&str>) -> String {
        if ignore == Some(base) || self.find_buffer_by_name(base).is_none() {
            return base.to_string();
        }
        let mut n = 2u64;
        loop {
            let candidate = format!("{}<{}>", base, n);
            if ignore == Some(candidate.as_str()) || self.find_buffer_by_name(&candidate).is_none()
            {
                return candidate;
            }
            n += 1;
        }
    }

    /// Allocate a unique marker id without associating it with a buffer.
    pub fn allocate_marker_id(&mut self) -> u64 {
        let id = self.next_marker_id;
        self.next_marker_id += 1;
        id
    }

    /// Create a marker in `buffer_id` at byte position `pos` with the given
    /// insertion type.  Returns the new marker's id and the raw
    /// `MarkerObj` pointer for the backing allocation.
    ///
    /// The backing `MarkerObj` is allocated via the tagged heap and is
    /// tracked in `TaggedHeap::marker_ptrs`, so GC will see it. Callers
    /// that need to re-register this marker later (e.g. state-marker
    /// buffer switch plumbing) should retain the returned pointer and
    /// pass it to `register_marker_id` after first calling
    /// `chain_unlink` on the owning buffer's `BufferText` to satisfy
    /// the chain_splice_at_head precondition.
    pub fn create_marker(
        &mut self,
        buffer_id: BufferId,
        pos: usize,
        insertion_type: InsertionType,
    ) -> (u64, *mut crate::tagged::header::MarkerObj) {
        let marker_id = self.next_marker_id;
        self.next_marker_id += 1;
        // Allocate a backing MarkerObj so the new chain has a valid node.
        // Position fields are overwritten inside register_marker; starting
        // values are placeholders.
        let marker_value =
            crate::emacs_core::value::Value::make_marker(crate::heap_types::MarkerData {
                buffer: Some(buffer_id),
                insertion_type: insertion_type == InsertionType::After,
                marker_id: Some(marker_id),
                bytepos: 0,
                charpos: 0,
                next_marker: std::ptr::null_mut(),
            });
        let marker_ptr = marker_value
            .as_veclike_ptr()
            .expect("freshly allocated marker should have a veclike ptr")
            as *mut crate::tagged::header::MarkerObj;
        let _ = self.register_marker_id(marker_ptr, buffer_id, marker_id, pos, insertion_type);
        (marker_id, marker_ptr)
    }

    /// Register an existing marker id in `buffer_id` at byte position `pos`.
    pub fn register_marker_id(
        &mut self,
        marker_ptr: *mut crate::tagged::header::MarkerObj,
        buffer_id: BufferId,
        marker_id: u64,
        pos: usize,
        insertion_type: InsertionType,
    ) -> Option<()> {
        let buf = self.buffers.get_mut(&buffer_id)?;
        buf.register_marker(marker_ptr, marker_id, pos, insertion_type);
        Some(())
    }

    /// Query the current byte position of a marker.
    pub fn marker_position(&self, buffer_id: BufferId, marker_id: u64) -> Option<usize> {
        // T7: walk the chain rather than the deleted Vec<MarkerEntry>.
        self.buffers
            .get(&buffer_id)
            .and_then(|buf| buf.text.marker_chain_lookup(marker_id))
            .map(|(bytepos, _charpos, _ins)| bytepos)
    }

    /// Query the current character position of a marker.
    pub fn marker_char_position(&self, buffer_id: BufferId, marker_id: u64) -> Option<usize> {
        self.buffers
            .get(&buffer_id)
            .and_then(|buf| buf.text.marker_chain_lookup(marker_id))
            .map(|(_bytepos, charpos, _ins)| charpos)
    }

    /// Phase 10D: write the global default for a `BUFFER_OBJFWD`
    /// slot, propagating the new default to every live buffer
    /// whose `local_flags` bit for that slot is clear.
    ///
    /// Mirrors GNU `set_default_internal` SYMBOL_FORWARDED arm
    /// (`data.c:2044-2078`): the new default is stored in
    /// `buffer_defaults` and broadcast into every buffer that
    /// shares the global value (i.e. has not made the variable
    /// local). Always-local slots (`local_flags_idx == -1`) are
    /// per-buffer in every buffer, so the propagation is a no-op
    /// for them — only `buffer_defaults` is updated.
    pub fn set_buffer_default_slot(&mut self, info: &BufferSlotInfo, value: Value) {
        debug_assert!(info.offset < BUFFER_SLOT_COUNT);
        self.buffer_defaults[info.offset] = value;
        if info.local_flags_idx >= 0 {
            // Conditional slot: propagate to non-local buffers.
            for buf in self.buffers.values_mut() {
                if !buf.slot_local_flag(info.offset) {
                    buf.slots[info.offset] = value;
                }
            }
        }
    }

    /// Remove a marker registration from any live buffer.
    pub fn remove_marker(&mut self, marker_id: u64) {
        for buf in self.buffers.values_mut() {
            buf.remove_marker_entry(marker_id);
        }
    }

    /// Update the insertion type of a registered marker across all buffers.
    pub fn update_marker_insertion_type(&mut self, marker_id: u64, ins_type: InsertionType) {
        for buf in self.buffers.values_mut() {
            // T7: chain presence check replaces the deleted Vec-based
            // `marker_entry().is_some()`.
            if buf.text.has_marker(marker_id) {
                buf.update_marker_insertion_type(marker_id, ins_type);
                return;
            }
        }
    }

    // pdump accessors
    pub(crate) fn dump_buffers(&self) -> &HashMap<BufferId, Buffer> {
        &self.buffers
    }
    pub(crate) fn dump_current(&self) -> Option<BufferId> {
        self.current
    }
    pub(crate) fn dump_next_id(&self) -> u64 {
        self.next_id
    }
    pub(crate) fn dump_next_marker_id(&self) -> u64 {
        self.next_marker_id
    }
    pub(crate) fn from_dump(
        mut buffers: HashMap<BufferId, Buffer>,
        current: Option<BufferId>,
        next_id: u64,
        next_marker_id: u64,
        dumped_buffer_defaults: Option<&[crate::emacs_core::value::Value]>,
    ) -> Self {
        let indirect_buffers: Vec<(BufferId, BufferId)> = buffers
            .iter()
            .filter_map(|(id, buffer)| buffer.base_buffer.map(|base_id| (*id, base_id)))
            .collect();
        for (buffer_id, base_id) in indirect_buffers {
            // Borrow base buffer's text/undo state directly from the
            // map. `BufferText::Clone` is a deep clone (it allocates a
            // fresh `Rc<RefCell<BufferTextStorage>>`), so cloning a
            // base Buffer first and then `shared_clone`ing its text
            // would link the indirect buffer to the *temporary* base,
            // not the one in `buffers`. Use `shared_clone` directly on
            // the base buffer's `BufferText` to preserve Rc identity.
            let (shared_text, shared_undo) = match buffers.get(&base_id) {
                Some(root) => (root.text.shared_clone(), root.undo_state.clone()),
                None => continue,
            };
            let Some(buffer) = buffers.get_mut(&buffer_id) else {
                continue;
            };
            buffer.text = shared_text;
            buffer.undo_state = shared_undo;
        }

        // Seed `buffer_defaults` from BUFFER_SLOT_INFO's install-time
        // defaults, then overlay any values the dump carried. The
        // overlay preserves runtime defaults set by `setq-default`
        // during pdump creation (e.g. bindings.el's rich
        // `mode-line-format` list); the seed provides backward
        // compatibility for older dumps that didn't carry the
        // `buffer_defaults` field.
        let mut buffer_defaults = [crate::emacs_core::value::Value::NIL; BUFFER_SLOT_COUNT];
        for info in BUFFER_SLOT_INFO {
            buffer_defaults[info.offset] = info.default.to_value();
        }
        if let Some(dumped) = dumped_buffer_defaults {
            for (idx, value) in dumped.iter().enumerate() {
                if idx >= BUFFER_SLOT_COUNT {
                    break;
                }
                buffer_defaults[idx] = *value;
            }
        }
        let manager = Self {
            buffers,
            current,
            next_id,
            next_marker_id,
            labeled_restrictions: HashMap::new(),
            dead_buffer_last_names: HashMap::new(),
            buffer_defaults,
        };
        manager
    }
}

impl Default for BufferManager {
    fn default() -> Self {
        Self::new()
    }
}

impl GcTrace for BufferManager {
    fn trace_roots(&self, roots: &mut Vec<Value>) {
        for buffer in self.buffers.values() {
            roots.push(buffer.name);
            buffer.text.trace_text_prop_roots(roots);
            buffer.undo_state.trace_roots(roots);
            buffer.overlays.trace_roots(roots);
            // BUFFER_OBJFWD slot table holds Lisp values that must
            // be GC-rooted. Mirrors GNU's `mark_buffer` walking the
            // C-side BVAR slots in `alloc.c`.
            for slot in &buffer.slots {
                roots.push(*slot);
            }
            // Phase 10F: `local_var_alist` is the single source of
            // truth for non-slot per-buffer bindings. The cons
            // cells forming the alist must be rooted (along with
            // every entry's value). A single push of the alist
            // head is sufficient — the GC's reachability walk
            // follows the spine.
            roots.push(buffer.local_var_alist);
            // `local_map` (buffer's keymap) must also be rooted.
            roots.push(buffer.keymap);
        }
        for last_name in self.dead_buffer_last_names.values() {
            roots.push(*last_name);
        }
        // Phase 10D: `buffer_defaults` holds the global default
        // values for every per-buffer slot. Mirrors GNU's
        // `mark_buffer (&buffer_defaults)` in `alloc.c`.
        for slot in &self.buffer_defaults {
            roots.push(*slot);
        }
        for restrictions in self.labeled_restrictions.values() {
            for restriction in restrictions {
                if let LabeledRestrictionLabel::User(label) = restriction.label {
                    roots.push(label);
                }
            }
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[path = "buffer_test.rs"]
mod tests;
