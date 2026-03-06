//! Emacs-compatible regex engine.
//!
//! Implements the Emacs regex syntax including:
//! - `\(` `\)` groups, `\|` alternation, `\{n,m\}` repetition
//! - `\b` `\B` word boundaries, `\<` `\>` word start/end
//! - `\s` `\S` syntax classes, `\c` `\C` categories
//! - `[...]` character sets with named classes `[:alpha:]` etc.
//! - `.` `^` `$` `*` `+` `?` with greedy and non-greedy variants
//! - Backreferences `\1` through `\9`
//! - Case-folding via the `CharProperties` trait

pub mod compile;
pub mod engine;
pub mod types;

pub use compile::compile;
pub use engine::{match_pattern, search};
pub use types::*;
