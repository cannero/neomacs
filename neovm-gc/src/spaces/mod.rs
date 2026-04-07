//! Space-specific heap configuration and metadata.

pub mod large;
pub mod nursery;
pub(crate) mod nursery_arena;
pub mod old;
pub mod pinned;

pub use large::LargeObjectSpaceConfig;
pub use nursery::NurseryConfig;
pub(crate) use nursery_arena::NurseryState;
pub use old::OldGenConfig;
pub(crate) use old::{
    OldGenPlanSelection, OldGenState, OldRegionCollectionStats, PreparedOldGenReclaim,
};
pub use pinned::PinnedSpaceConfig;
