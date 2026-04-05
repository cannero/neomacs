/// Collection statistics for one completed GC cycle.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CollectionStats {
    /// Number of collections that have completed.
    pub collections: u64,
    /// Number of nursery collections.
    pub minor_collections: u64,
    /// Number of old-generation collections.
    pub major_collections: u64,
    /// Bytes promoted from nursery to old generation.
    pub promoted_bytes: u64,
    /// Number of mark slices drained across completed GC cycles.
    pub mark_steps: u64,
    /// Number of mark worker rounds drained across completed GC cycles.
    pub mark_rounds: u64,
    /// Bytes reclaimed across completed GC cycles.
    pub reclaimed_bytes: u64,
    /// Number of finalized objects across completed GC cycles.
    pub finalized_objects: u64,
    /// Number of old-generation regions compacted across completed GC cycles.
    pub compacted_regions: u64,
    /// Number of old-generation regions reclaimed across completed GC cycles.
    pub reclaimed_regions: u64,
}

/// Per-space storage statistics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SpaceStats {
    /// Bytes reserved by the space.
    pub reserved_bytes: usize,
    /// Bytes currently live in the space.
    pub live_bytes: usize,
}

/// Public snapshot of one logical old-generation region.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OldRegionStats {
    /// Region index in allocation order.
    pub region_index: usize,
    /// Bytes reserved for this region.
    pub reserved_bytes: usize,
    /// Bytes currently consumed by the region allocation cursor.
    pub used_bytes: usize,
    /// Bytes currently live in this region.
    pub live_bytes: usize,
    /// Reclaimable bytes in this region.
    pub free_bytes: usize,
    /// Bytes lost to interior holes between live objects.
    pub hole_bytes: usize,
    /// Unused bytes still available at the end of the region.
    pub tail_bytes: usize,
    /// Number of live objects assigned to this region.
    pub object_count: usize,
    /// Number of occupied lines containing live objects.
    pub occupied_lines: usize,
}

/// Heap-wide runtime statistics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeapStats {
    /// Nursery statistics.
    pub nursery: SpaceStats,
    /// Old-generation statistics.
    pub old: SpaceStats,
    /// Pinned-space statistics.
    pub pinned: SpaceStats,
    /// Large-object-space statistics.
    pub large: SpaceStats,
    /// Collection counters.
    pub collections: CollectionStats,
}
