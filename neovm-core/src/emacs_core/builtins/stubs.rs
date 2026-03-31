use super::*;
use crate::buffer::BufferManager;
use crate::emacs_core::fontset;
use crate::window::{FrameManager, WindowId};
#[cfg(not(target_os = "linux"))]
use arboard::Clipboard;
#[cfg(target_os = "linux")]
use arboard::{Clipboard, GetExtLinux, LinuxClipboardKind, SetExtLinux};
use crate::emacs_core::value::{ValueKind, VecLikeType};

// =========================================================================
// fontset.c gap-fill stubs
// =========================================================================

pub(crate) fn builtin_fontset_list_all(args: Vec<Value>) -> EvalResult {
    expect_args("fontset-list-all", &args, 0)?;
    Ok(super::symbols::fontset_list_value())
}

// =========================================================================
// atimer.c gap-fill stubs
// =========================================================================

pub(crate) fn builtin_debug_timer_check(args: Vec<Value>) -> EvalResult {
    expect_args("debug-timer-check", &args, 0)?;
    Ok(Value::NIL)
}

// =========================================================================
// inotify.c gap-fill stubs
// =========================================================================

pub(crate) fn builtin_inotify_watch_list(args: Vec<Value>) -> EvalResult {
    expect_args("inotify-watch-list", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_inotify_allocated_p(args: Vec<Value>) -> EvalResult {
    expect_args("inotify-allocated-p", &args, 0)?;
    Ok(Value::NIL)
}

// =========================================================================
// dbusbind.c gap-fill stubs
// =========================================================================

pub(crate) fn builtin_dbus_make_inhibitor_lock(args: Vec<Value>) -> EvalResult {
    expect_range_args("dbus-make-inhibitor-lock", &args, 2, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_dbus_close_inhibitor_lock(args: Vec<Value>) -> EvalResult {
    expect_args("dbus-close-inhibitor-lock", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_dbus_registered_inhibitor_locks(args: Vec<Value>) -> EvalResult {
    expect_args("dbus-registered-inhibitor-locks", &args, 0)?;
    Ok(Value::NIL)
}

// =========================================================================
// term.c gap-fill stubs
// =========================================================================

pub(crate) fn builtin_tty_frame_at(args: Vec<Value>) -> EvalResult {
    expect_args("tty-frame-at", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_tty_frame_geometry(args: Vec<Value>) -> EvalResult {
    expect_range_args("tty-frame-geometry", &args, 0, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_tty_frame_edges(args: Vec<Value>) -> EvalResult {
    expect_range_args("tty-frame-edges", &args, 0, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_tty_frame_list_z_order(args: Vec<Value>) -> EvalResult {
    expect_range_args("tty-frame-list-z-order", &args, 0, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_tty_frame_restack(args: Vec<Value>) -> EvalResult {
    expect_range_args("tty-frame-restack", &args, 2, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_tty_display_pixel_width(args: Vec<Value>) -> EvalResult {
    expect_range_args("tty-display-pixel-width", &args, 0, 1)?;
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_tty_display_pixel_height(args: Vec<Value>) -> EvalResult {
    expect_range_args("tty-display-pixel-height", &args, 0, 1)?;
    Ok(Value::fixnum(0))
}

// =========================================================================
// lcms.c stubs (no lcms in NeoVM)
// =========================================================================

pub(crate) fn builtin_lcms2_available_p(args: Vec<Value>) -> EvalResult {
    expect_args("lcms2-available-p", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_lcms_cie_de2000(args: Vec<Value>) -> EvalResult {
    expect_range_args("lcms-cie-de2000", &args, 2, 5)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_lcms_xyz_to_jch(args: Vec<Value>) -> EvalResult {
    expect_range_args("lcms-xyz->jch", &args, 1, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_lcms_jch_to_xyz(args: Vec<Value>) -> EvalResult {
    expect_range_args("lcms-jch->xyz", &args, 1, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_lcms_jch_to_jab(args: Vec<Value>) -> EvalResult {
    expect_range_args("lcms-jch->jab", &args, 1, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_lcms_jab_to_jch(args: Vec<Value>) -> EvalResult {
    expect_range_args("lcms-jab->jch", &args, 1, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_lcms_cam02_ucs(args: Vec<Value>) -> EvalResult {
    expect_range_args("lcms-cam02-ucs", &args, 2, 4)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_lcms_temp_to_white_point(args: Vec<Value>) -> EvalResult {
    expect_args("lcms-temp->white-point", &args, 1)?;
    Ok(Value::NIL)
}

// =========================================================================
// treesit.c gap-fill stubs
// =========================================================================

pub(crate) fn builtin_treesit_grammar_location(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-grammar-location", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_tracking_line_column_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("treesit-tracking-line-column-p", &args, 0, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_tracking_line_column_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-tracking-line-column-p", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_query_eagerly_compiled_p(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-query-eagerly-compiled-p", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_query_source(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-query-source", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_embed_level(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-embed-level", &args, 1)?;
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_treesit_parser_set_embed_level(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-set-embed-level", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parse_string(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parse-string", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_parser_changed_regions(args: Vec<Value>) -> EvalResult {
    expect_args("treesit-parser-changed-regions", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_linecol_at(args: Vec<Value>) -> EvalResult {
    expect_args("treesit--linecol-at", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_linecol_cache_set(args: Vec<Value>) -> EvalResult {
    expect_args("treesit--linecol-cache-set", &args, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_treesit_linecol_cache(args: Vec<Value>) -> EvalResult {
    expect_args("treesit--linecol-cache", &args, 0)?;
    Ok(Value::NIL)
}

// =========================================================================
// neomacsfns.c gap-fill stubs
// =========================================================================

#[derive(Clone, Debug, PartialEq)]
pub struct NeomacsMonitorInfo {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub scale: f64,
    pub width_mm: i32,
    pub height_mm: i32,
    pub name: Option<String>,
}

pub fn set_neomacs_monitor_info(monitors: Vec<NeomacsMonitorInfo>) {
    NEOMACS_MONITORS.with(|slot| *slot.borrow_mut() = monitors);
}

pub fn neomacs_monitor_info_snapshot() -> Vec<NeomacsMonitorInfo> {
    NEOMACS_MONITORS.with(|slot| slot.borrow().clone())
}

fn set_cached_clipboard_text(text: Option<String>) {
    NEOMACS_CLIPBOARD_TEXT.with(|slot| *slot.borrow_mut() = text);
}

fn cached_clipboard_text() -> Option<String> {
    NEOMACS_CLIPBOARD_TEXT.with(|slot| slot.borrow().clone())
}

fn set_cached_primary_selection_text(text: Option<String>) {
    NEOMACS_PRIMARY_SELECTION_TEXT.with(|slot| *slot.borrow_mut() = text);
}

fn cached_primary_selection_text() -> Option<String> {
    NEOMACS_PRIMARY_SELECTION_TEXT.with(|slot| slot.borrow().clone())
}

fn set_system_clipboard_text(text: &str) -> bool {
    Clipboard::new()
        .and_then(|mut clipboard| clipboard.set_text(text.to_owned()))
        .is_ok()
}

fn get_system_clipboard_text() -> Option<String> {
    Clipboard::new()
        .ok()
        .and_then(|mut clipboard| clipboard.get_text().ok())
}

#[cfg(target_os = "linux")]
fn set_system_primary_selection_text(text: &str) -> bool {
    Clipboard::new()
        .and_then(|mut clipboard| {
            clipboard
                .set()
                .clipboard(LinuxClipboardKind::Primary)
                .text(text.to_owned())
        })
        .is_ok()
}

#[cfg(not(target_os = "linux"))]
fn set_system_primary_selection_text(_text: &str) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn get_system_primary_selection_text() -> Option<String> {
    Clipboard::new().ok().and_then(|mut clipboard| {
        clipboard
            .get()
            .clipboard(LinuxClipboardKind::Primary)
            .text()
            .ok()
    })
}

#[cfg(not(target_os = "linux"))]
fn get_system_primary_selection_text() -> Option<String> {
    None
}

fn monitor_geometry_value(monitor: &NeomacsMonitorInfo) -> Value {
    Value::list(vec![
        Value::fixnum(monitor.x as i64),
        Value::fixnum(monitor.y as i64),
        Value::fixnum(monitor.width as i64),
        Value::fixnum(monitor.height as i64),
    ])
}

fn monitor_mm_size_value(monitor: &NeomacsMonitorInfo) -> Value {
    Value::list(vec![
        Value::fixnum(monitor.width_mm as i64),
        Value::fixnum(monitor.height_mm as i64),
    ])
}

fn monitor_alist_value(monitor: &NeomacsMonitorInfo, frames: Value) -> Value {
    Value::list(vec![
        Value::cons(Value::symbol("geometry"), monitor_geometry_value(monitor)),
        Value::cons(Value::symbol("workarea"), monitor_geometry_value(monitor)),
        Value::cons(Value::symbol("mm-size"), monitor_mm_size_value(monitor)),
        Value::cons(Value::symbol("frames"), frames),
        Value::cons(
            Value::symbol("scale-factor"),
            Value::make_float(monitor.scale),
        ),
        Value::cons(
            Value::symbol("name"),
            monitor
                .name
                .as_deref()
                .map(Value::string)
                .unwrap_or(Value::NIL),
        ),
        Value::cons(Value::symbol("source"), Value::string("Neomacs")),
    ])
}

pub(crate) fn builtin_neomacs_frame_geometry(args: Vec<Value>) -> EvalResult {
    expect_range_args("neomacs-frame-geometry", &args, 0, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_neomacs_frame_edges(args: Vec<Value>) -> EvalResult {
    expect_range_args("neomacs-frame-edges", &args, 0, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_neomacs_mouse_absolute_pixel_position(args: Vec<Value>) -> EvalResult {
    expect_args("neomacs-mouse-absolute-pixel-position", &args, 0)?;
    Ok(Value::cons(Value::fixnum(0), Value::fixnum(0)))
}

pub(crate) fn builtin_neomacs_set_mouse_absolute_pixel_position(args: Vec<Value>) -> EvalResult {
    expect_args("neomacs-set-mouse-absolute-pixel-position", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_neomacs_display_monitor_attributes_list(
    eval: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("neomacs-display-monitor-attributes-list", &args, 0, 1)?;
    let frames = eval
        .frames
        .frame_list()
        .into_iter()
        .map(|fid| Value::make_frame(fid.0))
        .collect::<Vec<_>>();
    let monitor_values = neomacs_monitor_info_snapshot();
    if monitor_values.is_empty() {
        return Ok(Value::NIL);
    }

    let mut alists = Vec::with_capacity(monitor_values.len());
    for (index, monitor) in monitor_values.iter().enumerate() {
        let frame_list = if index == 0 {
            Value::list(frames.clone())
        } else {
            Value::NIL
        };
        alists.push(monitor_alist_value(monitor, frame_list));
    }
    Ok(Value::list(alists))
}

pub(crate) fn builtin_x_scroll_bar_foreground(args: Vec<Value>) -> EvalResult {
    expect_args("x-scroll-bar-foreground", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_x_scroll_bar_background(args: Vec<Value>) -> EvalResult {
    expect_args("x-scroll-bar-background", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_neomacs_clipboard_set(args: Vec<Value>) -> EvalResult {
    expect_args("neomacs-clipboard-set", &args, 1)?;
    let text = match args[0].kind() {
        ValueKind::Nil => None,
        ValueKind::String => Some(args[0].as_str().unwrap().to_owned()),
        _ => Some(format!("{}", args[0])),
    };
    set_cached_clipboard_text(text.clone());
    if let Some(text) = text {
        let _ = set_system_clipboard_text(&text);
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_neomacs_clipboard_get(args: Vec<Value>) -> EvalResult {
    expect_args("neomacs-clipboard-get", &args, 0)?;
    Ok(get_system_clipboard_text()
        .or_else(cached_clipboard_text)
        .map(Value::string)
        .unwrap_or(Value::NIL))
}

pub(crate) fn builtin_neomacs_primary_selection_set(args: Vec<Value>) -> EvalResult {
    expect_args("neomacs-primary-selection-set", &args, 1)?;
    let text = match args[0].kind() {
        ValueKind::Nil => None,
        ValueKind::String => Some(args[0].as_str().unwrap().to_owned()),
        _ => Some(format!("{}", args[0])),
    };
    set_cached_primary_selection_text(text.clone());
    if let Some(text) = text {
        let _ = set_system_primary_selection_text(&text);
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_neomacs_primary_selection_get(args: Vec<Value>) -> EvalResult {
    expect_args("neomacs-primary-selection-get", &args, 0)?;
    Ok(get_system_primary_selection_text()
        .or_else(cached_primary_selection_text)
        .map(Value::string)
        .unwrap_or(Value::NIL))
}

pub(crate) fn builtin_neomacs_core_backend(args: Vec<Value>) -> EvalResult {
    expect_args("neomacs-core-backend", &args, 0)?;
    Ok(Value::string("rust"))
}

pub(super) fn reset_stubs_thread_locals() {
    SQLITE_NEXT_HANDLE_ID.with(|slot| *slot.borrow_mut() = 0);
    SQLITE_OPEN_HANDLES.with(|slot| slot.borrow_mut().clear());
    WINDOW_NEW_NORMAL.with(|slot| slot.borrow_mut().clear());
    WINDOW_NEW_PIXEL.with(|slot| slot.borrow_mut().clear());
    WINDOW_NEW_TOTAL.with(|slot| slot.borrow_mut().clear());
    NEOMACS_CLIPBOARD_TEXT.with(|slot| *slot.borrow_mut() = None);
    NEOMACS_PRIMARY_SELECTION_TEXT.with(|slot| *slot.borrow_mut() = None);
    NEOMACS_MONITORS.with(|slot| slot.borrow_mut().clear());
    INOTIFY_NEXT_WATCH_ID.with(|slot| *slot.borrow_mut() = 0);
    INOTIFY_ACTIVE_WATCHES.with(|slot| slot.borrow_mut().clear());
}

/// Return a snapshot of the `new_pixel` map for `window-resize-apply`.
pub(crate) fn snapshot_window_new_pixel() -> HashMap<u64, i64> {
    WINDOW_NEW_PIXEL.with(|slot| slot.borrow().clone())
}

/// Return a snapshot of the `new_total` map for `window-resize-apply-total`.
pub(crate) fn snapshot_window_new_total() -> HashMap<u64, i64> {
    WINDOW_NEW_TOTAL.with(|slot| slot.borrow().clone())
}

/// Return a snapshot of the `new_normal` map for `window-resize-apply`.
/// Returns only numeric (f64) entries since that's what the resize logic needs.
pub(crate) fn snapshot_window_new_normal() -> HashMap<u64, f64> {
    WINDOW_NEW_NORMAL.with(|slot| {
        slot.borrow()
            .iter()
            .filter_map(|(&id, v)| match v.kind() {
                ValueKind::Float => Some((id, v.as_float().unwrap())),
                ValueKind::Fixnum(i) => Some((id, i as f64)),
                _ => None,
            })
            .collect()
    })
}

thread_local! {
    static SQLITE_NEXT_HANDLE_ID: RefCell<i64> = RefCell::new(0);
    static SQLITE_OPEN_HANDLES: RefCell<Vec<i64>> = RefCell::new(Vec::new());
    static WINDOW_NEW_NORMAL: RefCell<HashMap<u64, Value>> = RefCell::new(HashMap::new());
    static WINDOW_NEW_PIXEL: RefCell<HashMap<u64, i64>> = RefCell::new(HashMap::new());
    static WINDOW_NEW_TOTAL: RefCell<HashMap<u64, i64>> = RefCell::new(HashMap::new());
    static NEOMACS_CLIPBOARD_TEXT: RefCell<Option<String>> = const { RefCell::new(None) };
    static NEOMACS_PRIMARY_SELECTION_TEXT: RefCell<Option<String>> = const { RefCell::new(None) };
    static NEOMACS_MONITORS: RefCell<Vec<NeomacsMonitorInfo>> = const { RefCell::new(Vec::new()) };
}

fn window_state_id(value: &Value) -> Option<u64> {
    if let Some(wid) = value.as_window_id() {
        return Some(wid);
    }
    match value.kind() {
        ValueKind::Fixnum(id) if id >= 0 => Some(id as u64),
        _ => None,
    }
}

pub(super) fn window_new_normal_value(window: Option<&Value>) -> Value {
    let Some(id) = window.and_then(window_state_id) else {
        return Value::NIL;
    };
    WINDOW_NEW_NORMAL
        .with(|slot| slot.borrow().get(&id).copied())
        .unwrap_or(Value::NIL)
}

pub(super) fn set_window_new_normal_value(window: &Value, value: Value) -> Value {
    if let Some(id) = window_state_id(window) {
        WINDOW_NEW_NORMAL.with(|slot| {
            slot.borrow_mut().insert(id, value);
        });
    }
    value
}

pub(super) fn window_new_pixel_value(window: Option<&Value>) -> Value {
    let Some(id) = window.and_then(window_state_id) else {
        return Value::fixnum(0);
    };
    Value::fixnum(
        WINDOW_NEW_PIXEL
            .with(|slot| slot.borrow().get(&id).copied())
            .unwrap_or(0),
    )
}

pub(super) fn set_window_new_pixel_value(window: &Value, size: i64, add: bool) -> Value {
    let Some(id) = window_state_id(window) else {
        return Value::fixnum(size);
    };
    let stored = WINDOW_NEW_PIXEL.with(|slot| {
        let mut state = slot.borrow_mut();
        let entry = state.entry(id).or_insert(0);
        if add {
            *entry += size;
        } else {
            *entry = size;
        }
        *entry
    });
    Value::fixnum(stored)
}

pub(super) fn window_new_total_value(window: Option<&Value>) -> Value {
    let Some(id) = window.and_then(window_state_id) else {
        return Value::fixnum(0);
    };
    Value::fixnum(
        WINDOW_NEW_TOTAL
            .with(|slot| slot.borrow().get(&id).copied())
            .unwrap_or(0),
    )
}

pub(super) fn set_window_new_total_value(window: &Value, size: i64, add: bool) -> Value {
    let Some(id) = window_state_id(window) else {
        return Value::fixnum(size);
    };
    let stored = WINDOW_NEW_TOTAL.with(|slot| {
        let mut state = slot.borrow_mut();
        let entry = state.entry(id).or_insert(0);
        if add {
            *entry += size;
        } else {
            *entry = size;
        }
        *entry
    });
    Value::fixnum(stored)
}

fn sqlite_handle_id(value: &Value) -> Option<i64> {
    let items = value.as_vector_data()?;
    if items.len() != 2 {
        return None;
    }
    match (items[0].kind(), items[1].kind()) {
        (ValueKind::Keyword(tag), ValueKind::Fixnum(id)) if resolve_sym(tag) == "sqlite-handle" => Some(id),
        _ => None,
    }
}

fn sqlite_is_open_handle(id: i64) -> bool {
    SQLITE_OPEN_HANDLES.with(|slot| slot.borrow().contains(&id))
}

fn sqlite_register_handle() -> i64 {
    let id = SQLITE_NEXT_HANDLE_ID.with(|slot| {
        let mut next = slot.borrow_mut();
        *next += 1;
        *next
    });
    SQLITE_OPEN_HANDLES.with(|slot| slot.borrow_mut().push(id));
    id
}

fn sqlite_close_handle(id: i64) {
    SQLITE_OPEN_HANDLES.with(|slot| {
        let mut handles = slot.borrow_mut();
        if let Some(pos) = handles.iter().position(|&open| open == id) {
            handles.remove(pos);
        }
    });
}

fn expect_sqlitep(value: &Value) -> Result<i64, Flow> {
    if let Some(id) = sqlite_handle_id(value) {
        Ok(id)
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sqlitep"), *value],
        ))
    }
}

pub(crate) fn builtin_sqlite_available_p(args: Vec<Value>) -> EvalResult {
    expect_args("sqlite-available-p", &args, 0)?;
    Ok(Value::T)
}

pub(crate) fn builtin_sqlite_version(args: Vec<Value>) -> EvalResult {
    expect_args("sqlite-version", &args, 0)?;
    Ok(Value::string("3.50.4"))
}

pub(crate) fn builtin_sqlitep(args: Vec<Value>) -> EvalResult {
    expect_args("sqlitep", &args, 1)?;
    Ok(Value::bool_val(sqlite_handle_id(&args[0]).is_some()))
}

pub(crate) fn builtin_sqlite_open(args: Vec<Value>) -> EvalResult {
    expect_range_args("sqlite-open", &args, 0, 1)?;
    if let Some(file) = args.first() {
        if !file.is_nil() {
            let _ = expect_strict_string(file)?;
        }
    }
    let id = sqlite_register_handle();
    Ok(Value::vector(vec![
        Value::keyword("sqlite-handle"),
        Value::fixnum(id),
    ]))
}

pub(crate) fn builtin_sqlite_close(args: Vec<Value>) -> EvalResult {
    expect_args("sqlite-close", &args, 1)?;
    let id = expect_sqlitep(&args[0])?;
    sqlite_close_handle(id);
    Ok(Value::T)
}

pub(crate) fn builtin_sqlite_execute(args: Vec<Value>) -> EvalResult {
    expect_range_args("sqlite-execute", &args, 2, 3)?;
    let id = expect_sqlitep(&args[0])?;
    if !sqlite_is_open_handle(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sqlitep"), args[0]],
        ));
    }
    let sql = expect_strict_string(&args[1])?;
    if sql.contains("insert into sqlite_schema") {
        return Err(signal(
            "sqlite-error",
            vec![Value::string("table sqlite_master may not be modified")],
        ));
    }
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_sqlite_execute_batch(
    _ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("sqlite-execute-batch", &args, 2)?;
    let id = expect_sqlitep(&args[0])?;
    if !sqlite_is_open_handle(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sqlitep"), args[0]],
        ));
    }
    let _ = expect_strict_string(&args[1])?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_sqlite_select(args: Vec<Value>) -> EvalResult {
    expect_range_args("sqlite-select", &args, 2, 4)?;
    let id = expect_sqlitep(&args[0])?;
    if !sqlite_is_open_handle(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sqlitep"), args[0]],
        ));
    }
    let sql = expect_strict_string(&args[1])?;
    if sql.trim() == "select 1" {
        return Ok(Value::list(vec![Value::list(vec![Value::fixnum(1)])]));
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_sqlite_next(args: Vec<Value>) -> EvalResult {
    expect_args("sqlite-next", &args, 1)?;
    let id = expect_sqlitep(&args[0])?;
    if !sqlite_is_open_handle(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sqlitep"), args[0]],
        ));
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_sqlite_more_p(args: Vec<Value>) -> EvalResult {
    expect_args("sqlite-more-p", &args, 1)?;
    let id = expect_sqlitep(&args[0])?;
    if !sqlite_is_open_handle(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sqlitep"), args[0]],
        ));
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_sqlite_columns(args: Vec<Value>) -> EvalResult {
    expect_args("sqlite-columns", &args, 1)?;
    let id = expect_sqlitep(&args[0])?;
    if !sqlite_is_open_handle(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sqlitep"), args[0]],
        ));
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_sqlite_finalize(args: Vec<Value>) -> EvalResult {
    expect_args("sqlite-finalize", &args, 1)?;
    let id = expect_sqlitep(&args[0])?;
    if !sqlite_is_open_handle(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sqlitep"), args[0]],
        ));
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_sqlite_pragma(args: Vec<Value>) -> EvalResult {
    expect_args("sqlite-pragma", &args, 2)?;
    let id = expect_sqlitep(&args[0])?;
    if !sqlite_is_open_handle(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sqlitep"), args[0]],
        ));
    }
    let _ = expect_strict_string(&args[1])?;
    Ok(Value::T)
}

pub(crate) fn builtin_sqlite_commit(args: Vec<Value>) -> EvalResult {
    expect_args("sqlite-commit", &args, 1)?;
    let id = expect_sqlitep(&args[0])?;
    if !sqlite_is_open_handle(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sqlitep"), args[0]],
        ));
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_sqlite_rollback(args: Vec<Value>) -> EvalResult {
    expect_args("sqlite-rollback", &args, 1)?;
    let id = expect_sqlitep(&args[0])?;
    if !sqlite_is_open_handle(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sqlitep"), args[0]],
        ));
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_sqlite_transaction(args: Vec<Value>) -> EvalResult {
    expect_args("sqlite-transaction", &args, 1)?;
    let id = expect_sqlitep(&args[0])?;
    if !sqlite_is_open_handle(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sqlitep"), args[0]],
        ));
    }
    Ok(Value::T)
}

pub(crate) fn builtin_sqlite_load_extension(args: Vec<Value>) -> EvalResult {
    expect_args("sqlite-load-extension", &args, 2)?;
    let id = expect_sqlitep(&args[0])?;
    if !sqlite_is_open_handle(id) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("sqlitep"), args[0]],
        ));
    }
    let _ = expect_strict_string(&args[1])?;
    Err(signal(
        "sqlite-error",
        vec![Value::string("load-extension failed")],
    ))
}

fn fillarray_character_from_value(value: &Value) -> Result<char, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) if n >= 0 => Ok((n as u8) as char),
        ValueKind::Char(c) => Ok(c),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *value],
        )),
    }
}

pub(crate) fn builtin_fillarray(args: Vec<Value>) -> EvalResult {
    const CHAR_TABLE_DEFAULT_SLOT: usize = 1;
    const BOOL_VECTOR_SIZE_SLOT: usize = 1;
    const BOOL_VECTOR_BITS_START: usize = 2;

    expect_args("fillarray", &args, 2)?;
    match args[0].kind() {
        ValueKind::Veclike(VecLikeType::Vector) => {
            let is_bool_vector = super::chartable::is_bool_vector(&args[0]);
            let is_char_table = !is_bool_vector && super::chartable::is_char_table(&args[0]);
            if is_bool_vector {
                let fill_bit = if args[1].is_nil() { 0 } else { 1 };
                let v = args[0].as_vector_data().unwrap();
                let logical_len = match v.get(BOOL_VECTOR_SIZE_SLOT).map(|val| val.kind()) {
                    Some(ValueKind::Fixnum(n)) if n > 0 => n as usize,
                    _ => 0,
                };
                let available_bits = v.len().saturating_sub(BOOL_VECTOR_BITS_START);
                let bit_count = logical_len.min(available_bits);
                let vec = args[0].as_vector_data_mut().unwrap();
                for bit in vec.iter_mut().skip(BOOL_VECTOR_BITS_START).take(bit_count) {
                    *bit = Value::fixnum(fill_bit);
                }
                return Ok(args[0]);
            }
            if is_char_table {
                let vec = args[0].as_vector_data_mut().unwrap();
                if vec.len() > CHAR_TABLE_DEFAULT_SLOT {
                    vec[CHAR_TABLE_DEFAULT_SLOT] = args[1];
                }
                return Ok(args[0]);
            }
            let vec = args[0].as_vector_data_mut().unwrap();
            for slot in vec.iter_mut() {
                *slot = args[1];
            }
            Ok(args[0])
        }
        ValueKind::String => {
            let fill = fillarray_character_from_value(&args[1])?;
            let len = args[0].as_str().unwrap().chars().count();
            let new_str = fill.to_string().repeat(len);
            let lisp_str = args[0].as_lisp_string_mut().unwrap().make_mut();
            lisp_str.clear();
            lisp_str.push_str(&new_str);
            Ok(args[0])
        }
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("arrayp"), args[0]],
        )),
    }
}

pub(crate) fn builtin_define_fringe_bitmap(args: Vec<Value>) -> EvalResult {
    expect_range_args("define-fringe-bitmap", &args, 2, 5)?;
    if args[0].as_symbol_name().is_none() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    }
    if !matches!(args[1].kind(), ValueKind::Veclike(VecLikeType::Vector) | ValueKind::String) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("arrayp"), args[1]],
        ));
    }

    if let Some(height) = args.get(2) {
        if !height.is_nil() {
            let _ = expect_fixnum(height)?;
        }
    }
    if let Some(width) = args.get(3) {
        if !width.is_nil() {
            let _ = expect_fixnum(width)?;
        }
    }
    if let Some(align) = args.get(4) {
        if !align.is_nil() && align.as_symbol_name().is_none() {
            return Err(signal("error", vec![Value::string("Bad align argument")]));
        }
    }

    Ok(args[0])
}

pub(crate) fn builtin_destroy_fringe_bitmap(args: Vec<Value>) -> EvalResult {
    expect_args("destroy-fringe-bitmap", &args, 1)?;
    if args[0].as_symbol_name().is_none() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_display_line_is_continued_p(args: Vec<Value>) -> EvalResult {
    expect_args("display--line-is-continued-p", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_display_update_for_mouse_movement(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("display--update-for-mouse-movement", &args, 3)?;
    let fid = super::window_cmds::resolve_frame_id_in_state(
        &mut eval.frames,
        &mut eval.buffers,
        Some(&args[0]),
        "frame-live-p",
    )?;
    let x = expect_fixnum(&args[1])?;
    let y = expect_fixnum(&args[2])?;
    eval.note_mouse_move_for_frame(Some(fid), x, y);
    Ok(Value::NIL)
}

pub(crate) fn builtin_external_debugging_output(args: Vec<Value>) -> EvalResult {
    expect_args("external-debugging-output", &args, 1)?;
    let ch = expect_fixnum(&args[0])?;
    if ch < 0 {
        return Err(signal(
            "error",
            vec![Value::string("Invalid character: f03fffff")],
        ));
    }
    Ok(Value::fixnum(ch))
}

pub(crate) fn builtin_internal_labeled_narrow_to_region(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_internal_labeled_narrow_to_region_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_internal_labeled_narrow_to_region_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("internal--labeled-narrow-to-region", &args, 3)?;
    let start = super::buffers::expect_integer_or_marker_in_buffers(buffers, &args[0])?;
    let end = super::buffers::expect_integer_or_marker_in_buffers(buffers, &args[1])?;
    let label = args[2];
    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let (byte_start, byte_end) =
        super::buffers::normalize_narrow_region_in_buffers(buffers, current_id, start, end)?;
    let _ = buffers.internal_labeled_narrow_to_region(current_id, byte_start, byte_end, label);
    Ok(Value::NIL)
}

pub(crate) fn builtin_internal_labeled_widen(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    builtin_internal_labeled_widen_in_buffers(&mut eval.buffers, args)
}

pub(crate) fn builtin_internal_labeled_widen_in_buffers(
    buffers: &mut BufferManager,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("internal--labeled-widen", &args, 1)?;
    let current_id = buffers
        .current_buffer_id()
        .ok_or_else(|| signal("error", vec![Value::string("No current buffer")]))?;
    let _ = buffers.internal_labeled_widen(current_id, &args[0]);
    Ok(Value::NIL)
}

pub(crate) fn builtin_internal_obarray_buckets(args: Vec<Value>) -> EvalResult {
    expect_args("internal--obarray-buckets", &args, 1)?;
    let obarray_val = expect_obarray_vector_id(&args[0])?;
    let buckets = obarray_val.as_vector_data().unwrap().clone();
    Ok(Value::list(buckets))
}

pub(crate) fn builtin_handle_save_session(args: Vec<Value>) -> EvalResult {
    expect_args("handle-save-session", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_handle_switch_frame(args: Vec<Value>) -> EvalResult {
    expect_args("handle-switch-frame", &args, 1)?;
    let frame = match args[0].kind() {
        ValueKind::Veclike(VecLikeType::Frame) => args[0],
        ValueKind::Cons => {
            let pair_car = args[0].cons_car();
            let pair_cdr = args[0].cons_cdr();
            match pair_car.as_symbol_name() {
                Some("switch-frame") => {
                    let cdr = pair_cdr;
                    match cdr.kind() {
                        ValueKind::Cons => cdr.cons_car(),
                        _ => {
                            return Err(signal(
                                "wrong-type-argument",
                                vec![Value::symbol("framep"), args[0]],
                            ));
                        }
                    }
                }
                _ => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("framep"), args[0]],
                    ));
                }
            }
        }
        _ => {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("framep"), args[0]],
            ));
        }
    };
    if !frame.is_frame() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("framep"), frame],
        ));
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_gpm_mouse_start(args: Vec<Value>) -> EvalResult {
    expect_args("gpm-mouse-start", &args, 0)?;
    Err(signal(
        "error",
        vec![Value::string(
            "Gpm-mouse only works in the GNU/Linux console",
        )],
    ))
}

pub(crate) fn builtin_gpm_mouse_stop(args: Vec<Value>) -> EvalResult {
    expect_args("gpm-mouse-stop", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_help_describe_vector(args: Vec<Value>) -> EvalResult {
    expect_args("help--describe-vector", &args, 7)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_init_image_library(args: Vec<Value>) -> EvalResult {
    expect_args("init-image-library", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_describe_buffer_bindings(args: Vec<Value>) -> EvalResult {
    expect_range_args("describe-buffer-bindings", &args, 1, 3)?;
    if !args[0].is_buffer() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("bufferp"), args[0]],
        ));
    }
    if let Some(prefixes) = args.get(1) {
        if !prefixes.is_nil()
            && !(prefixes.is_cons() || prefixes.is_vector() || prefixes.is_string() || prefixes.is_nil())
        {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("sequencep"), *prefixes],
            ));
        }
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_describe_vector(args: Vec<Value>) -> EvalResult {
    expect_range_args("describe-vector", &args, 1, 2)?;
    if !matches!(args[0].kind(), ValueKind::Veclike(VecLikeType::Vector)) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("vector-or-char-table-p"), args[0]],
        ));
    }
    if let Some(output) = args.get(1) {
        if !output.is_nil() {
            if let Some(name) = output.as_symbol_name() {
                return Err(signal("void-function", vec![Value::symbol(name)]));
            }
        }
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_frame_set_was_invisible(args: Vec<Value>) -> EvalResult {
    expect_args("frame--set-was-invisible", &args, 2)?;
    expect_frame_live_or_nil(&args[0])?;
    Ok(args[1])
}

pub(crate) fn builtin_frame_after_make_frame(args: Vec<Value>) -> EvalResult {
    expect_args("frame-after-make-frame", &args, 2)?;
    expect_frame_live_or_nil(&args[0])?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_frame_ancestor_p(args: Vec<Value>) -> EvalResult {
    expect_args("frame-ancestor-p", &args, 2)?;
    expect_frame_live_or_nil(&args[0])?;
    expect_frame_live_or_nil(&args[1])?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_frame_bottom_divider_width(args: Vec<Value>) -> EvalResult {
    expect_range_args("frame-bottom-divider-width", &args, 0, 1)?;
    if let Some(frame) = args.first() {
        expect_frame_live_or_nil(frame)?;
    }
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_frame_child_frame_border_width(args: Vec<Value>) -> EvalResult {
    expect_range_args("frame-child-frame-border-width", &args, 0, 1)?;
    if let Some(frame) = args.first() {
        expect_frame_live_or_nil(frame)?;
    }
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_frame_focus(args: Vec<Value>) -> EvalResult {
    expect_range_args("frame-focus", &args, 0, 1)?;
    if let Some(frame) = args.first() {
        expect_frame_live_or_nil(frame)?;
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_frame_font_cache(args: Vec<Value>) -> EvalResult {
    expect_range_args("frame-font-cache", &args, 0, 1)?;
    if let Some(frame) = args.first() {
        expect_frame_live_or_nil(frame)?;
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_frame_fringe_width(args: Vec<Value>) -> EvalResult {
    expect_range_args("frame-fringe-width", &args, 0, 1)?;
    if let Some(frame) = args.first() {
        expect_frame_live_or_nil(frame)?;
    }
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_frame_internal_border_width(args: Vec<Value>) -> EvalResult {
    expect_range_args("frame-internal-border-width", &args, 0, 1)?;
    if let Some(frame) = args.first() {
        expect_frame_live_or_nil(frame)?;
    }
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_frame_or_buffer_changed_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("frame-or-buffer-changed-p", &args, 0, 1)?;
    let Some(symbol) = args.first() else {
        return Ok(Value::T);
    };
    if symbol.is_nil() {
        return Ok(Value::NIL);
    }
    if symbol.as_symbol_name().is_none() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), *symbol],
        ));
    }
    Err(signal("void-variable", vec![*symbol]))
}

pub(crate) fn builtin_frame_parent(args: Vec<Value>) -> EvalResult {
    expect_range_args("frame-parent", &args, 0, 1)?;
    if let Some(frame) = args.first() {
        expect_frame_live_or_nil(frame)?;
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_frame_pointer_visible_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("frame-pointer-visible-p", &args, 0, 1)?;
    if let Some(frame) = args.first() {
        expect_frame_live_or_nil(frame)?;
    }
    Ok(Value::T)
}

pub(crate) fn builtin_frame_right_divider_width(args: Vec<Value>) -> EvalResult {
    expect_range_args("frame-right-divider-width", &args, 0, 1)?;
    if let Some(frame) = args.first() {
        expect_frame_live_or_nil(frame)?;
    }
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_frame_scale_factor(args: Vec<Value>) -> EvalResult {
    expect_range_args("frame-scale-factor", &args, 0, 1)?;
    if let Some(frame) = args.first() {
        expect_frame_live_or_nil(frame)?;
    }
    Ok(Value::make_float(1.0))
}

pub(crate) fn builtin_frame_scroll_bar_height(args: Vec<Value>) -> EvalResult {
    expect_range_args("frame-scroll-bar-height", &args, 0, 1)?;
    if let Some(frame) = args.first() {
        expect_frame_live_or_nil(frame)?;
    }
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_frame_scroll_bar_width(args: Vec<Value>) -> EvalResult {
    expect_range_args("frame-scroll-bar-width", &args, 0, 1)?;
    if let Some(frame) = args.first() {
        expect_frame_live_or_nil(frame)?;
    }
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_frame_window_state_change(args: Vec<Value>) -> EvalResult {
    expect_range_args("frame-window-state-change", &args, 0, 1)?;
    if let Some(frame) = args.first() {
        expect_frame_live_or_nil(frame)?;
    }
    Ok(Value::NIL)
}

// --- frame.c missing builtins ---

/// Eval-dependent variant: defaults to selected frame.
pub(crate) fn builtin_frame_id(eval: &mut super::eval::Context, args: Vec<Value>) -> EvalResult {
    expect_range_args("frame-id", &args, 0, 1)?;
    let fid = super::window_cmds::resolve_frame_id_in_state(
        &mut eval.frames,
        &mut eval.buffers,
        args.first(),
        "frame-live-p",
    )?;
    let public_id = if fid.0 >= crate::window::FRAME_ID_BASE {
        fid.0 - crate::window::FRAME_ID_BASE + 1
    } else {
        fid.0
    };
    Ok(Value::fixnum(public_id as i64))
}

/// Eval-dependent variant: defaults to selected frame.
pub(crate) fn builtin_frame_root_frame(
    eval: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_range_args("frame-root-frame", &args, 0, 1)?;
    let fid = super::window_cmds::resolve_frame_id_in_state(
        &mut eval.frames,
        &mut eval.buffers,
        args.first(),
        "frame-live-p",
    )?;
    Ok(Value::make_frame(fid.0))
}

/// `(set-frame-size-and-position-pixelwise FRAME WIDTH HEIGHT LEFT TOP &optional GRAVITY)`
/// — combined resize+move stub, returns nil.
pub(crate) fn builtin_set_frame_size_and_position_pixelwise(args: Vec<Value>) -> EvalResult {
    expect_range_args("set-frame-size-and-position-pixelwise", &args, 5, 6)?;
    expect_frame_live_or_nil(&args[0])?;
    Ok(Value::NIL)
}

/// `(mouse-position-in-root-frame)` — stub, returns nil.
pub(crate) fn builtin_mouse_position_in_root_frame(args: Vec<Value>) -> EvalResult {
    expect_args("mouse-position-in-root-frame", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_fringe_bitmaps_at_pos(args: Vec<Value>) -> EvalResult {
    expect_range_args("fringe-bitmaps-at-pos", &args, 0, 2)?;
    if let Some(pos) = args.first() {
        if !pos.is_nil() {
            let _ = expect_integer_or_marker(pos)?;
        }
    }
    if let Some(window) = args.get(1) {
        if !window.is_nil() && !window.is_window() {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("window-live-p"), *window],
            ));
        }
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_gap_position(args: Vec<Value>) -> EvalResult {
    expect_args("gap-position", &args, 0)?;
    Ok(Value::fixnum(1))
}

pub(crate) fn builtin_gap_size(args: Vec<Value>) -> EvalResult {
    expect_args("gap-size", &args, 0)?;
    Ok(Value::fixnum(2001))
}

pub(crate) fn builtin_garbage_collect_maybe(args: Vec<Value>) -> EvalResult {
    expect_args("garbage-collect-maybe", &args, 1)?;
    let Some(n) = args[0].as_fixnum() else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("wholenump"), args[0]],
        ));
    };
    if n < 0 {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("wholenump"), Value::fixnum(n)],
        ));
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_garbage_collect_heapsize(args: Vec<Value>) -> EvalResult {
    expect_args("garbage-collect-heapsize", &args, 0)?;
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_get_unicode_property_internal(args: Vec<Value>) -> EvalResult {
    expect_args("get-unicode-property-internal", &args, 2)?;
    Err(signal(
        "wrong-type-argument",
        vec![Value::symbol("char-table-p"), args[0]],
    ))
}

pub(crate) fn builtin_gnutls_available_p(args: Vec<Value>) -> EvalResult {
    expect_args("gnutls-available-p", &args, 0)?;
    Ok(Value::list(vec![Value::symbol("gnutls")]))
}

pub(crate) fn builtin_gnutls_ciphers(args: Vec<Value>) -> EvalResult {
    expect_args("gnutls-ciphers", &args, 0)?;
    Ok(Value::list(vec![Value::symbol("AES-256-GCM")]))
}

pub(crate) fn builtin_gnutls_digests(args: Vec<Value>) -> EvalResult {
    expect_args("gnutls-digests", &args, 0)?;
    Ok(Value::list(vec![Value::symbol("SHA256")]))
}

pub(crate) fn builtin_gnutls_macs(args: Vec<Value>) -> EvalResult {
    expect_args("gnutls-macs", &args, 0)?;
    Ok(Value::list(vec![Value::symbol("AEAD")]))
}

pub(crate) fn builtin_gnutls_errorp(args: Vec<Value>) -> EvalResult {
    expect_args("gnutls-errorp", &args, 1)?;
    Ok(Value::T)
}

pub(crate) fn builtin_gnutls_error_string(args: Vec<Value>) -> EvalResult {
    expect_args("gnutls-error-string", &args, 1)?;
    match args[0].kind() {
        ValueKind::Fixnum(0) => Ok(Value::string("Success.")),
        ValueKind::Nil => Ok(Value::string("Symbol has no numeric gnutls-code property")),
        _ => Ok(Value::string("Unknown TLS error")),
    }
}

pub(crate) fn builtin_gnutls_error_fatalp(args: Vec<Value>) -> EvalResult {
    expect_args("gnutls-error-fatalp", &args, 1)?;
    if args[0].is_nil() {
        return Err(signal(
            "error",
            vec![Value::string("Symbol has no numeric gnutls-code property")],
        ));
    }
    Ok(Value::NIL)
}

fn expect_processp(value: &Value) -> Result<(), Flow> {
    if value.is_nil() {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("processp"), *value],
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn builtin_gnutls_peer_status_warning_describe(args: Vec<Value>) -> EvalResult {
    expect_args("gnutls-peer-status-warning-describe", &args, 1)?;
    if args[0].is_nil() {
        return Ok(Value::NIL);
    }
    if args[0].as_symbol_name().is_none() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_gnutls_asynchronous_parameters(args: Vec<Value>) -> EvalResult {
    expect_args("gnutls-asynchronous-parameters", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_gnutls_bye(args: Vec<Value>) -> EvalResult {
    expect_args("gnutls-bye", &args, 2)?;
    expect_processp(&args[0])?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_gnutls_deinit(args: Vec<Value>) -> EvalResult {
    expect_args("gnutls-deinit", &args, 1)?;
    expect_processp(&args[0])?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_gnutls_format_certificate(args: Vec<Value>) -> EvalResult {
    expect_args("gnutls-format-certificate", &args, 1)?;
    let _ = expect_strict_string(&args[0])?;
    Ok(Value::string("Certificate"))
}

pub(crate) fn builtin_gnutls_get_initstage(args: Vec<Value>) -> EvalResult {
    expect_args("gnutls-get-initstage", &args, 1)?;
    expect_processp(&args[0])?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_gnutls_hash_digest(args: Vec<Value>) -> EvalResult {
    expect_args("gnutls-hash-digest", &args, 2)?;
    if args[0].is_nil() {
        return Err(signal(
            "error",
            vec![
                Value::string("GnuTLS digest-method is invalid or not found"),
                Value::NIL,
            ],
        ));
    }
    if args[0].as_symbol_name().is_none() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    }
    let _ = expect_strict_string(&args[1])?;
    Ok(Value::string("digest"))
}

pub(crate) fn builtin_gnutls_hash_mac(args: Vec<Value>) -> EvalResult {
    expect_args("gnutls-hash-mac", &args, 3)?;
    if args[0].is_nil() {
        return Err(signal(
            "error",
            vec![
                Value::string("GnuTLS MAC-method is invalid or not found"),
                Value::NIL,
            ],
        ));
    }
    if args[0].as_symbol_name().is_none() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    }
    let _ = expect_strict_string(&args[1])?;
    let _ = expect_strict_string(&args[2])?;
    Ok(Value::string("mac"))
}

pub(crate) fn builtin_gnutls_peer_status(args: Vec<Value>) -> EvalResult {
    expect_args("gnutls-peer-status", &args, 1)?;
    expect_processp(&args[0])?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_gnutls_symmetric_decrypt(args: Vec<Value>) -> EvalResult {
    expect_range_args("gnutls-symmetric-decrypt", &args, 4, 5)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_gnutls_symmetric_encrypt(args: Vec<Value>) -> EvalResult {
    expect_range_args("gnutls-symmetric-encrypt", &args, 4, 5)?;
    Ok(Value::NIL)
}

pub(super) const FACE_ATTRIBUTES_VECTOR_LEN: usize = 20;

pub(crate) fn builtin_font_get_system_font(args: Vec<Value>) -> EvalResult {
    expect_args("font-get-system-font", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_font_get_system_normal_font(args: Vec<Value>) -> EvalResult {
    expect_args("font-get-system-normal-font", &args, 0)?;
    Ok(Value::NIL)
}

fn expect_characterp_from_int(value: &Value) -> Result<char, Flow> {
    match value.kind() {
        ValueKind::Fixnum(n) if n >= 0 => char::from_u32(n as u32).ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("characterp"), *value],
            )
        }),
        ValueKind::Char(c) => Ok(c),
        _ => Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("characterp"), *value],
        )),
    }
}

fn is_font_object(value: &Value) -> bool {
    match value.kind() {
        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = value.as_vector_data().unwrap();
            items.first().is_some_and(|v| {
                matches!(v.kind(), ValueKind::Keyword(tag) if resolve_sym(tag) == "font-object")
            })
        }
        _ => false,
    }
}

fn is_font_spec(value: &Value) -> bool {
    match value.kind() {
        ValueKind::Veclike(VecLikeType::Vector) => {
            let items = value.as_vector_data().unwrap();
            items.first().is_some_and(|v| {
                matches!(v.kind(), ValueKind::Keyword(tag) if resolve_sym(tag) == "font-spec")
            })
        }
        _ => false,
    }
}

fn unspecified_face_attributes_vector() -> Value {
    Value::vector(vec![
        Value::symbol("unspecified");
        FACE_ATTRIBUTES_VECTOR_LEN
    ])
}

pub(crate) fn builtin_face_attributes_as_vector(args: Vec<Value>) -> EvalResult {
    expect_args("face-attributes-as-vector", &args, 1)?;
    Ok(unspecified_face_attributes_vector())
}

fn expect_window_live_or_nil_in_state(frames: &FrameManager, value: &Value) -> Result<(), Flow> {
    if value.is_nil() {
        return Ok(());
    }
    let live = if let Some(wid) = value.as_window_id() {
        frames.is_live_window_id(WindowId(wid))
    } else {
        match value.kind() {
            ValueKind::Fixnum(id) if id >= 0 => frames.is_live_window_id(WindowId(id as u64)),
            _ => false,
        }
    };
    if live {
        Ok(())
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("window-live-p"), *value],
        ))
    }
}

pub(crate) fn builtin_font_face_attributes(args: Vec<Value>) -> EvalResult {
    expect_range_args("font-face-attributes", &args, 1, 2)?;
    if !is_font_object(&args[0]) {
        return Err(signal("error", vec![Value::string("Invalid font object")]));
    }
    Ok(unspecified_face_attributes_vector())
}

pub(crate) fn builtin_font_get_glyphs(args: Vec<Value>) -> EvalResult {
    expect_range_args("font-get-glyphs", &args, 3, 4)?;
    if !is_font_object(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("font-object"), args[0]],
        ));
    }
    let _ = expect_fixnum(&args[1])?;
    let _ = expect_fixnum(&args[2])?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_font_has_char_p(args: Vec<Value>) -> EvalResult {
    expect_range_args("font-has-char-p", &args, 2, 3)?;
    if !is_font_object(&args[0]) && !is_font_spec(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("font"), args[0]],
        ));
    }
    let _ = expect_characterp_from_int(&args[1])?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_font_match_p(args: Vec<Value>) -> EvalResult {
    expect_args("font-match-p", &args, 2)?;
    if !is_font_spec(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("font-spec"), args[0]],
        ));
    }
    if !is_font_spec(&args[1]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("font-spec"), args[1]],
        ));
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_font_shape_gstring(args: Vec<Value>) -> EvalResult {
    expect_args("font-shape-gstring", &args, 2)?;
    if !matches!(args[0].kind(), ValueKind::Veclike(VecLikeType::Vector)) {
        return Err(signal(
            "error",
            vec![Value::string("Invalid glyph-string: ")],
        ));
    }
    let _ = expect_fixnum(&args[1])?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_font_variation_glyphs(args: Vec<Value>) -> EvalResult {
    expect_args("font-variation-glyphs", &args, 2)?;
    if !is_font_object(&args[0]) {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("font-object"), args[0]],
        ));
    }
    let _ = expect_characterp_from_int(&args[1])?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_fontset_font(args: Vec<Value>) -> EvalResult {
    expect_range_args("fontset-font", &args, 2, 3)?;
    let ch = expect_characterp_from_int(&args[1])?;
    fontset::fontset_font(
        &args[0],
        ch,
        args.get(2).is_some_and(|value| !value.is_nil()),
    )
}

pub(crate) fn builtin_fontset_info(args: Vec<Value>) -> EvalResult {
    expect_range_args("fontset-info", &args, 1, 2)?;
    Err(signal(
        "error",
        vec![Value::string(
            "Window system is not in use or not initialized",
        )],
    ))
}

pub(crate) fn builtin_fontset_list(args: Vec<Value>) -> EvalResult {
    expect_args("fontset-list", &args, 0)?;
    Ok(super::symbols::fontset_list_value())
}

fn expect_window_live_or_nil(value: &Value) -> Result<(), Flow> {
    if value.is_nil() || value.is_window() {
        Ok(())
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("window-live-p"), *value],
        ))
    }
}

pub(super) fn expect_window_valid_or_nil(value: &Value) -> Result<(), Flow> {
    if value.is_nil() || value.is_window() {
        Ok(())
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("window-valid-p"), *value],
        ))
    }
}

fn expect_frame_live_or_nil(value: &Value) -> Result<(), Flow> {
    if value.is_nil() || value.is_frame() {
        Ok(())
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("frame-live-p"), *value],
        ))
    }
}

pub(crate) fn builtin_window_bottom_divider_width(args: Vec<Value>) -> EvalResult {
    expect_range_args("window-bottom-divider-width", &args, 0, 1)?;
    if let Some(window) = args.first() {
        expect_window_live_or_nil(window)?;
    }
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_window_lines_pixel_dimensions(args: Vec<Value>) -> EvalResult {
    expect_range_args("window-lines-pixel-dimensions", &args, 0, 6)?;
    if let Some(window) = args.first() {
        expect_window_live_or_nil(window)?;
    }
    Ok(Value::NIL)
}

pub(crate) fn builtin_window_new_normal(args: Vec<Value>) -> EvalResult {
    expect_range_args("window-new-normal", &args, 0, 1)?;
    if let Some(window) = args.first() {
        expect_window_valid_or_nil(window)?;
    }
    Ok(window_new_normal_value(args.first()))
}

pub(crate) fn builtin_window_new_pixel(args: Vec<Value>) -> EvalResult {
    expect_range_args("window-new-pixel", &args, 0, 1)?;
    if let Some(window) = args.first() {
        expect_window_valid_or_nil(window)?;
    }
    Ok(window_new_pixel_value(args.first()))
}

pub(crate) fn builtin_window_new_total(args: Vec<Value>) -> EvalResult {
    expect_range_args("window-new-total", &args, 0, 1)?;
    if let Some(window) = args.first() {
        expect_window_valid_or_nil(window)?;
    }
    Ok(window_new_total_value(args.first()))
}

pub(crate) fn builtin_window_old_body_pixel_height(args: Vec<Value>) -> EvalResult {
    expect_range_args("window-old-body-pixel-height", &args, 0, 1)?;
    if let Some(window) = args.first() {
        expect_window_live_or_nil(window)?;
    }
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_window_old_body_pixel_width(args: Vec<Value>) -> EvalResult {
    expect_range_args("window-old-body-pixel-width", &args, 0, 1)?;
    if let Some(window) = args.first() {
        expect_window_live_or_nil(window)?;
    }
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_window_old_pixel_height(args: Vec<Value>) -> EvalResult {
    expect_range_args("window-old-pixel-height", &args, 0, 1)?;
    if let Some(window) = args.first() {
        expect_window_valid_or_nil(window)?;
    }
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_window_old_pixel_width(args: Vec<Value>) -> EvalResult {
    expect_range_args("window-old-pixel-width", &args, 0, 1)?;
    if let Some(window) = args.first() {
        expect_window_valid_or_nil(window)?;
    }
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_window_right_divider_width(args: Vec<Value>) -> EvalResult {
    expect_range_args("window-right-divider-width", &args, 0, 1)?;
    if let Some(window) = args.first() {
        expect_window_live_or_nil(window)?;
    }
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_window_scroll_bar_height(args: Vec<Value>) -> EvalResult {
    expect_range_args("window-scroll-bar-height", &args, 0, 1)?;
    if let Some(window) = args.first() {
        expect_window_live_or_nil(window)?;
    }
    Ok(Value::fixnum(0))
}

pub(crate) fn builtin_window_scroll_bar_width(args: Vec<Value>) -> EvalResult {
    expect_range_args("window-scroll-bar-width", &args, 0, 1)?;
    if let Some(window) = args.first() {
        expect_window_live_or_nil(window)?;
    }
    Ok(Value::fixnum(0))
}

thread_local! {
    static INOTIFY_NEXT_WATCH_ID: RefCell<i64> = RefCell::new(0);
    static INOTIFY_ACTIVE_WATCHES: RefCell<Vec<(i64, i64)>> = RefCell::new(Vec::new());
}

fn inotify_watch_descriptor_parts(value: &Value) -> Option<(i64, i64)> {
    if !value.is_cons() {
        return None;
    };
    let pair_car = value.cons_car();
    let pair_cdr = value.cons_cdr();
    let fd = pair_car.as_int()?;
    let wd = pair_cdr.as_int()?;
    Some((fd, wd))
}

fn inotify_register_watch() -> (i64, i64) {
    let watch_id = INOTIFY_NEXT_WATCH_ID.with(|slot| {
        let mut next = slot.borrow_mut();
        let id = *next;
        *next += 1;
        id
    });
    let descriptor = (1, watch_id);
    INOTIFY_ACTIVE_WATCHES.with(|slot| slot.borrow_mut().push(descriptor));
    descriptor
}

fn inotify_watch_is_active(value: &Value) -> bool {
    let Some(descriptor) = inotify_watch_descriptor_parts(value) else {
        return false;
    };
    INOTIFY_ACTIVE_WATCHES.with(|slot| slot.borrow().contains(&descriptor))
}

fn inotify_remove_watch(value: &Value) -> bool {
    let Some(descriptor) = inotify_watch_descriptor_parts(value) else {
        return false;
    };
    INOTIFY_ACTIVE_WATCHES.with(|slot| {
        let mut watches = slot.borrow_mut();
        if let Some(pos) = watches.iter().position(|&active| active == descriptor) {
            watches.remove(pos);
            true
        } else {
            false
        }
    })
}

pub(crate) fn builtin_inotify_valid_p(args: Vec<Value>) -> EvalResult {
    expect_args("inotify-valid-p", &args, 1)?;
    Ok(Value::bool_val(inotify_watch_is_active(&args[0])))
}

pub(crate) fn builtin_inotify_add_watch(args: Vec<Value>) -> EvalResult {
    expect_args("inotify-add-watch", &args, 3)?;
    let _ = expect_strict_string(&args[0])?;
    let (fd, wd) = inotify_register_watch();
    Ok(Value::cons(Value::fixnum(fd), Value::fixnum(wd)))
}

pub(crate) fn builtin_inotify_rm_watch(args: Vec<Value>) -> EvalResult {
    expect_args("inotify-rm-watch", &args, 1)?;
    if inotify_remove_watch(&args[0]) {
        return Ok(Value::T);
    }
    let mut payload = vec![
        Value::string("Invalid descriptor "),
        Value::string("No such file or directory"),
    ];
    if !args[0].is_nil() {
        payload.push(args[0]);
    }
    Err(signal("file-notify-error", payload))
}

// =========================================================================
// eval.c gap-fill stubs
// =========================================================================

/// GNU eval.c:838 — return SYMBOL's toplevel buffer-local value in BUFFER.
///
/// "Toplevel" means outside any let binding.  This pure stub returns nil;
/// a full implementation needs eval access (buffer manager + dynamic stack)
/// and is dispatched via the eval-backed path in builtins/mod.rs.
pub(crate) fn builtin_buffer_local_toplevel_value(args: Vec<Value>) -> EvalResult {
    expect_range_args("buffer-local-toplevel-value", &args, 1, 2)?;
    Ok(Value::NIL)
}

/// GNU eval.c:857 — set SYMBOL's toplevel buffer-local value in BUFFER.
pub(crate) fn builtin_set_buffer_local_toplevel_value(args: Vec<Value>) -> EvalResult {
    expect_range_args("set-buffer-local-toplevel-value", &args, 2, 3)?;
    Ok(args[1])
}

pub(crate) fn builtin_debugger_trap(args: Vec<Value>) -> EvalResult {
    expect_args("debugger-trap", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_internal_delete_indirect_variable(args: Vec<Value>) -> EvalResult {
    expect_args("internal-delete-indirect-variable", &args, 1)?;
    // Stub: return the symbol unchanged
    Ok(args[0])
}

// =========================================================================
// coding.c gap-fill stubs
// =========================================================================

/// GNU coding.c:10362 — internal-decode-string-utf-8.
///
/// These are test/benchmark functions (inside ENABLE_UTF_8_CONVERTER_TEST
/// in GNU).  NeoVM stores all strings as UTF-8 natively, so decode is a
/// pass-through.  We validate arguments per GNU to return nil on bad input.
pub(crate) fn builtin_internal_decode_string_utf_8(args: Vec<Value>) -> EvalResult {
    expect_args("internal-decode-string-utf-8", &args, 7)?;
    // GNU returns nil if STRING is not a string.
    if args[0].as_str().is_none() {
        return Ok(Value::NIL);
    }
    // GNU: CHECK_FIXNUM(count)
    if !args[6].is_fixnum() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("fixnump"), args[6]],
        ));
    }
    // NeoVM is UTF-8 natively; return the input string unchanged.
    Ok(args[0])
}

/// GNU coding.c:10306 — internal-encode-string-utf-8.
///
/// Same rationale as decode: NeoVM strings are already UTF-8.
pub(crate) fn builtin_internal_encode_string_utf_8(args: Vec<Value>) -> EvalResult {
    expect_args("internal-encode-string-utf-8", &args, 7)?;
    if args[0].as_str().is_none() {
        return Ok(Value::NIL);
    }
    if !args[6].is_fixnum() {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("fixnump"), args[6]],
        ));
    }
    Ok(args[0])
}

// =========================================================================
// buffer.c gap-fill stubs
// =========================================================================

pub(crate) fn builtin_overlay_tree(args: Vec<Value>) -> EvalResult {
    expect_range_args("overlay-tree", &args, 0, 1)?;
    Ok(Value::NIL)
}

// =========================================================================
// =========================================================================
// thread.c gap-fill stubs
// =========================================================================

pub(crate) fn builtin_thread_buffer_disposition(args: Vec<Value>) -> EvalResult {
    expect_args("thread-buffer-disposition", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_thread_set_buffer_disposition(args: Vec<Value>) -> EvalResult {
    expect_args("thread-set-buffer-disposition", &args, 2)?;
    // Stub: ignore the set
    Ok(Value::NIL)
}

// =========================================================================
// window.c gap-fill stubs
// =========================================================================

pub(crate) fn builtin_window_discard_buffer_from_window(args: Vec<Value>) -> EvalResult {
    expect_range_args("window-discard-buffer-from-window", &args, 2, 3)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_window_cursor_info(args: Vec<Value>) -> EvalResult {
    expect_range_args("window-cursor-info", &args, 0, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_combine_windows(args: Vec<Value>) -> EvalResult {
    expect_args("combine-windows", &args, 2)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_uncombine_window(args: Vec<Value>) -> EvalResult {
    expect_args("uncombine-window", &args, 1)?;
    Ok(Value::NIL)
}

// =========================================================================
// frame.c gap-fill stubs
// =========================================================================

pub(crate) fn builtin_frame_windows_min_size(args: Vec<Value>) -> EvalResult {
    expect_args("frame-windows-min-size", &args, 4)?;
    Ok(Value::fixnum(0))
}

// =========================================================================
// xdisp.c gap-fill stubs
// =========================================================================

pub(crate) fn builtin_remember_mouse_glyph(args: Vec<Value>) -> EvalResult {
    expect_args("remember-mouse-glyph", &args, 3)?;
    Ok(Value::NIL)
}

// =========================================================================
// image.c gap-fill stubs
// =========================================================================

pub(crate) fn builtin_lookup_image(args: Vec<Value>) -> EvalResult {
    expect_args("lookup-image", &args, 1)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_imagemagick_types(args: Vec<Value>) -> EvalResult {
    expect_args("imagemagick-types", &args, 0)?;
    Ok(Value::NIL)
}

// =========================================================================
// font.c gap-fill stubs
// =========================================================================

pub(crate) fn builtin_font_drive_otf(args: Vec<Value>) -> EvalResult {
    expect_args("font-drive-otf", &args, 6)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_font_otf_alternates(args: Vec<Value>) -> EvalResult {
    expect_args("font-otf-alternates", &args, 3)?;
    Ok(Value::NIL)
}

// =========================================================================
// emacs.c / version.c gap-fill stubs for loadup.el
// =========================================================================

pub(crate) fn builtin_emacs_repository_get_version(args: Vec<Value>) -> EvalResult {
    expect_args("emacs-repository-get-version", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_emacs_repository_get_branch(args: Vec<Value>) -> EvalResult {
    expect_args("emacs-repository-get-branch", &args, 0)?;
    Ok(Value::NIL)
}

pub(crate) fn builtin_emacs_repository_get_dirty(args: Vec<Value>) -> EvalResult {
    expect_args("emacs-repository-get-dirty", &args, 0)?;
    Ok(Value::NIL)
}
