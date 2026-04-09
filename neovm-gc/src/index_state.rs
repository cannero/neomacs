use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::ops::BitXor;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::descriptor::{GcErased, ObjectKey, Tracer, TypeDesc, TypeFlags};
use crate::object::{ObjectRecord, SpaceKind};
use crate::plan::CollectionKind;
use crate::reclaim::PreparedReclaimSurvivor;
use crate::spaces::OldGenState;
use crate::stats::HeapStats;

/// Fast non-cryptographic hasher specialized for
/// [`ObjectKey`] and other `usize`-width keys that don't
/// need HashDoS resistance. Uses the FxHash algorithm
/// (rotate + XOR + multiply by a 64-bit seed), which is
/// what rustc and clippy use internally for their own
/// hot-path maps.
///
/// A flamegraph of `multi_mutator_scaling/alloc/1` showed
/// ~16% of cycles in `SipHasher13::finish` called from
/// `ObjectIndex::insert` on every allocation; swapping the
/// default `RandomState` (SipHash13) hasher for this one
/// eliminates almost all of that cost. ObjectKey is an
/// internal crate type (wrapping a `NonNull<ObjectHeader>`
/// pointer cast to `usize`) that is never exposed to
/// untrusted input, so HashDoS resistance is not needed
/// here.
#[derive(Default)]
pub(crate) struct ObjectKeyHasher(u64);

const OBJECT_KEY_HASH_SEED: u64 = 0xf1f6_4f39_f27f_9fe1;
const OBJECT_KEY_HASH_ROTATE: u32 = 5;

impl ObjectKeyHasher {
    #[inline]
    fn add(&mut self, i: u64) {
        self.0 = self
            .0
            .rotate_left(OBJECT_KEY_HASH_ROTATE)
            .bitxor(i)
            .wrapping_mul(OBJECT_KEY_HASH_SEED);
    }
}

