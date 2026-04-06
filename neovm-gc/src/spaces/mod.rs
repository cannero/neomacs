//! Space-specific heap configuration and metadata.

pub mod large;
pub mod nursery;
pub mod old;
pub mod pinned;

pub use large::LargeObjectSpaceConfig;
pub use nursery::NurseryConfig;
pub use old::OldGenConfig;
pub(crate) use old::{OldGenPlanSelection, OldGenState, OldRegion, OldRegionCollectionStats};
pub use pinned::PinnedSpaceConfig;
