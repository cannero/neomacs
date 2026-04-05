/// Old-generation configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OldGenConfig {
    /// Region size in bytes.
    pub region_bytes: usize,
    /// Line size in bytes for occupancy tracking.
    pub line_bytes: usize,
    /// Maximum number of old regions to target in one planned compaction cycle.
    pub compaction_candidate_limit: usize,
    /// Minimum reclaimable bytes required for a region to become a compaction candidate.
    pub selective_reclaim_threshold_bytes: usize,
    /// Maximum live bytes selected for compaction in one planned cycle.
    pub max_compaction_bytes_per_cycle: usize,
    /// Maximum number of concurrent mark workers.
    pub concurrent_mark_workers: usize,
    /// Number of major-mark slices one mutator operation should assist.
    pub mutator_assist_slices: usize,
}

impl Default for OldGenConfig {
    fn default() -> Self {
        Self {
            region_bytes: 4 * 1024 * 1024,
            line_bytes: 256,
            compaction_candidate_limit: 8,
            selective_reclaim_threshold_bytes: 1,
            max_compaction_bytes_per_cycle: usize::MAX,
            concurrent_mark_workers: 1,
            mutator_assist_slices: 1,
        }
    }
}