impl Hasher for ObjectKeyHasher {
    #[inline]
    fn write(&mut self, mut bytes: &[u8]) {
        while bytes.len() >= 8 {
            let (chunk, rest) = bytes.split_at(8);
            self.add(u64::from_ne_bytes(chunk.try_into().unwrap()));
            bytes = rest;
        }
        for &byte in bytes {
            self.add(byte as u64);
        }
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

/// `BuildHasher` that produces fresh [`ObjectKeyHasher`]
/// instances. `#[derive(Default, Clone)]` gives
/// `HashMap::with_hasher` and clone-based rehashes a
/// trivial zero-overhead setup; there is no seed state to
/// carry.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ObjectKeyBuildHasher;

impl BuildHasher for ObjectKeyBuildHasher {
    type Hasher = ObjectKeyHasher;
    #[inline]
    fn build_hasher(&self) -> Self::Hasher {
        ObjectKeyHasher(0)
    }
}

pub(crate) type ObjectIndex = HashMap<ObjectKey, usize, ObjectKeyBuildHasher>;
pub(crate) type ForwardingMap = HashMap<ObjectKey, GcErased, ObjectKeyBuildHasher>;

/// Explicit-edge fallback remembered set, owner-only model.
///
/// Tracks the deduped set of non-block-backed old-gen owners
/// that have at least one edge into the nursery. The minor GC
/// scans these owners as additional roots; after each
/// collection the set is re-derived by walking each candidate
/// owner's record edges. Owners that no longer hold a nursery
/// reference (because the target moved or was overwritten)
/// drop out of the set automatically.
///
/// Block-backed owners take the per-block dirty card fast path
/// instead and never appear in this set.
///
/// The `pending_inserts` mutex exists so the write-barrier hot
/// path can record a new fallback owner without acquiring the
/// `HeapCore` write lock. The canonical `owners`/`owner_set`
/// fields are only mutated through `&mut self` methods (GC-time
/// paths that already hold the `HeapCore` write lock); readers
/// that need to see the combined set either call
/// `owners_including_pending()` (for `&self`) or
/// `merge_pending_owners()` first (for `&mut self`).
///
/// `pending_count` shadows `pending_inserts.len()` as a
/// relaxed atomic so callers can test "is pending empty?"
/// without acquiring the mutex. This matters because
/// `storage_stats()` is called on the barrier hot path (via
/// the active-major-mark refresh) and the old per-barrier
/// cost was dominated by a hot-path mutex acquisition when
/// the counter was added naively.
#[derive(Debug)]
pub(crate) struct RememberedSetState {
    pub(crate) owners: Vec<ObjectKey>,
    owner_set: HashSet<ObjectKey>,
    /// Owners the barrier hot path appended without the
    /// `HeapCore` write lock. Drained into `owners`/`owner_set`
    /// by the next GC-time consumer via `merge_pending_owners`.
    pending_inserts: Mutex<Vec<ObjectKey>>,
    /// Relaxed mirror of `pending_inserts.len()` so readers can
    /// skip the mutex on the empty fast path. Incremented under
    /// the `pending_inserts` mutex on push and cleared via
    /// `store(0)` during drain/clear.
    pending_count: AtomicUsize,
}

impl Default for RememberedSetState {
    fn default() -> Self {
        Self {
            owners: Vec::new(),
            owner_set: HashSet::new(),
            pending_inserts: Mutex::new(Vec::new()),
            pending_count: AtomicUsize::new(0),
        }
    }
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
    pub(crate) remembered_owners: Vec<ObjectKey>,
}

#[derive(Debug, Default)]
pub(crate) struct PostSweepIndexRebuild {
    finalizable_candidates: HashSet<ObjectKey>,
}

impl RememberedSetState {
    /// Record `owner_key` as having a fresh old-to-young edge.
    /// Idempotent: a second call with the same key is a no-op.
    /// The caller is responsible for ensuring the owner is not
    /// block-backed (block-backed owners take the per-block
    /// dirty card path instead). Only used by tests that seed
    /// the canonical set directly; production code uses
    /// [`record_owner_shared`] + [`merge_pending_owners`].
    #[cfg(test)]
    pub(crate) fn record_owner(&mut self, owner_key: ObjectKey) {
        if self.owner_set.insert(owner_key) {
            self.owners.push(owner_key);
        }
    }

    /// Barrier hot-path insert: queue `owner_key` for merge into
    /// the canonical owners set on the next GC-time visit.
    /// Callable through `&self` because the only mutation is to
    /// `pending_inserts`, which is guarded by its own mutex.
    /// Dedup and idempotency are deferred to
    /// [`merge_pending_owners`] so the hot path stays as cheap
    /// as one atomic mutex acquisition plus one vec push.
    pub(crate) fn record_owner_shared(&self, owner_key: ObjectKey) {
        let mut pending = self
            .pending_inserts
            .lock()
            .expect("remembered pending_inserts poisoned");
        pending.push(owner_key);
        self.pending_count.store(pending.len(), Ordering::Relaxed);
    }

    /// Drain `pending_inserts` into the canonical
    /// `owners`/`owner_set`, preserving insertion order and
    /// deduping new entries against both the existing canonical
    /// set and the other pending entries.
    pub(crate) fn merge_pending_owners(&mut self) {
        let drained = std::mem::take(
            self.pending_inserts
                .get_mut()
                .expect("remembered pending_inserts poisoned"),
        );
        self.pending_count.store(0, Ordering::Relaxed);
        for key in drained {
            if self.owner_set.insert(key) {
                self.owners.push(key);
            }
        }
    }

    /// Fast-path length of the effective remembered set —
    /// canonical `owners` plus any pending hot-path inserts —
    /// that avoids allocating when pending is empty. The empty
    /// check uses a relaxed atomic so the barrier hot path
    /// (which calls `storage_stats` for the active-major-mark
    /// assist) never acquires the pending mutex when there's
    /// nothing queued. Marked `#[inline]` so the allocation
    /// hot path (which calls `storage_stats` through the
    /// collector plan refresh) folds the atomic load and the
    /// `Vec::len` read into a single basic block.
    #[inline]
    pub(crate) fn effective_len(&self) -> usize {
        if self.pending_count.load(Ordering::Relaxed) == 0 {
            return self.owners.len();
        }
        // Slow path: dedup pending against canonical owners and
        // against itself. Matches the dedup
        // `owners_including_pending` would produce without
        // allocating a full combined `Vec`.
        let pending = self
            .pending_inserts
            .lock()
            .expect("remembered pending_inserts poisoned");
        let mut extra = 0usize;
        let mut seen: HashSet<ObjectKey> = HashSet::new();
        for key in pending.iter().copied() {
            if !self.owner_set.contains(&key) && seen.insert(key) {
                extra = extra.saturating_add(1);
            }
        }
        self.owners.len().saturating_add(extra)
    }

    /// Snapshot of the remembered-owner set including any entries
    /// the hot path has queued via `record_owner_shared` but not
    /// yet merged. Callable through `&self`. Returns a freshly
    /// allocated `Vec`; use the `&mut self` path when the caller
    /// already holds exclusive access.
    pub(crate) fn owners_including_pending(&self) -> Vec<ObjectKey> {
        if self.pending_count.load(Ordering::Relaxed) == 0 {
            return self.owners.clone();
        }
        let pending = self
            .pending_inserts
            .lock()
            .expect("remembered pending_inserts poisoned");
        if pending.is_empty() {
            return self.owners.clone();
        }
        let mut combined = Vec::with_capacity(self.owners.len() + pending.len());
        combined.extend(self.owners.iter().copied());
        for key in pending.iter().copied() {
            if !self.owner_set.contains(&key) && !combined[self.owners.len()..].contains(&key) {
                combined.push(key);
            }
        }
        combined
    }


    /// Re-derive the current owner set by walking each tracked
    /// owner's record edges. Owners that are dead (no longer in
    /// `object_index`), moved out, no longer in an old-gen
    /// space, or no longer hold any nursery reference get
    /// dropped.
    ///
    /// Used after every minor cycle to clean up owners whose
    /// nursery edges either moved to old (via promotion) or
    /// were overwritten between collections.
    pub(crate) fn refresh_from_records(
        &mut self,
        objects: &[ObjectRecord],
        object_index: &ObjectIndex,
    ) {
        self.merge_pending_owners();
        let mut next_owners = Vec::with_capacity(self.owners.len());
        let mut next_set = HashSet::with_capacity(self.owners.len());
        for owner_key in &self.owners {
            let Some(&owner_index) = object_index.get(owner_key) else {
                continue;
            };
            let owner = &objects[owner_index];
            if !owner_qualifies_as_explicit_remembered_owner(owner) {
                continue;
            }
            if owner_has_nursery_edge(objects, object_index, owner)
                && next_set.insert(*owner_key)
            {
                next_owners.push(*owner_key);
            }
        }
        self.owners = next_owners;
        self.owner_set = next_set;
    }

    /// Filter the current owners against a `kind`-specific
    /// "keep" predicate (e.g. major drops unmarked owners) and
    /// the live-nursery-edge predicate. Returns the surviving
    /// owner keys ordered by the original insertion order. Any
    /// hot-path entries still sitting in `pending_inserts` are
    /// merged into the result so the collection sees a
    /// consistent view.
    pub(crate) fn owners_for_collection(
        &self,
        objects: &[ObjectRecord],
        object_index: &ObjectIndex,
        kind: CollectionKind,
    ) -> Vec<ObjectKey> {
        self.owners_including_pending()
            .into_iter()
            .filter(|owner_key| {
                let Some(&owner_index) = object_index.get(owner_key) else {
                    return false;
                };
                let owner = &objects[owner_index];
                if !keep_object_for_collection(kind, owner) {
                    return false;
                }
                if !owner_qualifies_as_explicit_remembered_owner(owner) {
                    return false;
                }
                owner_has_nursery_edge(objects, object_index, owner)
            })
            .collect()
    }

    /// Replace the owner set with the given list. Used by the
    /// prepared-reclaim commit path so the owners filtered
    /// during prepare match the rebuilt object index.
    pub(crate) fn replace(&mut self, owners: Vec<ObjectKey>) {
        self.owners = owners;
        self.owner_set = self.owners.iter().copied().collect();
        // Pending inserts were already folded into the prepared
        // owner list during `owners_for_collection`, so clear
        // them atomically here to avoid re-introducing stale
        // entries on the next merge.
        self.pending_inserts
            .get_mut()
            .expect("remembered pending_inserts poisoned")
            .clear();
        self.pending_count.store(0, Ordering::Relaxed);
    }
}

/// Predicate: is `owner` a candidate for explicit-edge
/// remembered tracking? Pinned and large-space records (and
/// any other non-nursery, non-immortal owner that has not been
/// moved out) qualify; nursery, immortal, and moved-out
/// records do not.
fn owner_qualifies_as_explicit_remembered_owner(owner: &ObjectRecord) -> bool {
    let space = owner.space();
    space != SpaceKind::Nursery
        && space != SpaceKind::Immortal
        && !owner.header().is_moved_out()
}

/// Predicate: does `owner` currently hold at least one
/// reference into the nursery? Walks the owner's trace edges
/// with a short-circuiting tracer that stops as soon as it
/// finds a nursery target.
fn owner_has_nursery_edge(
    objects: &[ObjectRecord],
    object_index: &ObjectIndex,
    owner: &ObjectRecord,
) -> bool {
    struct NurseryDetectTracer<'a> {
        objects: &'a [ObjectRecord],
        index: &'a ObjectIndex,
        seen: bool,
    }

    impl Tracer for NurseryDetectTracer<'_> {
        fn mark_erased(&mut self, object: GcErased) {
            if self.seen {
                return;
            }
            if let Some(&target_index) = self.index.get(&object.object_key())
                && self.objects[target_index].space() == SpaceKind::Nursery
            {
                self.seen = true;
            }
        }
    }

