use std::collections::{HashMap, HashSet};

use crate::barrier::RememberedEdge;
use crate::descriptor::{GcErased, ObjectKey, TypeDesc, TypeFlags};
use crate::object::{ObjectRecord, SpaceKind};
use crate::plan::CollectionKind;
use crate::reclaim::PreparedReclaimSurvivor;
use crate::stats::HeapStats;

pub(crate) type ObjectIndex = HashMap<ObjectKey, usize>;
pub(crate) type ForwardingMap = HashMap<ObjectKey, GcErased>;

#[derive(Debug, Default)]
pub(crate) struct RememberedSetState {
    pub(crate) edges: Vec<RememberedEdge>,
    pub(crate) owners: Vec<ObjectKey>,
    owner_set: HashSet<ObjectKey>,
}

#[derive(Debug, Default)]
pub(crate) struct HeapIndexState {
    pub(crate) object_index: ObjectIndex,
    pub(crate) finalizable_candidates: Vec<ObjectKey>,
    pub(crate) weak_candidates: Vec<ObjectKey>,
    pub(crate) ephemeron_candidates: Vec<ObjectKey>,
    pub(crate) remembered: RememberedSetState,
}

#[derive(Debug, Default)]
pub(crate) struct PreparedIndexReclaim {
    pub(crate) rebuilt_object_index: ObjectIndex,
    pub(crate) finalize_indices: Vec<usize>,
    pub(crate) finalizable_candidates: Vec<ObjectKey>,
    pub(crate) weak_candidates: Vec<ObjectKey>,
    pub(crate) ephemeron_candidates: Vec<ObjectKey>,
    pub(crate) remembered_edges: Vec<RememberedEdge>,
    pub(crate) remembered_owners: Vec<ObjectKey>,
}

#[derive(Debug, Default)]
pub(crate) struct PostSweepIndexRebuild {
    finalizable_candidates: HashSet<ObjectKey>,
}

impl RememberedSetState {
    pub(crate) fn record_edge(&mut self, owner: GcErased, target: GcErased) {
        let owner_key = owner.object_key();
        self.edges.push(RememberedEdge {
            owner: unsafe { crate::root::Gc::from_erased(owner) },
            target: unsafe { crate::root::Gc::from_erased(target) },
        });
        if self.owner_set.insert(owner_key) {
            self.owners.push(owner_key);
        }
    }

    pub(crate) fn rebuild_owners(&mut self) {
        self.owner_set.clear();
        self.owners.clear();
        for edge in &self.edges {
            let owner = edge.owner.erase().object_key();
            if self.owner_set.insert(owner) {
                self.owners.push(owner);
            }
        }
    }

    pub(crate) fn edges_for_collection(
        &self,
        objects: &[ObjectRecord],
        object_index: &ObjectIndex,
        kind: CollectionKind,
    ) -> (Vec<RememberedEdge>, Vec<ObjectKey>) {
        let remembered_edges: Vec<_> = self
            .edges
            .iter()
            .copied()
            .filter(|edge| {
                let Some(&owner_index) = object_index.get(&edge.owner.erase().object_key()) else {
                    return false;
                };
                let Some(&target_index) = object_index.get(&edge.target.erase().object_key())
                else {
                    return false;
                };
                let owner = &objects[owner_index];
                let target = &objects[target_index];
                keep_object_for_collection(kind, owner)
                    && owner.space() != SpaceKind::Nursery
                    && owner.space() != SpaceKind::Immortal
                    && keep_object_for_collection(kind, target)
                    && target.space() == SpaceKind::Nursery
            })
            .collect();

        let mut remembered_owners = Vec::new();
        let mut remembered_owner_set = HashSet::new();
        for edge in &remembered_edges {
            let owner = edge.owner.erase().object_key();
            if remembered_owner_set.insert(owner) {
                remembered_owners.push(owner);
            }
        }
        (remembered_edges, remembered_owners)
    }

    pub(crate) fn retain_for_post_sweep_objects(
        &mut self,
        objects: &[ObjectRecord],
        object_index: &ObjectIndex,
    ) {
        self.edges.retain(|edge| {
            let owner = edge.owner.erase().object_key();
            let target = edge.target.erase().object_key();
            let owner_space = object_index
                .get(&owner)
                .map(|&index| objects[index].space());
            let target_space = object_index
                .get(&target)
                .map(|&index| objects[index].space());
            owner_space
                .is_some_and(|space| space != SpaceKind::Nursery && space != SpaceKind::Immortal)
                && target_space == Some(SpaceKind::Nursery)
        });
        self.rebuild_owners();
    }

    pub(crate) fn replace(&mut self, edges: Vec<RememberedEdge>, owners: Vec<ObjectKey>) {
        self.edges = edges;
        self.owners = owners;
        self.owner_set = self.owners.iter().copied().collect();
    }
}

impl HeapIndexState {
    pub(crate) fn apply_storage_stats(&self, stats: &mut HeapStats) {
        stats.remembered_edges = self.remembered.edges.len();
        stats.remembered_owners = self.remembered.owners.len();
        stats.finalizable_candidates = self.finalizable_candidates.len();
        stats.weak_candidates = self.weak_candidates.len();
        stats.ephemeron_candidates = self.ephemeron_candidates.len();
    }

    pub(crate) fn record_allocated_object(
        &mut self,
        object_key: ObjectKey,
        index: usize,
        desc: &'static TypeDesc,
    ) {
        self.object_index.insert(object_key, index);
        self.record_descriptor_candidates(object_key, desc);
    }

