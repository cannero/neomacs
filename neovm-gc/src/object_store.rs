use std::hash::{BuildHasher, Hash, Hasher};
use std::sync::{RwLock, RwLockReadGuard};

use crate::descriptor::{ObjectKey, TypeDesc, TypeFlags};
use crate::index_state::{
    HeapIndexState, ObjectIndex, ObjectKeyBuildHasher, ObjectLocator, RememberedSetState,
};
use crate::object::ObjectRecord;

const OBJECT_STORE_SHARDS: usize = 32;

#[derive(Debug, Default)]
struct ObjectShard {
    objects: Vec<ObjectRecord>,
    object_index: ObjectIndex,
    finalizable_candidates: Vec<ObjectKey>,
    weak_candidates: Vec<ObjectKey>,
    ephemeron_candidates: Vec<ObjectKey>,
}

impl ObjectShard {
    fn clear(&mut self) {
        self.objects.clear();
        self.object_index.clear();
        self.finalizable_candidates.clear();
        self.weak_candidates.clear();
        self.ephemeron_candidates.clear();
    }

    fn record_allocated_object(
        &mut self,
        shard: usize,
        object_key: ObjectKey,
        slot: usize,
        desc: &'static TypeDesc,
    ) {
        self.object_index
            .insert(object_key, ObjectLocator::new(shard, slot));
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
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ObjectShardReadRaw {
    objects_ptr: *const ObjectRecord,
    objects_len: usize,
    index_ptr: *const ObjectIndex,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ObjectReadRaw<'a> {
    shards: &'a [ObjectShardReadRaw],
    object_count: usize,
}

impl<'a> ObjectReadRaw<'a> {
    #[inline]
    pub(crate) fn len(self) -> usize {
        self.object_count
    }

    #[inline]
    pub(crate) fn get(self, locator: ObjectLocator) -> &'a ObjectRecord {
        let shard = &self.shards[locator.shard];
        debug_assert!(locator.slot < shard.objects_len);
        unsafe { &*shard.objects_ptr.add(locator.slot) }
    }

    #[inline]
    pub(crate) fn locator_of_key(self, key: ObjectKey) -> Option<ObjectLocator> {
        let shard = shard_index_for_key(key, self.shards.len());
        unsafe { (&*self.shards[shard].index_ptr).get(&key).copied() }
    }

    pub(crate) fn all_locators(self) -> Vec<ObjectLocator> {
        let mut locators = Vec::with_capacity(self.object_count);
        for (shard_index, shard) in self.shards.iter().enumerate() {
            for slot in 0..shard.objects_len {
                locators.push(ObjectLocator::new(shard_index, slot));
            }
        }
        locators
    }
}

unsafe impl Send for ObjectReadRaw<'_> {}
unsafe impl Sync for ObjectReadRaw<'_> {}

pub(crate) trait ObjectReadView {
    fn raw(&self) -> ObjectReadRaw<'_>;

    #[inline]
    fn len(&self) -> usize {
        self.raw().len()
    }

    #[inline]
    fn get(&self, locator: ObjectLocator) -> &ObjectRecord {
        self.raw().get(locator)
    }

    #[inline]
    fn locator_of_key(&self, key: ObjectKey) -> Option<ObjectLocator> {
        self.raw().locator_of_key(key)
    }

    #[inline]
    fn all_locators(&self) -> Vec<ObjectLocator> {
        self.raw().all_locators()
    }
}

#[derive(Debug)]
pub(crate) struct ObjectStoreReadGuard<'a> {
    _guards: Vec<RwLockReadGuard<'a, ObjectShard>>,
    raw: Vec<ObjectShardReadRaw>,
    object_count: usize,
    remembered: &'a RememberedSetState,
}

impl<'a> ObjectStoreReadGuard<'a> {
    #[inline]
    pub(crate) fn raw(&'a self) -> ObjectReadRaw<'a> {
        ObjectReadRaw {
            shards: &self.raw,
            object_count: self.object_count,
        }
    }

    #[inline]
    pub(crate) fn object_count(&self) -> usize {
        self.object_count
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.object_count
    }

    pub(crate) fn iter(&'a self) -> impl Iterator<Item = &'a ObjectRecord> + 'a {
        self._guards.iter().flat_map(|shard| shard.objects.iter())
    }

    pub(crate) fn finalizable_candidates(&self) -> Vec<ObjectKey> {
        self._guards
            .iter()
            .flat_map(|shard| shard.finalizable_candidates.iter().copied())
            .collect()
    }

    pub(crate) fn weak_candidates(&self) -> Vec<ObjectKey> {
        self._guards
            .iter()
            .flat_map(|shard| shard.weak_candidates.iter().copied())
            .collect()
    }

    pub(crate) fn ephemeron_candidates(&self) -> Vec<ObjectKey> {
        self._guards
            .iter()
            .flat_map(|shard| shard.ephemeron_candidates.iter().copied())
            .collect()
    }

    #[inline]
    pub(crate) fn remembered(&self) -> &RememberedSetState {
        self.remembered
    }

    pub(crate) fn candidate_counts(&self) -> (usize, usize, usize) {
        self._guards.iter().fold((0, 0, 0), |(f, w, e), shard| {
            (
                f + shard.finalizable_candidates.len(),
                w + shard.weak_candidates.len(),
                e + shard.ephemeron_candidates.len(),
            )
        })
    }
}

impl ObjectReadView for ObjectStoreReadGuard<'_> {
    fn raw(&self) -> ObjectReadRaw<'_> {
        ObjectReadRaw {
            shards: &self.raw,
            object_count: self.object_count,
        }
    }
}

