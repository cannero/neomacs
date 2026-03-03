//! Rust Layout Engine FFI Entry Point
//!
//! neomacs_rust_layout_frame, neomacs_layout_charpos_at_pixel,
//! neomacs_layout_window_charpos.

use super::*;

// ============================================================================
// Rust Layout Engine FFI Entry Point
// ============================================================================

/// Global layout engine instance (lazily initialized)
pub(crate) static mut LAYOUT_ENGINE: Option<crate::layout::LayoutEngine> = None;

/// Pending ligatures-enabled flag, set before layout engine is initialized.
/// Applied on first engine creation so init.el settings are not lost.
pub(crate) static mut PENDING_LIGATURES_ENABLED: Option<bool> = None;

/// Pending cosmic-metrics-enabled flag, set before layout engine is initialized.
/// Applied on first engine creation so init.el settings are not lost.
pub(crate) static mut PENDING_COSMIC_METRICS: Option<bool> = None;

/// Called from C when `neomacs-use-rust-display` is enabled.
/// The Rust layout engine reads buffer data via FFI helpers and produces
/// a FrameGlyphBuffer, bypassing the C matrix extraction.
///
/// # Safety
/// Must be called on the Emacs thread. All pointers must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_rust_layout_frame(
    handle: *mut NeomacsDisplay,
    frame_ptr: *mut c_void,
    width: f32,
    height: f32,
    char_width: f32,
    char_height: f32,
    font_pixel_size: f32,
    background: u32,
    vertical_border_fg: u32,
    right_divider_width: i32,
    bottom_divider_width: i32,
    divider_fg: u32,
    divider_first_fg: u32,
    divider_last_fg: u32,
) {
    if handle.is_null() || frame_ptr.is_null() {
        tracing::error!("neomacs_rust_layout_frame: null handle or frame_ptr");
        return;
    }

    // Wrap in catch_unwind to prevent Rust panics from crossing FFI boundary
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let display = &mut *handle;

        // Validate Emacs struct offsets on first call
        crate::layout::emacs_types::ensure_offsets_valid();

        // Initialize layout engine on first call
        if (*std::ptr::addr_of!(LAYOUT_ENGINE)).is_none() {
            let mut engine = crate::layout::LayoutEngine::new();
            // Apply pending ligatures setting from init.el (set before engine existed)
            if let Some(enabled) = *std::ptr::addr_of!(PENDING_LIGATURES_ENABLED) {
                engine.ligatures_enabled = enabled;
                tracing::info!("Applied pending ligatures_enabled={}", enabled);
            }
            // Apply pending cosmic metrics setting from init.el
            if let Some(enabled) = *std::ptr::addr_of!(PENDING_COSMIC_METRICS) {
                engine.use_cosmic_metrics = enabled;
                tracing::info!("Applied pending use_cosmic_metrics={}", enabled);
            }
            *std::ptr::addr_of_mut!(LAYOUT_ENGINE) = Some(engine);
            tracing::info!("Rust layout engine initialized");
        }

        let engine = match (*std::ptr::addr_of_mut!(LAYOUT_ENGINE)).as_mut() {
            Some(e) => e,
            None => {
                tracing::error!("Rust layout engine initialization failed");
                return;
            }
        };
        let frame_params = crate::layout::FrameParams {
            width,
            height,
            char_width: if char_width > 0.0 { char_width } else { 8.0 },
            char_height: if char_height > 0.0 { char_height } else { 16.0 },
            font_pixel_size: if font_pixel_size > 0.0 {
                font_pixel_size
            } else {
                14.0
            },
            background,
            vertical_border_fg,
            right_divider_width,
            bottom_divider_width,
            divider_fg,
            divider_first_fg,
            divider_last_fg,
        };

        // Rust layout path emits transition hints directly from the layout
        // engine; clear C-side hint tracking to avoid stale topology state.
        display.transition_prev_window_infos.clear();
        display.transition_curr_window_infos.clear();

        engine.layout_frame(frame_ptr, &frame_params, &mut display.frame_glyphs);
    }));

    if let Err(e) = result {
        let msg = if let Some(s) = e.downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = e.downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic".to_string()
        };
        tracing::error!("PANIC in neomacs_rust_layout_frame: {}", msg);
    }
}

// ============================================================================
// NeoVM-Core Layout FFI Entry Point (Phase 2)
// ============================================================================

