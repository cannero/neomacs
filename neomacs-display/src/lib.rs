//! Neomacs Display C Bridge
//!
//! This crate exposes the legacy C ABI surface for the display engine while
//! delegating runtime behavior to `neomacs-display-runtime`.

#![allow(unused)]
#![allow(unsafe_op_in_unsafe_fn)]

#[cfg(all(feature = "core-backend-emacs-c", feature = "core-backend-rust"))]
compile_error!("features `core-backend-emacs-c` and `core-backend-rust` are mutually exclusive");

pub use neomacs_display_runtime::*;

pub mod ffi;
