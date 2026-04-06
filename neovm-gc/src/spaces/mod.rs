//! Space-specific heap configuration and metadata.

pub mod large;
pub mod nursery;
pub mod old;
pub mod pinned;

pub use large::LargeObjectSpaceConfig;
pub use nursery::NurseryConfig;
pub use old::OldGenConfig;
pub(crate) use old::{
    OldGenState, OldRegion, OldRegionCollectionStats, compare_compaction_candidate_priority,
};
pub use pinned::PinnedSpaceConfig;
