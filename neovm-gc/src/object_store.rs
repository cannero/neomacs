use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use parking_lot::{RwLock, RwLockReadGuard};

use crate::descriptor::{ObjectKey, TypeFlags};
use crate::index_state::{
    HeapIndexState, ObjectIndex, ObjectKeyBuildHasher, ObjectLocator, RememberedSetState,
};
use crate::object::ObjectRecord;

pub(crate) const OBJECT_STORE_SHARDS: usize = 32;
const OBJECT_STORE_CHUNK_CAPACITY: usize = 256;

#[derive(Debug)]
struct ObjectChunk {
    objects: Box<[MaybeUninit<ObjectRecord>]>,
    published_len: AtomicUsize,
}

impl ObjectChunk {
    fn new() -> Self {
        Self {
            objects: Box::new_uninit_slice(OBJECT_STORE_CHUNK_CAPACITY),
            published_len: AtomicUsize::new(0),
        }
    }

    #[inline]
    fn published_len(&self) -> usize {
        self.published_len.load(Ordering::Acquire)
    }

    #[inline]
    fn is_full(&self) -> bool {
        self.published_len() >= OBJECT_STORE_CHUNK_CAPACITY
    }

    unsafe fn write_reserved(&self, offset: usize, record: ObjectRecord) {
        debug_assert!(offset < OBJECT_STORE_CHUNK_CAPACITY);
        debug_assert_eq!(offset, self.published_len.load(Ordering::Relaxed));
        let slot = unsafe { self.objects.as_ptr().add(offset) as *mut MaybeUninit<ObjectRecord> };
        unsafe { (*slot).write(record) };
    }

    fn publish_reserved(&self, offset: usize) {
        self.published_len
            .store(offset.saturating_add(1), Ordering::Release);
    }

    fn read_raw(&self) -> ObjectChunkReadRaw {
        ObjectChunkReadRaw {
            objects_ptr: self.objects.as_ptr() as *const ObjectRecord,
            published_len: self.published_len(),
        }
    }

    fn iter(&self) -> impl Iterator<Item = &ObjectRecord> + '_ {
        let published = self.published_len();
        (0..published).map(|slot| unsafe { &*self.objects[slot].as_ptr() })
    }

    fn drain_published_into(&self, out: &mut Vec<ObjectRecord>) {
        let published = self.published_len.swap(0, Ordering::AcqRel);
        out.reserve(published);
        for slot in 0..published {
            out.push(unsafe { self.objects[slot].assume_init_read() });
        }
    }
}

impl Drop for ObjectChunk {
    fn drop(&mut self) {
        let published = *self.published_len.get_mut();
        for slot in 0..published {
            unsafe { self.objects[slot].assume_init_drop() };
        }
    }
}

#[derive(Clone, Debug)]
struct ObjectChunkReadRaw {
    objects_ptr: *const ObjectRecord,
    published_len: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct ObjectShardReadRaw {
    chunks: Arc<[ObjectChunkReadRaw]>,
}

#[derive(Clone, Debug)]
pub(crate) struct ObjectReadRaw<'a> {
    shards: Arc<[ObjectShardReadRaw]>,
    index_ptr: *const ObjectIndex,
    _owned_index: Option<Arc<ObjectIndex>>,
    all_locators: Arc<[ObjectLocator]>,
    object_count: usize,
    _marker: PhantomData<&'a ObjectIndex>,
}