#[derive(Debug)]
pub(crate) struct FlatObjectStore {
    pub(crate) objects: Vec<ObjectRecord>,
    pub(crate) indexes: HeapIndexState,
}

#[derive(Debug)]
pub(crate) struct FlatReadView<'a> {
    raw: [ObjectShardReadRaw; 1],
    object_count: usize,
    pub(crate) objects: &'a [ObjectRecord],
    pub(crate) indexes: &'a HeapIndexState,
}

impl<'a> FlatReadView<'a> {
    pub(crate) fn new(objects: &'a [ObjectRecord], indexes: &'a HeapIndexState) -> Self {
        Self {
            raw: [ObjectShardReadRaw {
                objects_ptr: objects.as_ptr(),
                objects_len: objects.len(),
                index_ptr: &indexes.object_index as *const _,
            }],
            object_count: objects.len(),
            objects,
            indexes,
        }
    }

    #[inline]
    pub(crate) fn raw(&'a self) -> ObjectReadRaw<'a> {
        ObjectReadRaw {
            shards: &self.raw,
            object_count: self.object_count,
        }
    }
}

impl ObjectReadView for FlatReadView<'_> {
    fn raw(&self) -> ObjectReadRaw<'_> {
        ObjectReadRaw {
            shards: &self.raw,
            object_count: self.object_count,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ObjectStore {
    shards: Box<[RwLock<ObjectShard>]>,
    remembered: RememberedSetState,
}

impl Default for ObjectStore {
    fn default() -> Self {
        let mut shards = Vec::with_capacity(OBJECT_STORE_SHARDS);
        for _ in 0..OBJECT_STORE_SHARDS {
            shards.push(RwLock::new(ObjectShard::default()));
        }
        Self {
            shards: shards.into_boxed_slice(),
            remembered: RememberedSetState::default(),
        }
    }
}

impl ObjectStore {
    pub(crate) fn read(&self) -> ObjectStoreReadGuard<'_> {
        let mut guards = Vec::with_capacity(self.shards.len());
        let mut raw = Vec::with_capacity(self.shards.len());
        let mut object_count = 0usize;
        for shard in self.shards.iter() {
            let guard = shard.read().expect("object shard lock poisoned");
            object_count = object_count.saturating_add(guard.objects.len());
            raw.push(ObjectShardReadRaw {
                objects_ptr: guard.objects.as_ptr(),
                objects_len: guard.objects.len(),
                index_ptr: &guard.object_index as *const _,
            });
            guards.push(guard);
        }
        ObjectStoreReadGuard {
            _guards: guards,
            raw,
            object_count,
            remembered: &self.remembered,
        }
    }

    pub(crate) fn remember_owner_shared(&self, owner_key: ObjectKey) {
        self.remembered.record_owner_shared(owner_key);
    }

    pub(crate) fn effective_remembered_len(&self) -> usize {
        self.remembered.effective_len()
    }

    pub(crate) fn restore_remembered(&mut self, owners: Vec<ObjectKey>) {
        self.remembered.replace(owners);
    }

    pub(crate) fn publish_shared(&self, record: ObjectRecord) -> ObjectLocator {
        let object_key = record.object_key();
        let shard_index = shard_index_for_key(object_key, self.shards.len());
        let mut shard = self.shards[shard_index]
            .write()
            .expect("object shard lock poisoned");
        let slot = shard.objects.len();
        let desc = record.header().desc();
        shard.objects.push(record);
        shard.record_allocated_object(shard_index, object_key, slot, desc);
        ObjectLocator::new(shard_index, slot)
    }

    pub(crate) fn take_flat(&mut self) -> FlatObjectStore {
        let mut objects = Vec::new();
        let mut remembered = std::mem::take(&mut self.remembered);
        for shard_lock in self.shards.iter_mut() {
            let shard = shard_lock.get_mut().expect("object shard lock poisoned");
            objects.append(&mut shard.objects);
            shard.clear();
        }
        let mut indexes = HeapIndexState::default();
        indexes.remembered = std::mem::take(&mut remembered);
        indexes.reset_candidate_indexes(objects.len());
        for (slot, object) in objects.iter().enumerate() {
            indexes.record_allocated_object(
                object.object_key(),
                ObjectLocator::flat(slot),
                object.header().desc(),
            );
        }
        FlatObjectStore { objects, indexes }
    }

    pub(crate) fn restore_from_flat(&mut self, mut flat: FlatObjectStore) {
        self.remembered = std::mem::take(&mut flat.indexes.remembered);
        for shard_lock in self.shards.iter_mut() {
            shard_lock
                .get_mut()
                .expect("object shard lock poisoned")
                .clear();
        }
        for object in flat.objects.drain(..) {
            let object_key = object.object_key();
            let shard_index = shard_index_for_key(object_key, self.shards.len());
            let shard = self.shards[shard_index]
                .get_mut()
                .expect("object shard lock poisoned");
            let slot = shard.objects.len();
            let desc = object.header().desc();
            shard.objects.push(object);
            shard.record_allocated_object(shard_index, object_key, slot, desc);
        }
    }
}

fn shard_index_for_key(key: ObjectKey, shard_count: usize) -> usize {
    debug_assert!(shard_count > 0);
    let mut hasher = ObjectKeyBuildHasher.build_hasher();
    key.hash(&mut hasher);
    (hasher.finish() as usize) % shard_count
}
