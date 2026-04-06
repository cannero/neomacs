use std::collections::{HashMap, HashSet};

use crate::barrier::RememberedEdge;
use crate::descriptor::{GcErased, ObjectKey, TypeDesc, TypeFlags};
use crate::object::{ObjectRecord, SpaceKind};
use crate::plan::CollectionKind;

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

    pub(crate) fn remembered_edges_for_collection(
        &self,
        objects: &[ObjectRecord],
        kind: CollectionKind,
    ) -> (Vec<RememberedEdge>, Vec<ObjectKey>) {
        self.remembered
            .edges_for_collection(objects, &self.object_index, kind)
    }

    pub(crate) fn retain_remembered_edges_for_post_sweep_objects(
        &mut self,
        objects: &[ObjectRecord],
    ) {
        self.remembered
            .retain_for_post_sweep_objects(objects, &self.object_index);
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