impl<'a> ObjectReadRaw<'a> {
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.object_count
    }

    #[inline]
    pub(crate) fn get(&self, locator: ObjectLocator) -> &'a ObjectRecord {
        let shard = &self.shards[locator.shard];
        let chunk_index = locator.slot / OBJECT_STORE_CHUNK_CAPACITY;
        let chunk_offset = locator.slot % OBJECT_STORE_CHUNK_CAPACITY;
        debug_assert!(chunk_index < shard.chunks.len());
        let chunk = &shard.chunks[chunk_index];
        debug_assert!(chunk_offset < chunk.published_len);
        unsafe { &*chunk.objects_ptr.add(chunk_offset) }
    }

    #[inline]
    pub(crate) fn locator_of_key(&self, key: ObjectKey) -> Option<ObjectLocator> {
        unsafe { (&*self.index_ptr).get(&key).copied() }
    }

    pub(crate) fn all_locators(&self) -> Vec<ObjectLocator> {
        self.all_locators.as_ref().to_vec()
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
    _chunk_guards: Vec<RwLockReadGuard<'a, Vec<Arc<ObjectChunk>>>>,
    shards_raw: Arc<[ObjectShardReadRaw]>,
    index: Arc<ObjectIndex>,
    all_locators: Arc<[ObjectLocator]>,
    finalizable_candidates: Arc<[ObjectKey]>,
    weak_candidates: Arc<[ObjectKey]>,
    ephemeron_candidates: Arc<[ObjectKey]>,
    object_count: usize,
    remembered: &'a RememberedSetState,
}

