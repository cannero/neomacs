//! Core types and data structures for the display engine.

pub mod animation;
pub mod animation_config;
pub mod buffer_transition;
pub mod casefiddle;
pub mod casetab;
pub mod category;
pub mod char_utils;
pub mod chartab;
pub mod composite;
pub mod cursor_animation;
pub mod error;
pub mod gap_buffer;
pub mod itree;
pub mod marker;
pub mod profiler;
pub mod regex;
pub mod region_cache;
pub mod search;
pub mod syntax_table;
pub mod textprop;
pub mod undo;

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
