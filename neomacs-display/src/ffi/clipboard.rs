//! Clipboard and Primary Selection FFI functions

use super::*;

// ============================================================================
// Clipboard
// ============================================================================

/// Set clipboard text.  The text is a UTF-8 C string.
/// Returns 0 on success, -1 on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_clipboard_set_text(text: *const c_char) -> c_int {
    if text.is_null() {
        return -1;
    }
    let c_str = match CStr::from_ptr(text).to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    match arboard::Clipboard::new() {
        Ok(mut clipboard) => match clipboard.set_text(c_str) {
            Ok(()) => 0,
            Err(e) => {
                tracing::warn!("Clipboard set failed: {}", e);
                -1
            }
        },
        Err(e) => {
            tracing::warn!("Clipboard open failed: {}", e);
            -1
        }
    }
}

/// Get clipboard text.  Returns a newly allocated UTF-8 C string
/// that the caller must free with neomacs_clipboard_free_text(),
/// or NULL if the clipboard is empty or an error occurred.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_clipboard_get_text() -> *mut c_char {
    match arboard::Clipboard::new() {
        Ok(mut clipboard) => match clipboard.get_text() {
            Ok(text) => match CString::new(text) {
                Ok(c_string) => c_string.into_raw(),
                Err(_) => ptr::null_mut(),
            },
            Err(_) => ptr::null_mut(),
        },
        Err(e) => {
            tracing::warn!("Clipboard open failed: {}", e);
            ptr::null_mut()
        }
    }
}

/// Free a string returned by neomacs_clipboard_get_text().
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_clipboard_free_text(text: *mut c_char) {
    if !text.is_null() {
        drop(CString::from_raw(text));
    }
}

// ============================================================================
// Primary Selection (X11/Wayland)
// ============================================================================

/// Set primary selection text.  The text is a UTF-8 C string.
/// Returns 0 on success, -1 on failure.
#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_primary_selection_set_text(text: *const c_char) -> c_int {
    use arboard::{LinuxClipboardKind, SetExtLinux};
    if text.is_null() {
        return -1;
    }
    let c_str = match CStr::from_ptr(text).to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    match arboard::Clipboard::new() {
        Ok(mut clipboard) => {
            match clipboard
                .set()
                .clipboard(LinuxClipboardKind::Primary)
                .text(c_str.to_owned())
            {
                Ok(()) => 0,
                Err(e) => {
                    tracing::warn!("Primary selection set failed: {}", e);
                    -1
                }
            }
        }
        Err(e) => {
            tracing::warn!("Clipboard open failed: {}", e);
            -1
        }
    }
}

#[cfg(not(target_os = "linux"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_primary_selection_set_text(_text: *const c_char) -> c_int {
    -1
}

/// Get primary selection text.  Returns a newly allocated UTF-8 C string
/// that the caller must free with neomacs_clipboard_free_text(),
/// or NULL if the selection is empty or an error occurred.
#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_primary_selection_get_text() -> *mut c_char {
    use arboard::{GetExtLinux, LinuxClipboardKind};
    match arboard::Clipboard::new() {
        Ok(mut clipboard) => {
            match clipboard
                .get()
                .clipboard(LinuxClipboardKind::Primary)
                .text()
            {
                Ok(text) => match CString::new(text) {
                    Ok(c_string) => c_string.into_raw(),
                    Err(_) => ptr::null_mut(),
                },
                Err(_) => ptr::null_mut(),
            }
        }
        Err(e) => {
            tracing::warn!("Clipboard open failed: {}", e);
            ptr::null_mut()
        }
    }
}

#[cfg(not(target_os = "linux"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_primary_selection_get_text() -> *mut c_char {
    ptr::null_mut()
}