    let mut tracer = NurseryDetectTracer {
        objects,
        index: object_index,
        seen: false,
    };
    owner.trace_edges(&mut tracer);
    tracer.seen
}

impl HeapIndexState {
    pub(crate) fn space_for_erased(
        &self,
        objects: &[ObjectRecord],
        object: GcErased,
    ) -> Option<SpaceKind> {
        self.object_index
            .get(&object.object_key())
            .map(|&index| objects[index].space())
    }

    pub(crate) fn apply_storage_stats(&self, stats: &mut HeapStats) {
        // The explicit-edge fallback is now an owner-only set
        // (one entry per non-block-backed old-gen owner that
        // has at least one nursery edge). Each owner counts as
        // one entry in both the edge and owner stats since the
        // dense edge Vec was retired.
        //
        // `effective_len` folds the hot-path `pending_inserts`
        // Vec into a deduped count without allocating. The
        // empty-pending fast path is a single relaxed atomic
        // load, which keeps this call cheap for the barrier
        // hot path (which reaches here via
        // `storage_stats()` → `apply_storage_stats`).
        let explicit_owners = self.remembered.effective_len();
        stats.remembered_explicit_edges = explicit_owners;
        stats.remembered_explicit_owners = explicit_owners;
        stats.remembered_edges = explicit_owners;
        stats.remembered_owners = explicit_owners;
        stats.remembered_dirty_cards = 0;
        stats.remembered_dirty_card_owners = 0;
        stats.finalizable_candidates = self.finalizable_candidates.len();
        stats.weak_candidates = self.weak_candidates.len();
        stats.ephemeron_candidates = self.ephemeron_candidates.len();
    }

