//! Shared heap payload types used by both the tagged runtime and legacy dump code.
//!
//! These types are not specific to the old `gc::heap` implementation. Keeping
//! them behind a neutral module boundary lets the tagged runtime depend on them
//! without importing the legacy GC namespace.

pub use crate::gc::types::{LispString, MarkerData, OverlayData};
