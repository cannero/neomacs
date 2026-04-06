use std::collections::HashMap;

use crate::collector_exec::ForwardingRelocator;
use crate::descriptor::{MovePolicy, Relocator};
use crate::heap::AllocError;
use crate::index_state::{ForwardingMap, HeapIndexState};
use crate::object::{ObjectRecord, SpaceKind};
use crate::root::RootStack;
use crate::spaces::{OldGenConfig, OldGenState};
use crate::stats::HeapStats;

/// Nursery-space configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NurseryConfig {
    /// Bytes reserved for each nursery semispace.
    pub semispace_bytes: usize,
    /// Maximum object size allowed in nursery allocation.
    pub max_regular_object_bytes: usize,
    /// Survivor age at which nursery objects are promoted into old generation.
    pub promotion_age: u8,
    /// Number of worker threads to use for stop-the-world nursery tracing.
    pub parallel_minor_workers: usize,
}

impl Default for NurseryConfig {
    fn default() -> Self {
        Self {
            semispace_bytes: 16 * 1024 * 1024,
            max_regular_object_bytes: 64 * 1024,
            promotion_age: 2,
            parallel_minor_workers: 1,
        }
    }
}

#[derive(Debug)]
pub(crate) struct EvacuationOutcome {
    pub(crate) forwarding: ForwardingMap,
    pub(crate) promoted_bytes: usize,
}

pub(crate) fn target_space_for_survivor(
    move_policy: MovePolicy,
    current_age: u8,
    promotion_age: u8,
) -> SpaceKind {
    let next_age = current_age.saturating_add(1);
    if next_age < promotion_age {
        return SpaceKind::Nursery;
    }

    match move_policy {
        MovePolicy::PromoteToPinned => SpaceKind::Pinned,
        _ => SpaceKind::Old,
    }
}

pub(crate) fn evacuate_marked_nursery(
    objects: &mut Vec<ObjectRecord>,
    indexes: &mut HeapIndexState,
    old_gen: &mut OldGenState,
    old_config: &OldGenConfig,
    nursery_config: &NurseryConfig,
    stats: &mut HeapStats,
) -> Result<EvacuationOutcome, AllocError> {
    let mut forwarding = HashMap::new();
    let mut evacuated: Vec<(ObjectRecord, SpaceKind)> = Vec::new();
    let mut promoted_bytes = 0usize;

    for object in objects.iter() {
        if object.space() == SpaceKind::Nursery && object.is_marked() {
            let target_space = target_space_for_survivor(
                object.header().desc().move_policy,
                object.header().age(),
                nursery_config.promotion_age,
            );
            let new_record = object.evacuate_to_space(target_space)?;
            new_record.set_marked(true);
            forwarding.insert(object.object_key(), new_record.erased());
            evacuated.push((new_record, target_space));
        }
    }

    let mut records = Vec::with_capacity(evacuated.len());
    for (mut new_record, target_space) in evacuated {
        if target_space == SpaceKind::Old {
            let placement = old_gen.allocate_placement(old_config, new_record.total_size());
            new_record.set_old_region_placement(placement);
            old_gen.record_object(&new_record);
            stats.old.reserved_bytes = old_gen.reserved_bytes();
            promoted_bytes = promoted_bytes.saturating_add(new_record.total_size());
        }
        records.push(new_record);
    }

    let start = objects.len();
    objects.extend(records);
    for index in start..objects.len() {
        let object_key = objects[index].object_key();
        indexes.object_index.insert(object_key, index);
        let desc = objects[index].header().desc();
        indexes.record_descriptor_candidates(object_key, desc);
    }

    Ok(EvacuationOutcome {
        forwarding,
        promoted_bytes,
    })
}

pub(crate) fn relocate_roots_and_edges(
    roots: &mut RootStack,
    objects: &[ObjectRecord],
    indexes: &mut HeapIndexState,
    forwarding: &ForwardingMap,
) {
    if forwarding.is_empty() {
        return;
    }

    let mut relocator = ForwardingRelocator::new(forwarding);
    roots.relocate_all(&mut relocator);

    for object in objects {
        let copied_nursery_survivor = object.space() == SpaceKind::Nursery
            && object.is_marked()
            && !object.header().is_moved_out();
        if object.space() != SpaceKind::Nursery || copied_nursery_survivor {
            object.relocate_edges(&mut relocator);
        }
    }

    for edge in &mut indexes.remembered.edges {
        edge.owner =
            unsafe { crate::root::Gc::from_erased(relocator.relocate_erased(edge.owner.erase())) };
        edge.target =
            unsafe { crate::root::Gc::from_erased(relocator.relocate_erased(edge.target.erase())) };
    }
}

#[cfg(test)]
#[path = "nursery_test.rs"]
mod tests;