impl<'a> ObjectStoreReadGuard<'a> {
    #[inline]
    pub(crate) fn raw(&'a self) -> ObjectReadRaw<'a> {
        ObjectReadRaw {
            shards: Arc::clone(&self.shards_raw),
            index_ptr: Arc::as_ptr(&self.index),
            _owned_index: Some(Arc::clone(&self.index)),
            all_locators: Arc::clone(&self.all_locators),
            object_count: self.object_count,
            _marker: PhantomData,
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
        self._chunk_guards
            .iter()
            .flat_map(|chunks| chunks.iter())
            .flat_map(|chunk| chunk.iter())
    }

    pub(crate) fn finalizable_candidates(&self) -> Vec<ObjectKey> {
        self.finalizable_candidates.as_ref().to_vec()
    }

    pub(crate) fn weak_candidates(&self) -> Vec<ObjectKey> {
        self.weak_candidates.as_ref().to_vec()
    }

    pub(crate) fn ephemeron_candidates(&self) -> Vec<ObjectKey> {
        self.ephemeron_candidates.as_ref().to_vec()
    }

    #[inline]
    pub(crate) fn remembered(&self) -> &RememberedSetState {
        self.remembered
    }

    pub(crate) fn candidate_counts(&self) -> (usize, usize, usize) {
        (
            self.finalizable_candidates.len(),
            self.weak_candidates.len(),
            self.ephemeron_candidates.len(),
        )
    }
}

impl ObjectReadView for ObjectStoreReadGuard<'_> {
    fn raw(&self) -> ObjectReadRaw<'_> {
        ObjectReadRaw {
            shards: Arc::clone(&self.shards_raw),
            index_ptr: Arc::as_ptr(&self.index),
            _owned_index: Some(Arc::clone(&self.index)),
            all_locators: Arc::clone(&self.all_locators),
            object_count: self.object_count,
            _marker: PhantomData,
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
    object_count: usize,
    pub(crate) objects: &'a [ObjectRecord],
    pub(crate) indexes: &'a HeapIndexState,
}

impl<'a> FlatReadView<'a> {
    pub(crate) fn new(objects: &'a [ObjectRecord], indexes: &'a HeapIndexState) -> Self {
        Self {
            object_count: objects.len(),
            objects,
            indexes,
        }
    }

    fn all_locators(&self) -> Arc<[ObjectLocator]> {
        Arc::from(
            (0..self.object_count)
                .map(ObjectLocator::flat)
                .collect::<Vec<_>>(),
        )
    }

    #[inline]
    pub(crate) fn raw(&'a self) -> ObjectReadRaw<'a> {
        ObjectReadRaw {
            shards: Arc::from(vec![ObjectShardReadRaw {
                chunks: Arc::from(
                    self.objects
                        .chunks(OBJECT_STORE_CHUNK_CAPACITY)
                        .map(|chunk| ObjectChunkReadRaw {
                            objects_ptr: chunk.as_ptr(),
                            published_len: chunk.len(),
                        })
                        .collect::<Vec<_>>(),
                ),
            }]),
            index_ptr: &self.indexes.object_index as *const _,
            _owned_index: None,
            all_locators: self.all_locators(),
            object_count: self.object_count,
            _marker: PhantomData,
        }
    }
}

impl ObjectReadView for FlatReadView<'_> {
    fn raw(&self) -> ObjectReadRaw<'_> {
        ObjectReadRaw {
            shards: Arc::from(vec![ObjectShardReadRaw {
                chunks: Arc::from(
                    self.objects
                        .chunks(OBJECT_STORE_CHUNK_CAPACITY)
                        .map(|chunk| ObjectChunkReadRaw {
                            objects_ptr: chunk.as_ptr(),
                            published_len: chunk.len(),
                        })
                        .collect::<Vec<_>>(),
                ),
            }]),
            index_ptr: &self.indexes.object_index as *const _,
            _owned_index: None,
            all_locators: self.all_locators(),
            object_count: self.object_count,
            _marker: PhantomData,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ObjectPublishReservation {
    generation: u64,
    chunk_index: usize,
    next_offset: usize,
    chunk: Arc<ObjectChunk>,
}

#[derive(Debug)]
pub(crate) struct ObjectPublishLocal {
    reservations: Box<[Option<ObjectPublishReservation>]>,
}

impl Default for ObjectPublishLocal {
    fn default() -> Self {
        let reservations = std::iter::repeat_with(|| None)
            .take(OBJECT_STORE_SHARDS)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self { reservations }
    }
}

impl ObjectPublishLocal {
    fn reservation_mut(&mut self, shard: usize) -> &mut Option<ObjectPublishReservation> {
        &mut self.reservations[shard]
    }
}

#[derive(Debug, Default)]
struct ObjectShard {
    chunks: RwLock<Vec<Arc<ObjectChunk>>>,
}

impl ObjectShard {
    fn clear(&mut self) {
        self.chunks.get_mut().clear();
    }

    fn publish_owned_mut(&mut self, record: ObjectRecord) {
        let chunks = self.chunks.get_mut();
        let needs_chunk = chunks
            .last()
            .map(|chunk: &Arc<ObjectChunk>| chunk.is_full())
            .unwrap_or(true);
        if needs_chunk {
            chunks.push(Arc::new(ObjectChunk::new()));
        }
        let chunk_index = chunks.len().saturating_sub(1);
        let chunk = chunks[chunk_index].clone();
        let chunk_offset = chunk.published_len();
        unsafe { chunk.write_reserved(chunk_offset, record) };
        chunk.publish_reserved(chunk_offset);
    }
}

#[derive(Debug)]
pub(crate) struct ObjectStore {
    shards: Box<[ObjectShard]>,
    remembered: RememberedSetState,
    generation: AtomicU64,
}

impl Default for ObjectStore {
    fn default() -> Self {
        let mut shards = Vec::with_capacity(OBJECT_STORE_SHARDS);
        for _ in 0..OBJECT_STORE_SHARDS {
            shards.push(ObjectShard::default());
        }
        Self {
            shards: shards.into_boxed_slice(),
            remembered: RememberedSetState::default(),
            generation: AtomicU64::new(0),
        }
    }
}

impl ObjectStore {
    #[inline]
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn bump_generation(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    fn reserve_publish_chunk(&self, shard_index: usize) -> ObjectPublishReservation {
        let generation = self.generation();
        let chunk = Arc::new(ObjectChunk::new());
        let mut chunks = self.shards[shard_index].chunks.write();
        let chunk_index = chunks.len();
        chunks.push(Arc::clone(&chunk));
        ObjectPublishReservation {
            generation,
            chunk_index,
            next_offset: 0,
            chunk,
        }
    }

    pub(crate) fn read(&self) -> ObjectStoreReadGuard<'_> {
        let mut chunk_guards = Vec::with_capacity(self.shards.len());
        let mut shard_raws = Vec::with_capacity(self.shards.len());
        let mut object_count = 0usize;

        for shard in self.shards.iter() {
            let chunk_guard = shard.chunks.read();
            let mut chunk_raws = Vec::with_capacity(chunk_guard.len());
            for chunk in chunk_guard.iter() {
                let raw = chunk.read_raw();
                object_count = object_count.saturating_add(raw.published_len);
                chunk_raws.push(raw);
            }
            shard_raws.push(ObjectShardReadRaw {
                chunks: Arc::from(chunk_raws),
            });
            chunk_guards.push(chunk_guard);
        }

        let mut object_index =
            ObjectIndex::with_capacity_and_hasher(object_count, ObjectKeyBuildHasher);
        let mut all_locators = Vec::with_capacity(object_count);
        let mut finalizable_candidates = Vec::new();
        let mut weak_candidates = Vec::new();
        let mut ephemeron_candidates = Vec::new();

        for (shard_index, shard) in shard_raws.iter().enumerate() {
            for (chunk_index, chunk) in shard.chunks.iter().enumerate() {
                let base_slot = chunk_index.saturating_mul(OBJECT_STORE_CHUNK_CAPACITY);
                for chunk_offset in 0..chunk.published_len {
                    let locator = ObjectLocator::new(shard_index, base_slot + chunk_offset);
                    let record = unsafe { &*chunk.objects_ptr.add(chunk_offset) };
                    let object_key = record.object_key();
                    let flags = record.header().desc().flags;
                    object_index.insert(object_key, locator);
                    all_locators.push(locator);
                    if flags.contains(TypeFlags::FINALIZABLE) {
                        finalizable_candidates.push(object_key);
                    }
                    if flags.contains(TypeFlags::WEAK) {
                        weak_candidates.push(object_key);
                    }
                    if flags.contains(TypeFlags::EPHEMERON_KEY) {
                        ephemeron_candidates.push(object_key);
                    }
                }
            }
        }

        ObjectStoreReadGuard {
            _chunk_guards: chunk_guards,
            shards_raw: Arc::from(shard_raws),
            index: Arc::new(object_index),
            all_locators: Arc::from(all_locators),
            finalizable_candidates: Arc::from(finalizable_candidates),
            weak_candidates: Arc::from(weak_candidates),
            ephemeron_candidates: Arc::from(ephemeron_candidates),
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

    pub(crate) fn publish_shared(
        &self,
        record: ObjectRecord,
        publish_local: &mut ObjectPublishLocal,
    ) -> ObjectLocator {
        let object_key = record.object_key();
        let shard_index = shard_index_for_key(object_key, self.shards.len());
        let reservation = publish_local.reservation_mut(shard_index);
        let generation = self.generation();
        let needs_reservation = reservation.as_ref().is_none_or(|reservation| {
            reservation.generation != generation
                || reservation.next_offset >= OBJECT_STORE_CHUNK_CAPACITY
        });
        if needs_reservation {
            *reservation = Some(self.reserve_publish_chunk(shard_index));
        }

        let reservation = reservation
            .as_mut()
            .expect("publish reservation should exist after refill");
        let chunk_offset = reservation.next_offset;
        let slot = reservation
            .chunk_index
            .saturating_mul(OBJECT_STORE_CHUNK_CAPACITY)
            .saturating_add(chunk_offset);
        unsafe { reservation.chunk.write_reserved(chunk_offset, record) };
        reservation.chunk.publish_reserved(chunk_offset);
        reservation.next_offset = reservation.next_offset.saturating_add(1);
        ObjectLocator::new(shard_index, slot)
    }

    pub(crate) fn take_flat(&mut self) -> FlatObjectStore {
        let mut objects = Vec::new();
        let mut remembered = std::mem::take(&mut self.remembered);
        for shard in self.shards.iter_mut() {
            let chunks = shard.chunks.get_mut();
            for chunk in chunks.iter() {
                chunk.drain_published_into(&mut objects);
            }
            chunks.clear();
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
        for shard in self.shards.iter_mut() {
            shard.clear();
        }
        for object in flat.objects.drain(..) {
            let object_key = object.object_key();
            let shard_index = shard_index_for_key(object_key, self.shards.len());
            self.shards[shard_index].publish_owned_mut(object);
        }
        self.bump_generation();
    }
}

fn shard_index_for_key(key: ObjectKey, shard_count: usize) -> usize {
    debug_assert!(shard_count > 0);
    let addr = key.as_usize() >> 4;
    let mixed =
        addr ^ addr.rotate_right(13) ^ addr.wrapping_mul(0x9e37_79b9_7f4a_7c15_u64 as usize);
    if shard_count.is_power_of_two() {
        mixed & (shard_count - 1)
    } else {
        mixed % shard_count
    }
}
