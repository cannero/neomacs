//! Space-specific heap configuration and metadata.

/// Large-object space configuration and policy.
pub mod large;
/// Nursery (young-generation) configuration and policy.
pub mod nursery;
pub(crate) mod nursery_arena;
/// Old-generation configuration, region/block layout, and the
/// physical-compaction policy knob.
pub mod old;
/// Pinned-space configuration for objects that must not move.
pub mod pinned;

pub use large::LargeObjectSpaceConfig;
pub use nursery::NurseryConfig;
pub(crate) use nursery_arena::NurseryState;
pub use old::OldGenConfig;
pub(crate) use old::{
    OldBlock, OldGenPlanSelection, OldGenState, OldRegionCollectionStats, PreparedOldGenReclaim,
};
pub use pinned::PinnedSpaceConfig;