    /// Fold dirty card counts (the per-block fast-path remembered set)
    /// into the unified `stats.remembered_edges` / `stats.remembered
    /// _owners` counters so existing observers see the combined
    /// picture, AND populate the split
    /// `stats.remembered_dirty_cards` counter so observers that
    /// want to attribute pressure to the fast path can read it
    /// directly. Each dirty card is counted as both one edge and
    /// one owner approximation since the card represents at least
    /// one pending old-to-young root in that region.
    pub(crate) fn apply_dirty_card_storage_stats(
        &self,
        stats: &mut HeapStats,
        old_gen: &OldGenState,
    ) {
        let dirty_cards = old_gen.dirty_card_count();
        stats.remembered_dirty_cards = dirty_cards;
        stats.remembered_dirty_card_owners = dirty_cards;
        stats.remembered_edges = stats.remembered_edges.saturating_add(dirty_cards);
        stats.remembered_owners = stats.remembered_owners.saturating_add(dirty_cards);
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

    /// Seed the canonical fallback owner set directly. Used by
    /// tests and by the post-sweep rebuild path that replaces
    /// the set wholesale. Production barrier code routes
    /// through [`record_remembered_edge_if_needed`], which uses
    /// [`RememberedSetState::record_owner_shared`] internally.
    #[cfg(test)]
    pub(crate) fn record_remembered_owner(&mut self, owner: GcErased) {
        self.remembered.record_owner(owner.object_key());
    }

    /// Barrier hot-path entry. Takes `&self` so the
    /// `Mutator::store_edge` path only needs a `HeapCore` read
    /// lock. Any fallback-set mutation is routed through
    /// `RememberedSetState::record_owner_shared`, which appends
    /// to a per-set mutex rather than the canonical owner list.
    /// GC-time consumers merge the pending entries via
    /// [`RememberedSetState::merge_pending_owners`] or
    /// [`RememberedSetState::owners_for_collection`].
    pub(crate) fn record_remembered_edge_if_needed(
        &self,
        objects: &[ObjectRecord],
        old_gen: &OldGenState,
        owner: GcErased,
        new_value: Option<GcErased>,
    ) {
        let Some(owner_space) = self.space_for_erased(objects, owner) else {
            return;
        };
        let Some(target) = new_value else {
            return;
        };
        let Some(target_space) = self.space_for_erased(objects, target) else {
            return;
        };

        let owner_is_old = owner_space != SpaceKind::Nursery && owner_space != SpaceKind::Immortal;
        if owner_is_old && target_space == SpaceKind::Nursery {
            // Prefer marking the per-block card table over the
            // owner-only fallback set. The card table tracks
            // mutated regions in O(1) per write and rebuilds the
            // owner-set lazily by walking dirty cards at the
            // start of the next minor GC. The owner-only set
            // remains as a fallback for non-block-backed owners
            // (pinned space, large space, or system-allocated
            // promotions that didn't fit any block hole).
            let owner_addr = owner.header().as_ptr() as usize;
            if old_gen.record_write_barrier(owner_addr) {
                return;
            }
            self.remembered.record_owner_shared(owner.object_key());
        }
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

    pub(crate) fn remembered_owners_for_collection(
        &self,
        objects: &[ObjectRecord],
        kind: CollectionKind,
    ) -> Vec<ObjectKey> {
        self.remembered
            .owners_for_collection(objects, &self.object_index, kind)
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

        let mut rebuilt_object_index =
            ObjectIndex::with_capacity_and_hasher(survivors.len(), ObjectKeyBuildHasher);
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

        let remembered_owners = self.remembered_owners_for_collection(objects, kind);
        PreparedIndexReclaim {
            rebuilt_object_index,
            finalize_indices,
            finalizable_candidates,
            weak_candidates,
            ephemeron_candidates,
            remembered_owners,
        }
    }

    pub(crate) fn apply_prepared_reclaim(&mut self, prepared: PreparedIndexReclaim) {
        self.object_index = prepared.rebuilt_object_index;
        self.finalizable_candidates = prepared.finalizable_candidates;
        self.weak_candidates = prepared.weak_candidates;
        self.ephemeron_candidates = prepared.ephemeron_candidates;
        self.remembered.replace(prepared.remembered_owners);
    }

    pub(crate) fn refresh_remembered_owners_for_post_sweep_objects(
        &mut self,
        objects: &[ObjectRecord],
    ) {
        self.remembered
            .refresh_from_records(objects, &self.object_index);
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