    pub(crate) fn record_descriptor_candidates(
        &mut self,
        object_key: ObjectKey,
        desc: &'static TypeDesc,
    ) {
        if desc.flags.contains(TypeFlags::FINALIZABLE) {
            self.finalizable_candidates.push(object_key);
        }
        if desc.flags.contains(TypeFlags::WEAK) {
            self.weak_candidates.push(object_key);
        }
        if desc.flags.contains(TypeFlags::EPHEMERON_KEY) {
            self.ephemeron_candidates.push(object_key);
        }
    }

    pub(crate) fn candidate_indices(&self, candidates: &[ObjectKey]) -> Vec<usize> {
        candidates
            .iter()
            .filter_map(|key| self.object_index.get(key).copied())
            .collect()
    }

    pub(crate) fn record_remembered_edge(&mut self, owner: GcErased, target: GcErased) {
        self.remembered.record_edge(owner, target);
    }

    pub(crate) fn reset_candidate_indexes(&mut self, capacity: usize) {
        self.object_index.clear();
        self.object_index.reserve(capacity);
        self.finalizable_candidates.clear();
        self.weak_candidates.clear();
        self.ephemeron_candidates.clear();
        self.finalizable_candidates.reserve(capacity);
        self.weak_candidates.reserve(capacity);
        self.ephemeron_candidates.reserve(capacity);
    }

    pub(crate) fn begin_post_sweep_rebuild(&mut self, capacity: usize) -> PostSweepIndexRebuild {
        let rebuild = PostSweepIndexRebuild {
            finalizable_candidates: self.finalizable_candidates.iter().copied().collect(),
        };
        self.reset_candidate_indexes(capacity);
        rebuild
    }

    pub(crate) fn remembered_edges_for_collection(
        &self,
        objects: &[ObjectRecord],
        kind: CollectionKind,
    ) -> (Vec<RememberedEdge>, Vec<ObjectKey>) {
        self.remembered
            .edges_for_collection(objects, &self.object_index, kind)
    }

    pub(crate) fn prepare_reclaim_state(
        &self,
        objects: &[ObjectRecord],
        survivors: &[PreparedReclaimSurvivor],
        kind: CollectionKind,
    ) -> PreparedIndexReclaim {
        let finalizable_candidate_set: HashSet<_> =
            self.finalizable_candidates.iter().copied().collect();
        let weak_candidate_set: HashSet<_> = self.weak_candidates.iter().copied().collect();
        let ephemeron_candidate_set: HashSet<_> =
            self.ephemeron_candidates.iter().copied().collect();

        let mut rebuilt_object_index = HashMap::with_capacity(survivors.len());
        let mut survivor_keys = HashSet::with_capacity(survivors.len());
        let mut finalizable_candidates = Vec::new();
        let mut weak_candidates = Vec::new();
        let mut ephemeron_candidates = Vec::new();
        for (rebuilt_index, survivor) in survivors.iter().enumerate() {
            let object_key = objects[survivor.object_index].object_key();
            rebuilt_object_index.insert(object_key, rebuilt_index);
            survivor_keys.insert(object_key);
            if finalizable_candidate_set.contains(&object_key) {
                finalizable_candidates.push(object_key);
            }
            if weak_candidate_set.contains(&object_key) {
                weak_candidates.push(object_key);
            }
            if ephemeron_candidate_set.contains(&object_key) {
                ephemeron_candidates.push(object_key);
            }
        }

        let mut finalize_indices = Vec::new();
        for (object_index, object) in objects.iter().enumerate() {
            let object_key = object.object_key();
            if !survivor_keys.contains(&object_key)
                && finalizable_candidate_set.contains(&object_key)
                && !object.header().is_moved_out()
            {
                finalize_indices.push(object_index);
            }
        }

        let (remembered_edges, remembered_owners) =
            self.remembered_edges_for_collection(objects, kind);
        PreparedIndexReclaim {
            rebuilt_object_index,
            finalize_indices,
            finalizable_candidates,
            weak_candidates,
            ephemeron_candidates,
            remembered_edges,
            remembered_owners,
        }
    }

    pub(crate) fn apply_prepared_reclaim(&mut self, prepared: PreparedIndexReclaim) {
        self.object_index = prepared.rebuilt_object_index;
        self.finalizable_candidates = prepared.finalizable_candidates;
        self.weak_candidates = prepared.weak_candidates;
        self.ephemeron_candidates = prepared.ephemeron_candidates;
        self.remembered
            .replace(prepared.remembered_edges, prepared.remembered_owners);
    }

    pub(crate) fn retain_remembered_edges_for_post_sweep_objects(
        &mut self,
        objects: &[ObjectRecord],
    ) {
        self.remembered
            .retain_for_post_sweep_objects(objects, &self.object_index);
    }
}

impl PostSweepIndexRebuild {
    pub(crate) fn should_enqueue_finalizer(&self, object: &ObjectRecord) -> bool {
        self.finalizable_candidates.contains(&object.object_key())
            && !object.header().is_moved_out()
    }
}

fn keep_object_for_collection(kind: CollectionKind, object: &ObjectRecord) -> bool {
    match kind {
        CollectionKind::Minor => {
            object.space() == SpaceKind::Immortal
                || object.space() != SpaceKind::Nursery
                || (object.is_marked() && !object.header().is_moved_out())
        }
        CollectionKind::Major | CollectionKind::Full => {
            object.space() == SpaceKind::Immortal
                || (object.is_marked() && !object.header().is_moved_out())
        }
    }
}

#[cfg(test)]
#[path = "index_state_test.rs"]
mod tests;
