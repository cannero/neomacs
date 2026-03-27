//! Core types and data structures for the display engine.

pub mod animation;
pub mod animation_config;
pub mod buffer_transition;
pub mod cursor_animation;
pub mod error;
pub mod profiler;

pub use neomacs_display_protocol::{face, frame_glyphs, scene, types};
pub use neomacs_layout_engine::{bidi, font_loader};

pub use animation::*;
pub use animation_config::*;
pub use buffer_transition::*;
pub use cursor_animation::*;
pub use error::*;
pub use face::*;
pub use frame_glyphs::*;
pub use scene::*;
pub use types::*;
