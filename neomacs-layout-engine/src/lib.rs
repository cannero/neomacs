//! Rust Display Layout Engine.
//!
//! Replaces the C display engine (xdisp.c) for computing glyph layout.
//! Reads buffer data via FFI and produces `FrameGlyphBuffer` for the renderer.

#![allow(unsafe_op_in_unsafe_fn)] // FFI-heavy layout code; migrate to explicit blocks incrementally.

pub mod bidi;
pub mod bidi_layout;
pub mod emacs_ffi;
pub mod emacs_types;
pub mod engine;
pub mod font_loader;
pub mod font_match;
pub mod font_metrics;
pub mod hit_test;
pub mod neovm_bridge;
pub mod status_line;
pub mod types;
pub mod unicode;

pub use engine::*;
pub use hit_test::{hit_test_charpos_at_pixel, hit_test_window_charpos};
pub use types::*;

#[cfg(test)]
mod test_ffi_stubs {
    use std::ffi::{c_int, c_void};

    #[unsafe(no_mangle)]
    extern "C" fn neomacs_layout_get_stipple_bitmap(
        _frame: *mut c_void,
        _bitmap_id: c_int,
        _bits_out: *mut u8,
        _bits_buf_len: c_int,
        _width_out: *mut c_int,
        _height_out: *mut c_int,
    ) -> c_int {
        -1
    }

    #[unsafe(no_mangle)]
    extern "C" fn neomacs_layout_char_width(
        _window: *mut c_void,
        _charcode: c_int,
        _face_id: c_int,
    ) -> f32 {
        -1.0
    }

    #[unsafe(no_mangle)]
    extern "C" fn neomacs_layout_fill_ascii_widths(
        _window: *mut c_void,
        _face_id: c_int,
        widths: *mut f32,
    ) {
        if widths.is_null() {
            return;
        }

        unsafe {
            for idx in 0..128 {
                *widths.add(idx) = 0.0;
            }
        }
    }
}
