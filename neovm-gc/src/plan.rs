use crate::stats::CollectionStats;

/// High-level collection kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionKind {
    /// Nursery-only collection.
    Minor,
    /// Old-generation collection.
    Major,
    /// Whole-heap collection.
    Full,
}

/// Major-collection phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionPhase {
    /// No collection is in progress.
    Idle,
    /// Initial root capture.
    InitialMark,
    /// Concurrent marking.
    ConcurrentMark,
    /// Stop-the-world remark.
    Remark,
    /// Evacuation or compaction.
    Evacuate,
    /// Reclamation and cleanup.
    Reclaim,
}

/// Scheduler-visible collection plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectionPlan {
    /// Requested collection kind.
    pub kind: CollectionKind,
    /// Current phase.
    pub phase: CollectionPhase,
    /// Whether collector workers may run concurrently with mutators.
    pub concurrent: bool,
    /// Whether the collector may use multiple workers.
    pub parallel: bool,
    /// Number of collector workers planned for this cycle.
    pub worker_count: usize,
    /// Maximum number of objects to drain from one major-mark slice.
    pub mark_slice_budget: usize,
    /// Number of old regions implicated by this plan.
    pub target_old_regions: usize,
    /// Exact old-region indices selected for compaction or evacuation by this plan.
    pub selected_old_regions: Vec<usize>,
    /// Estimated live bytes that would be moved by the selected old-region compaction set.
    pub estimated_compaction_bytes: usize,
    /// Estimated bytes the plan may reclaim or compact.
    pub estimated_reclaim_bytes: usize,
}

/// Progress snapshot for one externally advanced major-mark session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MajorMarkProgress {
    /// Whether the major-mark worklist is fully drained.
    pub completed: bool,
    /// Number of objects drained in the most recent slice.
    pub drained_objects: usize,
    /// Total mark slices executed for this session so far.
    pub mark_steps: u64,
    /// Remaining pending objects in the mark worklist.
    pub remaining_work: usize,
}

/// One background collection service round.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackgroundCollectionStatus {
    /// No background-managed collection is currently active.
    Idle,
    /// Background marking made progress but has not finished.
    Progress(MajorMarkProgress),
    /// Background marking is complete and the collection is ready for the final stop-the-world
    /// finish phase.
    ReadyToFinish(MajorMarkProgress),
    /// Background service finished the active major collection.
    Finished(CollectionStats),
}

impl Default for CollectionPlan {
    fn default() -> Self {
        Self {
            kind: CollectionKind::Minor,
            phase: CollectionPhase::Idle,
            concurrent: false,
            parallel: true,
            worker_count: 1,
            mark_slice_budget: 0,
            target_old_regions: 0,
            selected_old_regions: Vec::new(),
            estimated_compaction_bytes: 0,
            estimated_reclaim_bytes: 0,
        }
    }
}