/// Layout a frame using neovm-core data (Rust-authoritative path).
///
/// Called when the neovm-core backend is active. Reads buffer text,
/// window geometry, and display parameters directly from the Rust
/// Evaluator's state instead of C Emacs structures.
///
/// Returns 0 on success, -1 on error.
///
/// # Safety
/// Must be called on the Emacs main thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_rust_layout_frame_neovm() -> c_int {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Get the evaluator (mutable for fontification pass)
        let evaluator = match super::eval_bridge::get_evaluator_mut() {
            Some(e) => e,
            None => {
                tracing::error!("neomacs_rust_layout_frame_neovm: evaluator not initialized");
                return -1;
            }
        };

        // Get the selected frame ID from the evaluator
        let frame_id = match evaluator.frame_manager().selected_frame() {
            Some(f) => f.id,
            None => {
                tracing::error!("neomacs_rust_layout_frame_neovm: no selected frame");
                return -1;
            }
        };

        // Get the display handle from THREADED_STATE
        let display_handle = match (*std::ptr::addr_of!(super::THREADED_STATE)).as_ref() {
            Some(state) => state.display_handle,
            None => {
                tracing::error!("neomacs_rust_layout_frame_neovm: threaded state not initialized");
                return -1;
            }
        };

        if display_handle.is_null() {
            tracing::error!("neomacs_rust_layout_frame_neovm: null display handle");
            return -1;
        }

        let display = &mut *display_handle;

        // Initialize layout engine on first call (same as existing path)
        if (*std::ptr::addr_of!(LAYOUT_ENGINE)).is_none() {
            let mut engine = crate::layout::LayoutEngine::new();
            if let Some(enabled) = *std::ptr::addr_of!(PENDING_LIGATURES_ENABLED) {
                engine.ligatures_enabled = enabled;
            }
            if let Some(enabled) = *std::ptr::addr_of!(PENDING_COSMIC_METRICS) {
                engine.use_cosmic_metrics = enabled;
            }
            *std::ptr::addr_of_mut!(LAYOUT_ENGINE) = Some(engine);
            tracing::info!("Rust layout engine initialized (neovm path)");
        }

        let engine = match (*std::ptr::addr_of_mut!(LAYOUT_ENGINE)).as_mut() {
            Some(e) => e,
            None => {
                tracing::error!(
                    "neomacs_rust_layout_frame_neovm: layout engine initialization failed"
                );
                return -1;
            }
        };

        // Rust layout path emits transition hints directly from the layout
        // engine; clear C-side hint tracking to avoid stale topology state.
        display.transition_prev_window_infos.clear();
        display.transition_curr_window_infos.clear();

        // Run layout using neovm-core data
        engine.layout_frame_rust(evaluator, frame_id, &mut display.frame_glyphs);

        // Send the frame to the render thread
        if let Some(state) = (*std::ptr::addr_of!(super::THREADED_STATE)).as_ref() {
            let frame = display.frame_glyphs.clone();
            let _ = state.emacs_comms.frame_tx.try_send(frame);
            let n_glyphs = display.frame_glyphs.glyphs.len();
            tracing::debug!(
                "neomacs_rust_layout_frame_neovm: sent frame for {:?} ({} glyphs)",
                frame_id,
                n_glyphs
            );
        }

        0
    }));

    match result {
        Ok(code) => code,
        Err(e) => {
            let msg = if let Some(s) = e.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = e.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic".to_string()
            };
            tracing::error!("PANIC in neomacs_rust_layout_frame_neovm: {}", msg);
            -1
        }
    }
}

/// Query buffer character position at given frame-relative pixel coordinates.
/// Used by mouse interaction (note_mouse_highlight, mouse clicks).
/// Returns charpos, or -1 if not found.
///
/// # Safety
/// Must be called on the Emacs thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_layout_charpos_at_pixel(px: f32, py: f32) -> i64 {
    crate::layout::hit_test_charpos_at_pixel(px, py)
}

/// Query buffer character position for a specific window at
/// window-relative pixel coordinates.
/// Returns charpos, or -1 if not found.
///
/// # Safety
/// Must be called on the Emacs thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_layout_window_charpos(window_id: i64, wx: f32, wy: f32) -> i64 {
    crate::layout::hit_test_window_charpos(window_id, wx, wy)
}

// Note: Event Polling FFI Functions have been removed
// Events are now delivered via the threaded mode wakeup mechanism
// Use neomacs_display_drain_input() instead

/// Set the font metrics backend for the layout engine.
/// backend: 0 = Emacs C (default), 1 = cosmic-text
///
/// When set to cosmic-text, the layout engine uses the same font resolution
/// as the render thread, eliminating width mismatches between layout and rendering.
///
/// # Safety
/// Must be called on the Emacs thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn neomacs_display_set_font_backend(
    _handle: *mut NeomacsDisplay,
    backend: c_int,
) {
    let use_cosmic = backend != 0;

    // Set on the layout engine if already initialized
    if let Some(ref mut engine) = *std::ptr::addr_of_mut!(LAYOUT_ENGINE) {
        engine.use_cosmic_metrics = use_cosmic;
        tracing::info!(
            "Font metrics backend set to {}",
            if use_cosmic { "cosmic-text" } else { "emacs-c" }
        );
    }
    // Always store pending so engine init picks it up even if set before creation
    *std::ptr::addr_of_mut!(PENDING_COSMIC_METRICS) = Some(use_cosmic);
}
