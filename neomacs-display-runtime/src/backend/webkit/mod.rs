//! WebKit browser embedding for Neomacs.
//!
//! This module provides WebKit browser views that can be embedded
//! directly into Emacs buffers using the WPE WebKit backend.
//!
//! Note: This module now uses WPE WebKit (headless rendering with EGL export)
//! instead of WebKitGTK (which requires GTK widget tree).

mod cache;
mod view;

pub use cache::WebKitCache;
pub use view::WebKitView;
