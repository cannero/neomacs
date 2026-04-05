use crate::root::Gc;

/// High-level barrier category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BarrierKind {
    /// Post-write barrier for old-to-young tracking.
    PostWrite,
    /// Pre-write SATB barrier for concurrent marking.
    SatbPreWrite,
}

/// Remembered-set edge from one object to another.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RememberedEdge {
    /// Source object.
    pub owner: Gc<()>,
    /// Destination object.
    pub target: Gc<()>,
}

/// Recorded barrier event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BarrierEvent {
    /// Barrier kind.
    pub kind: BarrierKind,
    /// Object being mutated.
    pub owner: Gc<()>,
    /// Slot index when known.
    pub slot: Option<usize>,
    /// Previous value for SATB.
    pub old_value: Option<Gc<()>>,
    /// New value for remembered-set tracking.
    pub new_value: Option<Gc<()>>,
}
