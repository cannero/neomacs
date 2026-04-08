use core::marker::PhantomData;
use core::slice;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use crossbeam_deque::{Steal, Stealer, Worker};

use crate::descriptor::{EphemeronVisitor, GcErased, ObjectKey, Relocator, Tracer, WeakProcessor};
use crate::heap::AllocError;
use crate::index_state::{ForwardingMap, HeapIndexState, ObjectIndex};
use crate::mark::MarkWorklist;
use crate::object::{ObjectRecord, SpaceKind};
use crate::plan::{CollectionKind, CollectionPhase, CollectionPlan};
use crate::reclaim::{
    PreparedReclaim, finish_prepared_reclaim_cycle,
    prepare_full_reclaim as orchestrate_full_reclaim,
    prepare_major_reclaim as orchestrate_major_reclaim, prepare_reclaim,
    sweep_minor_and_rebuild_post_collection as rebuild_minor_after_collection,
};
use crate::root::RootStack;
use crate::runtime_state::RuntimeStateHandle;
use crate::spaces::nursery::{
    NurseryConfig, evacuate_marked_nursery as evacuate_nursery_space,
    relocate_roots_and_edges as relocate_forwarded_roots_and_edges,
};
use crate::spaces::{OldGenConfig, OldGenState};
use crate::stats::{CollectionStats, HeapStats};

pub(crate) struct WeakRetention<'a> {
    objects: &'a [ObjectRecord],
    index: &'a ObjectIndex,
    forwarding: &'a ForwardingMap,
    kind: CollectionKind,
}

impl<'a> WeakRetention<'a> {
    pub(crate) fn new(
        objects: &'a [ObjectRecord],
        index: &'a ObjectIndex,
        forwarding: &'a ForwardingMap,
        kind: CollectionKind,
    ) -> Self {
        Self {
            objects,
            index,
            forwarding,
            kind,
        }
    }

    fn record_for(&self, object: GcErased) -> Option<&'a ObjectRecord> {
        self.index
            .get(&object.object_key())
            .map(|&index| &self.objects[index])
    }
}

impl WeakProcessor for WeakRetention<'_> {
    fn remap_or_drop(&mut self, object: GcErased) -> Option<GcErased> {
        if let Some(&forwarded) = self.forwarding.get(&object.object_key()) {
            return Some(forwarded);
        }
        let record = self.record_for(object)?;
        if record.space() == SpaceKind::Immortal {
            return Some(object);
        }
        match self.kind {
            CollectionKind::Minor => {
                (record.space() != SpaceKind::Nursery || record.is_marked()).then_some(object)
            }
            CollectionKind::Major | CollectionKind::Full => record.is_marked().then_some(object),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ParallelWeakShared<'a> {
    objects_ptr: *const ObjectRecord,
    objects_len: usize,
    index_ptr: *const ObjectIndex,
    forwarding_ptr: *const ForwardingMap,
    kind: CollectionKind,
    _marker: PhantomData<&'a ()>,
}

impl<'a> ParallelWeakShared<'a> {
    pub(crate) fn new(
        objects: &'a [ObjectRecord],
        index: &'a ObjectIndex,
        forwarding: &'a ForwardingMap,
        kind: CollectionKind,
    ) -> Self {
        Self {
            objects_ptr: objects.as_ptr(),
            objects_len: objects.len(),
            index_ptr: index as *const _,
            forwarding_ptr: forwarding as *const _,
            kind,
            _marker: PhantomData,
        }
    }

    pub(crate) fn objects(self) -> &'a [ObjectRecord] {
        unsafe { slice::from_raw_parts(self.objects_ptr, self.objects_len) }
    }

    pub(crate) fn processor(self) -> WeakRetention<'a> {
        WeakRetention::new(
            self.objects(),
            unsafe { &*self.index_ptr },
            unsafe { &*self.forwarding_ptr },
            self.kind,
        )
    }
}

// SAFETY: `ParallelWeakShared` is used only during stop-the-world weak processing.
// Workers read a stable liveness/forwarding view and mutate weak slots on disjoint
// object payloads through per-object interior mutability.
unsafe impl Send for ParallelWeakShared<'_> {}
unsafe impl Sync for ParallelWeakShared<'_> {}

pub(crate) struct ForwardingRelocator<'a> {
    forwarding: &'a ForwardingMap,
}

impl<'a> ForwardingRelocator<'a> {
    pub(crate) fn new(forwarding: &'a ForwardingMap) -> Self {
        Self { forwarding }
    }
}

impl Relocator for ForwardingRelocator<'_> {
    fn relocate_erased(&mut self, object: GcErased) -> GcErased {
        self.forwarding
            .get(&object.object_key())
            .copied()
            .unwrap_or(object)
    }
}

pub(crate) struct MajorEphemeronTracer<'a, 'b> {
    tracer: &'a mut MarkTracer<'b>,
    pub(crate) changed: bool,
}

impl<'a, 'b> MajorEphemeronTracer<'a, 'b> {
    pub(crate) fn new(tracer: &'a mut MarkTracer<'b>) -> Self {
        Self {
            tracer,
            changed: false,
        }
    }

    pub(crate) fn finish(self) -> &'a mut MarkTracer<'b> {
        self.tracer
    }
}

impl EphemeronVisitor for MajorEphemeronTracer<'_, '_> {
    fn visit_ephemeron(&mut self, key: GcErased, value: GcErased) {
        let Some(&key_index) = self.tracer.index.get(&key.object_key()) else {
            return;
        };
        if !self.tracer.objects[key_index].is_marked() {
            return;
        }

        let Some(&value_index) = self.tracer.index.get(&value.object_key()) else {
            return;
        };
        let value_record = &self.tracer.objects[value_index];
        if value_record.mark_if_unmarked() {
            self.tracer.worklist.push(value_index);
            self.changed = true;
        }
    }
}

pub(crate) struct MajorMarkSession<'a> {
    objects: &'a [ObjectRecord],
    tracer: MarkTracer<'a>,
    worker_count: usize,
    slice_budget: usize,
    mark_steps: u64,
    mark_rounds: u64,
}

impl<'a> MajorMarkSession<'a> {
    pub(crate) fn new(
        objects: &'a [ObjectRecord],
        index: &'a ObjectIndex,
        worker_count: usize,
        slice_budget: usize,
    ) -> Self {
        Self {
            objects,
            tracer: MarkTracer::new(objects, index),
            worker_count,
            slice_budget,
            mark_steps: 0,
            mark_rounds: 0,
        }
    }

    pub(crate) fn seed(&mut self, root: GcErased) {
        self.tracer.mark_erased(root);
    }

    pub(crate) fn drain_parallel(&mut self) {
        let (steps, rounds) = self
            .tracer
            .drain_parallel_until_empty(self.worker_count, self.slice_budget);
        self.mark_steps = self.mark_steps.saturating_add(steps);
        self.mark_rounds = self.mark_rounds.saturating_add(rounds);
    }

    pub(crate) fn run_ephemeron_fixpoint_parallel(&mut self) {
        loop {
            let changed = if self.worker_count.max(1) == 1 || self.objects.len() <= 1 {
                let mut visitor = MajorEphemeronTracer::new(&mut self.tracer);
                for object in self.objects {
                    if object.is_marked() {
                        object.visit_ephemerons(&mut visitor);
                    }
                }
                let changed = visitor.changed;
                let _tracer = visitor.finish();
                changed
            } else {
                self.scan_ephemerons_parallel()
            };
            let (steps, rounds) = self
                .tracer
                .drain_parallel_until_empty(self.worker_count, self.slice_budget);
            self.mark_steps = self.mark_steps.saturating_add(steps);
            self.mark_rounds = self.mark_rounds.saturating_add(rounds);
            if !changed {
                break;
            }
        }
    }

    fn scan_ephemerons_parallel(&mut self) -> bool {
        let workers = self.worker_count.max(1).min(self.objects.len().max(1));
        let chunk_size = self.objects.len().max(1).div_ceil(workers);
        let shared = ParallelMarkShared::new(self.objects, self.tracer.index);
        let worker_outputs = thread::scope(|scope| {
            let mut handles = Vec::with_capacity(workers);
            for worker_index in 0..workers {
                let start = worker_index.saturating_mul(chunk_size);
                let end = (start + chunk_size).min(self.objects.len());
                if start >= end {
                    continue;
                }
                handles.push(scope.spawn(move || {
                    let mut worker = shared.tracer(MarkWorklist::default());
                    let changed = {
                        let mut visitor = MajorEphemeronTracer::new(&mut worker);
                        for object in &shared.objects()[start..end] {
                            if object.is_marked() {
                                object.visit_ephemerons(&mut visitor);
                            }
                        }
                        visitor.changed
                    };
                    (changed, worker.into_worklist())
                }));
            }

            let mut outputs = Vec::with_capacity(handles.len());
            for handle in handles {
                outputs.push(handle.join().expect("parallel ephemeron worker panicked"));
            }
            outputs
        });

        let mut changed = false;
        for (worker_changed, mut worklist) in worker_outputs {
            changed |= worker_changed;
            self.tracer.worklist.append(&mut worklist);
        }
        changed
    }

    pub(crate) fn mark_steps(&self) -> u64 {
        self.mark_steps
    }

    pub(crate) fn mark_rounds(&self) -> u64 {
        self.mark_rounds
    }
}

pub(crate) fn trace_major(
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    worker_count: usize,
    slice_budget: usize,
    sources: impl IntoIterator<Item = GcErased>,
) -> (u64, u64) {
    let mut session = MajorMarkSession::new(objects, index, worker_count, slice_budget);
    for source in sources {
        session.seed(source);
    }
    session.drain_parallel();
    session.run_ephemeron_fixpoint_parallel();
    (session.mark_steps(), session.mark_rounds())
}

pub(crate) fn collect_global_sources(roots: &RootStack, objects: &[ObjectRecord]) -> Vec<GcErased> {
    roots
        .iter()
        .chain(
            objects
                .iter()
                .filter(|object| object.space() == SpaceKind::Immortal)
                .map(ObjectRecord::erased),
        )
        .collect()
}

/// Round `value` up to the next multiple of `align` (assumed non-zero).
/// Returns `value` unchanged if it is already aligned.
fn align_up_to(value: usize, align: usize) -> usize {
    if align <= 1 {
        return value;
    }
    let rem = value % align;
    if rem == 0 {
        value
    } else {
        value.saturating_add(align - rem)
    }
}

/// Walk every old-gen block, enumerate dirty cards, and gather the
/// `ObjectRecord` indices whose payload base falls inside any dirty
/// card range. The resulting indices are treated as additional minor
/// GC roots so the trace can pull live young targets out of mutated
/// old-gen objects.
///
/// Phase 4 perf: uses the per-block per-card object-start index so the
/// scan walks dirty cards in O(dirty_cards) plus O(objects-in-card)
/// total work, instead of O(blocks * dirty_cards * objects) for the
/// previous linear pass over every record per dirty card. The one-shot
/// header-pointer -> objects-index map is built once at the start of
/// the scan and is amortized over many dirty cards.
pub(crate) fn collect_dirty_card_root_indices(
    objects: &[ObjectRecord],
    old_gen: &OldGenState,
) -> Vec<usize> {
    collect_dirty_card_root_indices_with_counter(objects, old_gen, &mut 0usize)
}

/// Variant of `collect_dirty_card_root_indices` that also writes the
/// number of object-records inspected during the scan into `counter`.
/// Tests use this to assert that the per-card object-start index lets
/// the scan inspect far fewer records than `objects.len()`.
pub(crate) fn collect_dirty_card_root_indices_with_counter(
    objects: &[ObjectRecord],
    old_gen: &OldGenState,
    counter: &mut usize,
) -> Vec<usize> {
    let mut roots = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // One-shot header-pointer -> objects-index map. Built once and
    // shared across every dirty card so the per-card walk only pays a
    // single hash lookup per object header it visits.
    let mut header_index: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::with_capacity(objects.len());
    for (object_index, record) in objects.iter().enumerate() {
        let header_addr = record.erased().header().as_ptr() as usize;
        header_index.insert(header_addr, object_index);
    }

    for block in old_gen.blocks() {
        let card_table = block.card_table();
        let dirty = card_table.dirty_card_indices();
        if dirty.is_empty() {
            continue;
        }
        let card_size = card_table.card_size();
        let block_base = block.base_ptr() as usize;
        let block_len = block.capacity_bytes();
        let line_bytes = block.line_bytes().max(1);
        let object_starts = block.object_starts();
        for card_index in dirty {
            let Some(start_offset) = object_starts.get(card_index).copied().flatten() else {
                continue;
            };
            let card_end_offset = ((card_index + 1) * card_size).min(block_len);
            let mut offset = start_offset as usize;
            while offset < card_end_offset {
                let header_addr = block_base + offset;
                if let Some(&object_index) = header_index.get(&header_addr) {
                    *counter += 1;
                    let record = &objects[object_index];
                    let total_size = record.total_size().max(1);
                    if seen.insert(object_index) {
                        roots.push(object_index);
                    }
                    let next_offset = offset.saturating_add(total_size);
                    // After consuming this object, the next object header
                    // is line-aligned (try_alloc rounds every allocation
                    // up to a whole number of lines). Snap forward to the
                    // next line boundary so we don't waste time scanning
                    // the trailing pad bytes inside the same line.
                    offset = align_up_to(next_offset, line_bytes);
                    continue;
                }
                // No live record at this header address — advance to
                // the next line boundary so we keep making forward
                // progress without skipping over a real header that may
                // sit at the next line start.
                *counter += 1;
                offset = align_up_to(offset.saturating_add(1), line_bytes);
            }
        }
    }
    roots
}

/// After a minor GC clears all dirty cards, walk every block-backed
/// old-gen object whose payload still references a nursery survivor
/// and re-mark its card so the next minor cycle picks the edge up
/// again. This keeps the per-block card table consistent with the
/// surviving old-to-young edges across multiple GC cycles.
pub(crate) fn refresh_block_card_marks_after_minor(
    objects: &[ObjectRecord],
    object_index: &ObjectIndex,
    old_gen: &OldGenState,
) {
    struct NurseryDetectTracer<'a> {
        objects: &'a [ObjectRecord],
        index: &'a ObjectIndex,
        seen_nursery_target: bool,
    }

    impl Tracer for NurseryDetectTracer<'_> {
        fn mark_erased(&mut self, object: GcErased) {
            if self.seen_nursery_target {
                return;
            }
            if let Some(&target_index) = self.index.get(&object.object_key())
                && self.objects[target_index].space() == SpaceKind::Nursery
            {
                self.seen_nursery_target = true;
            }
        }
    }

    for record in objects.iter() {
        if record.old_block_placement().is_none() {
            continue;
        }
        if record.space() == SpaceKind::Nursery || record.space() == SpaceKind::Immortal {
            continue;
        }
        if record.header().is_moved_out() {
            continue;
        }
        let mut tracer = NurseryDetectTracer {
            objects,
            index: object_index,
            seen_nursery_target: false,
        };
        record.trace_edges(&mut tracer);
        if tracer.seen_nursery_target {
            let owner_addr = record.erased().header().as_ptr() as usize;
            old_gen.record_write_barrier(owner_addr);
        }
    }
}

pub(crate) fn trace_collection(
    plan: &CollectionPlan,
    objects: &[ObjectRecord],
    indexes: &HeapIndexState,
    sources: &[GcErased],
    mut record_phase: impl FnMut(CollectionPhase),
) -> (u64, u64) {
    match plan.kind {
        CollectionKind::Minor => trace_minor(
            objects,
            &indexes.object_index,
            &indexes.remembered.owners,
            &indexes.candidate_indices(&indexes.ephemeron_candidates),
            plan.worker_count.max(1),
            plan.mark_slice_budget,
            sources.iter().copied(),
        ),
        CollectionKind::Major | CollectionKind::Full => {
            record_phase(CollectionPhase::InitialMark);
            if plan.concurrent {
                record_phase(CollectionPhase::ConcurrentMark);
            }
            record_phase(CollectionPhase::Remark);
            trace_major(
                objects,
                &indexes.object_index,
                plan.worker_count.max(1),
                plan.mark_slice_budget,
                sources.iter().copied(),
            )
        }
    }
}

pub(crate) fn prepare_major_reclaim_for_plan(
    plan: &CollectionPlan,
    objects: &[ObjectRecord],
    indexes: &HeapIndexState,
    old_gen: &OldGenState,
    old_config: &OldGenConfig,
) -> PreparedReclaim {
    orchestrate_major_reclaim(
        plan,
        |plan| {
            let empty_forwarding = ForwardingMap::default();
            process_weak_references_for_candidates(
                objects,
                &indexes.weak_candidates,
                plan.kind,
                plan.worker_count.max(1),
                &empty_forwarding,
                &indexes.object_index,
            );
        },
        |plan| prepare_reclaim(objects, indexes, old_gen, old_config, plan.kind, plan),
    )
}

pub(crate) fn prepare_full_reclaim_for_plan(
    plan: &CollectionPlan,
    roots: &mut RootStack,
    objects: &mut Vec<ObjectRecord>,
    indexes: &mut HeapIndexState,
    old_gen: &mut OldGenState,
    old_config: &OldGenConfig,
    nursery_config: &NurseryConfig,
    stats: &mut HeapStats,
    nursery: &mut crate::spaces::NurseryState,
    mut record_phase: impl FnMut(CollectionPhase),
) -> Result<PreparedReclaim, AllocError> {
    struct FullReclaimState<'a> {
        roots: &'a mut RootStack,
        objects: &'a mut Vec<ObjectRecord>,
        indexes: &'a mut HeapIndexState,
        old_gen: &'a mut OldGenState,
        old_config: &'a OldGenConfig,
        nursery_config: &'a NurseryConfig,
        stats: &'a mut HeapStats,
        nursery: &'a mut crate::spaces::NurseryState,
    }

    let mut state = FullReclaimState {
        roots,
        objects,
        indexes,
        old_gen,
        old_config,
        nursery_config,
        stats,
        nursery,
    };
    record_phase(CollectionPhase::Evacuate);
    orchestrate_full_reclaim(
        &mut state,
        plan,
        |state| {
            let evacuation = evacuate_nursery_space(
                state.objects,
                state.indexes,
                state.old_gen,
                state.old_config,
                state.nursery_config,
                state.stats,
                state.nursery,
            )?;
            Ok((evacuation.forwarding, evacuation.promoted_bytes))
        },
        |state, forwarding| {
            relocate_forwarded_roots_and_edges(
                state.roots,
                state.objects,
                state.indexes,
                forwarding,
            )
        },
        |state, plan, forwarding| {
            process_weak_references_for_candidates(
                state.objects,
                &state.indexes.weak_candidates,
                plan.kind,
                plan.worker_count.max(1),
                forwarding,
                &state.indexes.object_index,
            );
        },
        |state, plan| {
            prepare_reclaim(
                state.objects,
                state.indexes,
                state.old_gen,
                state.old_config,
                plan.kind,
                plan,
            )
        },
    )
}

pub(crate) fn execute_collection_plan(
    plan: &CollectionPlan,
    roots: &mut RootStack,
    objects: &mut Vec<ObjectRecord>,
    indexes: &mut HeapIndexState,
    old_gen: &mut OldGenState,
    old_config: &OldGenConfig,
    nursery_config: &NurseryConfig,
    stats: &mut HeapStats,
    nursery: &mut crate::spaces::NurseryState,
    runtime_state: &RuntimeStateHandle,
    mut record_phase: impl FnMut(CollectionPhase),
) -> Result<CollectionStats, AllocError> {
    let before_bytes = stats.total_live_bytes();
    for object in objects.iter() {
        object.clear_mark();
    }

    let mut sources = collect_global_sources(roots, objects);
    // Phase 4: dirty-card scan. For minor collections, walk every
    // dirty card in every old-gen block and add the records living in
    // those cards as additional roots so the trace picks up any
    // young-target edges that mutators marked since the last cycle.
    if matches!(plan.kind, CollectionKind::Minor) {
        let dirty_card_root_indices = collect_dirty_card_root_indices(objects, old_gen);
        for object_index in dirty_card_root_indices {
            sources.push(objects[object_index].erased());
        }
    }
    let mark_started_at = Instant::now();
    let (mark_steps, mark_rounds) = trace_collection(plan, objects, indexes, &sources, |phase| {
        record_phase(phase)
    });
    let mark_elapsed_nanos = saturating_duration_nanos(mark_started_at.elapsed());

    match plan.kind {
        CollectionKind::Minor => {
            record_phase(CollectionPhase::Evacuate);
            let evacuation = evacuate_nursery_space(
                objects,
                indexes,
                old_gen,
                old_config,
                nursery_config,
                stats,
                nursery,
            )?;
            relocate_forwarded_roots_and_edges(roots, objects, indexes, &evacuation.forwarding);
            process_weak_references_for_candidates(
                objects,
                &indexes.weak_candidates,
                plan.kind,
                plan.worker_count.max(1),
                &evacuation.forwarding,
                &indexes.object_index,
            );
            record_phase(CollectionPhase::Reclaim);
            let runtime_state_for_callback = runtime_state.clone();
            let rebuild = rebuild_minor_after_collection(
                objects,
                indexes,
                old_gen,
                old_config,
                stats,
                runtime_state,
                plan.kind,
                Some(plan.clone()),
                move |object| runtime_state_for_callback.enqueue_pending_finalizer(object),
            );
            // Phase 4: clear every per-block dirty card now that the
            // minor scan has consumed them, then walk surviving block-
            // backed old-gen objects and re-mark cards for any whose
            // payload still references a nursery survivor. This
            // re-establishes remembered tracking across cycles without
            // requiring the mutator to redirty cards on edges that
            // existed before the GC.
            old_gen.clear_all_dirty_cards();
            refresh_block_card_marks_after_minor(objects, &indexes.object_index, old_gen);
            // Now that dead nursery records are dropped and survivors
            // have been copied into the to-space arena, swap from- and
            // to-spaces so new allocations bump-alloc from the same
            // buffer the survivors now live in, and reset the (now
            // drained) former from-space for the next minor cycle.
            nursery.swap_spaces_and_reset();
            Ok(CollectionStats::completed_minor_cycle(
                mark_steps,
                mark_rounds,
                evacuation.promoted_bytes,
                before_bytes,
                rebuild.after_bytes,
                rebuild.queued_finalizers,
                rebuild.old_region_stats,
            ))
        }
        CollectionKind::Major => {
            let reclaim_prepare_start = Instant::now();
            let prepared_reclaim =
                prepare_major_reclaim_for_plan(plan, objects, indexes, old_gen, old_config);
            record_phase(CollectionPhase::Reclaim);
            let runtime_state_for_callback = runtime_state.clone();
            Ok(finish_prepared_reclaim_cycle(
                objects,
                indexes,
                old_gen,
                stats,
                runtime_state,
                before_bytes,
                mark_steps,
                mark_rounds,
                mark_elapsed_nanos,
                saturating_duration_nanos(reclaim_prepare_start.elapsed()),
                prepared_reclaim,
                move |object| runtime_state_for_callback.enqueue_pending_finalizer(object),
            ))
        }
        CollectionKind::Full => {
            let reclaim_prepare_start = Instant::now();
            let prepared_reclaim = prepare_full_reclaim_for_plan(
                plan,
                roots,
                objects,
                indexes,
                old_gen,
                old_config,
                nursery_config,
                stats,
                nursery,
                &mut record_phase,
            )?;
            record_phase(CollectionPhase::Reclaim);
            let runtime_state_for_callback = runtime_state.clone();
            let cycle = finish_prepared_reclaim_cycle(
                objects,
                indexes,
                old_gen,
                stats,
                runtime_state,
                before_bytes,
                mark_steps,
                mark_rounds,
                mark_elapsed_nanos,
                saturating_duration_nanos(reclaim_prepare_start.elapsed()),
                prepared_reclaim,
                move |object| runtime_state_for_callback.enqueue_pending_finalizer(object),
            );
            // Phase 4: full collection rebuilt the old-gen block pool
            // and the nursery. Clear every dirty card and re-mark
            // surviving block-backed owners that still reference a
            // young object so the next minor cycle starts with a
            // consistent card table.
            old_gen.clear_all_dirty_cards();
            refresh_block_card_marks_after_minor(objects, &indexes.object_index, old_gen);
            // Full collection also evacuates the nursery; swap and
            // reset like a minor does.
            nursery.swap_spaces_and_reset();
            Ok(cycle)
        }
    }
}

fn saturating_duration_nanos(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

pub(crate) fn trace_minor(
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    remembered_owners: &[ObjectKey],
    ephemeron_candidates: &[usize],
    worker_count: usize,
    slice_budget: usize,
    sources: impl IntoIterator<Item = GcErased>,
) -> (u64, u64) {
    let mut tracer = MinorTracer::new(objects, index);
    for source in sources {
        tracer.scan_source(source);
    }

    for &owner in remembered_owners {
        if let Some(&owner_index) = index.get(&owner) {
            tracer.scan_source(objects[owner_index].erased());
        }
    }

    let (mut mark_steps, mut mark_rounds) =
        tracer.drain_parallel_until_empty(worker_count, slice_budget);
    let (ephemeron_steps, ephemeron_rounds) = trace_minor_ephemerons(
        objects,
        ephemeron_candidates,
        &mut tracer,
        worker_count,
        slice_budget,
    );
    mark_steps = mark_steps.saturating_add(ephemeron_steps);
    mark_rounds = mark_rounds.saturating_add(ephemeron_rounds);
    (mark_steps, mark_rounds)
}

pub(crate) struct MinorEphemeronTracer<'a, 'b> {
    tracer: &'a mut MinorTracer<'b>,
    pub(crate) changed: bool,
}

impl<'a, 'b> MinorEphemeronTracer<'a, 'b> {
    pub(crate) fn new(tracer: &'a mut MinorTracer<'b>) -> Self {
        Self {
            tracer,
            changed: false,
        }
    }

    pub(crate) fn finish(self) -> &'a mut MinorTracer<'b> {
        self.tracer
    }
}

impl EphemeronVisitor for MinorEphemeronTracer<'_, '_> {
    fn visit_ephemeron(&mut self, key: GcErased, value: GcErased) {
        let Some(&key_index) = self.tracer.index.get(&key.object_key()) else {
            return;
        };
        let key_record = &self.tracer.objects[key_index];
        let key_is_live = key_record.space() != SpaceKind::Nursery || key_record.is_marked();
        if !key_is_live {
            return;
        }

        let Some(&value_index) = self.tracer.index.get(&value.object_key()) else {
            return;
        };
        let value_record = &self.tracer.objects[value_index];
        if value_record.space() == SpaceKind::Nursery && value_record.mark_if_unmarked() {
            self.tracer.young_worklist.push(value_index);
            self.changed = true;
        }
    }
}

pub(crate) struct MarkTracer<'a> {
    pub(crate) objects: &'a [ObjectRecord],
    pub(crate) index: &'a ObjectIndex,
    pub(crate) worklist: MarkWorklist<usize>,
}

#[derive(Clone, Copy)]
pub(crate) struct ParallelMarkShared<'a> {
    objects_ptr: *const ObjectRecord,
    objects_len: usize,
    index_ptr: *const ObjectIndex,
    _marker: PhantomData<&'a ()>,
}

impl<'a> ParallelMarkShared<'a> {
    pub(crate) fn new(objects: &'a [ObjectRecord], index: &'a ObjectIndex) -> Self {
        Self {
            objects_ptr: objects.as_ptr(),
            objects_len: objects.len(),
            index_ptr: index as *const _,
            _marker: PhantomData,
        }
    }

    pub(crate) fn tracer(self, worklist: MarkWorklist<usize>) -> MarkTracer<'a> {
        MarkTracer::with_worklist(self.objects(), self.index(), worklist)
    }

    pub(crate) fn minor_tracer(self, worklist: MarkWorklist<usize>) -> MinorTracer<'a> {
        MinorTracer::with_worklist(self.objects(), self.index(), worklist)
    }

    pub(crate) fn objects(self) -> &'a [ObjectRecord] {
        unsafe { slice::from_raw_parts(self.objects_ptr, self.objects_len) }
    }

    fn index(self) -> &'a ObjectIndex {
        unsafe { &*self.index_ptr }
    }
}

// SAFETY: `ParallelMarkShared` is only constructed for stop-the-world mark rounds.
// During those rounds, the object graph and index are read-only across workers.
// The only shared mutation is through per-object atomic mark bits, while each
// worker owns a private worklist.
unsafe impl Send for ParallelMarkShared<'_> {}
unsafe impl Sync for ParallelMarkShared<'_> {}

impl<'a> MarkTracer<'a> {
    const SPLIT_THRESHOLD: usize = 32;

    pub(crate) fn new(objects: &'a [ObjectRecord], index: &'a ObjectIndex) -> Self {
        Self {
            objects,
            index,
            worklist: MarkWorklist::default(),
        }
    }

    pub(crate) fn with_worklist(
        objects: &'a [ObjectRecord],
        index: &'a ObjectIndex,
        worklist: MarkWorklist<usize>,
    ) -> Self {
        Self {
            objects,
            index,
            worklist,
        }
    }

    pub(crate) fn into_worklist(self) -> MarkWorklist<usize> {
        self.worklist
    }

    fn mark_index(&mut self, index: usize) {
        let object = &self.objects[index];
        if object.mark_if_unmarked() {
            self.worklist.push(index);
        }
    }

    pub(crate) fn drain_one_slice(&mut self, slice_budget: usize) -> usize {
        let budget = slice_budget.max(1);
        let mut drained = 0usize;

        if self.worklist.len() > Self::SPLIT_THRESHOLD {
            let mut spill = self.worklist.split_half();
            while drained < budget {
                let Some(index) = spill.pop() else {
                    break;
                };
                self.objects[index].trace_edges(self);
                drained += 1;
            }
            while let Some(index) = spill.pop() {
                self.worklist.push(index);
            }
        } else {
            while drained < budget {
                let Some(index) = self.worklist.pop() else {
                    break;
                };
                self.objects[index].trace_edges(self);
                drained += 1;
            }
        }

        drained
    }

    pub(crate) fn drain_worker_round(
        &mut self,
        worker_count: usize,
        slice_budget: usize,
    ) -> (usize, u64) {
        let workers = worker_count.max(1);
        if workers == 1 || self.worklist.len() <= 1 {
            let drained = self.drain_one_slice(slice_budget);
            return (drained, u64::from(drained > 0));
        }

        // Lock-free work-stealing path (Phase 3). The initial worklist is
        // distributed across `workers` crossbeam deques; workers dynamically
        // steal from each other whenever their local deque empties.
        run_stealing_round_major(
            self.objects,
            self.index,
            &mut self.worklist,
            workers,
            slice_budget,
        )
    }

    pub(crate) fn drain_parallel_until_empty(
        &mut self,
        worker_count: usize,
        slice_budget: usize,
    ) -> (u64, u64) {
        let mut slices = 0u64;
        let mut rounds = 0u64;
        while !self.worklist.is_empty() {
            let (_drained_objects, drained_slices) =
                self.drain_worker_round(worker_count, slice_budget);
            if drained_slices > 0 {
                slices = slices.saturating_add(drained_slices);
                rounds = rounds.saturating_add(1);
            }
        }
        (slices, rounds)
    }
}

impl Tracer for MarkTracer<'_> {
    fn mark_erased(&mut self, object: GcErased) {
        if let Some(&index) = self.index.get(&object.object_key()) {
            self.mark_index(index);
        }
    }
}

pub(crate) struct MinorTracer<'a> {
    pub(crate) objects: &'a [ObjectRecord],
    pub(crate) index: &'a ObjectIndex,
    pub(crate) young_worklist: MarkWorklist<usize>,
}

impl<'a> MinorTracer<'a> {
    const SPLIT_THRESHOLD: usize = 32;

    pub(crate) fn new(objects: &'a [ObjectRecord], index: &'a ObjectIndex) -> Self {
        Self {
            objects,
            index,
            young_worklist: MarkWorklist::default(),
        }
    }

    pub(crate) fn with_worklist(
        objects: &'a [ObjectRecord],
        index: &'a ObjectIndex,
        young_worklist: MarkWorklist<usize>,
    ) -> Self {
        Self {
            objects,
            index,
            young_worklist,
        }
    }

    pub(crate) fn into_worklist(self) -> MarkWorklist<usize> {
        self.young_worklist
    }

    pub(crate) fn scan_source(&mut self, object: GcErased) {
        let Some(&index) = self.index.get(&object.object_key()) else {
            return;
        };
        let source = &self.objects[index];
        if source.space() == SpaceKind::Nursery {
            self.mark_young(index);
        } else {
            source.trace_edges(self);
        }
    }

    fn mark_young(&mut self, index: usize) {
        let object = &self.objects[index];
        if object.space() == SpaceKind::Nursery && object.mark_if_unmarked() {
            self.young_worklist.push(index);
        }
    }

    fn drain_one_slice(&mut self, slice_budget: usize) -> usize {
        let budget = slice_budget.max(1);
        let mut drained = 0usize;

        if self.young_worklist.len() > Self::SPLIT_THRESHOLD {
            let mut spill = self.young_worklist.split_half();
            while drained < budget {
                let Some(index) = spill.pop() else {
                    break;
                };
                self.objects[index].trace_edges(self);
                drained += 1;
            }
            while let Some(index) = spill.pop() {
                self.young_worklist.push(index);
            }
        } else {
            while drained < budget {
                let Some(index) = self.young_worklist.pop() else {
                    break;
                };
                self.objects[index].trace_edges(self);
                drained += 1;
            }
        }

        drained
    }

    fn drain_worker_round(&mut self, worker_count: usize, slice_budget: usize) -> (usize, u64) {
        let workers = worker_count.max(1);
        if workers == 1 || self.young_worklist.len() <= 1 {
            let drained = self.drain_one_slice(slice_budget);
            return (drained, u64::from(drained > 0));
        }

        // Lock-free work-stealing path (Phase 3).
        run_stealing_round_minor(
            self.objects,
            self.index,
            &mut self.young_worklist,
            workers,
            slice_budget,
        )
    }

    pub(crate) fn drain_parallel_until_empty(
        &mut self,
        worker_count: usize,
        slice_budget: usize,
    ) -> (u64, u64) {
        let mut slices = 0u64;
        let mut rounds = 0u64;
        while !self.young_worklist.is_empty() {
            let (_drained_objects, drained_slices) =
                self.drain_worker_round(worker_count, slice_budget);
            if drained_slices > 0 {
                slices = slices.saturating_add(drained_slices);
                rounds = rounds.saturating_add(1);
            }
        }
        (slices, rounds)
    }
}

impl Tracer for MinorTracer<'_> {
    fn mark_erased(&mut self, object: GcErased) {
        let Some(&index) = self.index.get(&object.object_key()) else {
            return;
        };
        if self.objects[index].space() == SpaceKind::Nursery {
            self.mark_young(index);
        }
    }
}

// ---------------------------------------------------------------------------
// Lock-free work-stealing mark workers (Phase 3)
// ---------------------------------------------------------------------------
//
// The stealing tracers wrap a `crossbeam_deque::Worker<usize>` and implement
// the `Tracer` trait. During tracing they push freshly marked indices to their
// own local deque. When a worker's local queue empties, it tries to steal from
// sibling workers' stealers until it either finds work or observes a full
// quiescent round across all siblings.
//
// Termination rule used by `run_stealing_round`:
//   - A worker drains its local queue.
//   - When empty, it performs one full pass over all siblings trying to steal.
//   - If no work was found in that pass AND its local queue is still empty,
//     the worker exits.
//   - Any items that remain in a worker's local deque at exit (shouldn't
//     happen in practice) are drained and returned to the caller so the outer
//     `drain_parallel_until_empty` loop can pick them up on a subsequent round.
//
// Correctness relies on the fact that new work can only enter a worker's local
// deque from that same worker's tracing calls. Once a worker's local deque is
// empty AND one full steal pass finds nothing anywhere, the total work in the
// system has reached zero and no future push can happen without further
// tracing — which also requires new input to some worker's queue.
//
// The existing outer loop in `drain_parallel_until_empty` already handles
// "one round left something behind" by re-entering `drain_worker_round`, so
// partial progress is safe even if termination is imperfect.

/// Stealing tracer for major (full heap) marking. Pushes newly marked indices
/// to a local `crossbeam_deque::Worker<usize>` deque.
pub(crate) struct StealingMarkTracer<'a> {
    objects: &'a [ObjectRecord],
    index: &'a ObjectIndex,
    worker: Worker<usize>,
}

impl<'a> StealingMarkTracer<'a> {
    fn new(objects: &'a [ObjectRecord], index: &'a ObjectIndex, worker: Worker<usize>) -> Self {
        Self {
            objects,
            index,
            worker,
        }
    }

    fn mark_index(&mut self, index: usize) {
        let object = &self.objects[index];
        if object.mark_if_unmarked() {
            self.worker.push(index);
        }
    }

    fn trace_one(&mut self, index: usize) {
        self.objects[index].trace_edges(self);
    }

    fn into_remainder(self) -> Vec<usize> {
        let Self { worker, .. } = self;
        drain_worker_remainder(&worker)
    }
}

impl Tracer for StealingMarkTracer<'_> {
    fn mark_erased(&mut self, object: GcErased) {
        if let Some(&index) = self.index.get(&object.object_key()) {
            self.mark_index(index);
        }
    }
}

/// Stealing tracer for minor (nursery-only) marking.
pub(crate) struct StealingMinorTracer<'a> {
    objects: &'a [ObjectRecord],
    index: &'a ObjectIndex,
    worker: Worker<usize>,
}

impl<'a> StealingMinorTracer<'a> {
    fn new(objects: &'a [ObjectRecord], index: &'a ObjectIndex, worker: Worker<usize>) -> Self {
        Self {
            objects,
            index,
            worker,
        }
    }

    fn mark_young(&mut self, index: usize) {
        let object = &self.objects[index];
        if object.space() == SpaceKind::Nursery && object.mark_if_unmarked() {
            self.worker.push(index);
        }
    }

    fn trace_one(&mut self, index: usize) {
        self.objects[index].trace_edges(self);
    }

    fn into_remainder(self) -> Vec<usize> {
        let Self { worker, .. } = self;
        drain_worker_remainder(&worker)
    }
}

impl Tracer for StealingMinorTracer<'_> {
    fn mark_erased(&mut self, object: GcErased) {
        let Some(&index) = self.index.get(&object.object_key()) else {
            return;
        };
        if self.objects[index].space() == SpaceKind::Nursery {
            self.mark_young(index);
        }
    }
}

fn drain_worker_remainder(worker: &Worker<usize>) -> Vec<usize> {
    let mut remainder = Vec::new();
    while let Some(value) = worker.pop() {
        remainder.push(value);
    }
    remainder
}

/// Split a worklist into N LIFO workers, distributing items round-robin.
///
/// The initial distribution is simple round-robin over the drained worklist.
/// Work stealing balances the rest during the round, so the initial split
/// doesn't need to be perfectly even.
fn distribute_into_workers(
    initial: &mut MarkWorklist<usize>,
    worker_count: usize,
) -> Vec<Worker<usize>> {
    let workers: Vec<Worker<usize>> = (0..worker_count).map(|_| Worker::new_lifo()).collect();
    let mut slot = 0usize;
    while let Some(value) = initial.pop() {
        workers[slot].push(value);
        slot = (slot + 1) % worker_count;
    }
    workers
}

/// Steal one value from any sibling stealer. Returns the stolen value and a
/// flag indicating whether the caller should retry because a sibling reported
/// `Steal::Retry`.
fn try_steal_from_siblings(
    worker_idx: usize,
    stealers: &[Stealer<usize>],
) -> (Option<usize>, bool) {
    let mut should_retry = false;
    for (sibling_idx, stealer) in stealers.iter().enumerate() {
        if sibling_idx == worker_idx {
            continue;
        }
        match stealer.steal() {
            Steal::Empty => continue,
            Steal::Retry => {
                should_retry = true;
                continue;
            }
            Steal::Success(value) => return (Some(value), false),
        }
    }
    (None, should_retry)
}

/// Run one work-stealing mark round for major (full heap) marking.
///
/// Each worker drains up to `slice_budget` items, stealing from siblings
/// whenever its local deque empties. Once a worker has drained its full
/// slice budget, it stops and returns its remaining queue. Any leftover
/// items are pushed back into `initial` so the outer drain loop can re-enter
/// for the next round.
///
/// This preserves the "per-round total work ≤ worker_count * slice_budget"
/// semantic that the outer loop relies on for pause-time budgeting.
fn run_stealing_round_major(
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    initial: &mut MarkWorklist<usize>,
    worker_count: usize,
    slice_budget: usize,
) -> (usize, u64) {
    let worker_count = worker_count.max(1);
    let slice_budget = slice_budget.max(1);
    let workers = distribute_into_workers(initial, worker_count);
    let stealers: Arc<[Stealer<usize>]> = workers
        .iter()
        .map(|w| w.stealer())
        .collect::<Vec<_>>()
        .into();

    let shared = ParallelMarkShared::new(objects, index);
    let outputs: Vec<(usize, Vec<usize>)> = thread::scope(|scope| {
        let mut handles = Vec::with_capacity(worker_count);
        for (worker_idx, worker) in workers.into_iter().enumerate() {
            let stealers = Arc::clone(&stealers);
            handles.push(scope.spawn(move || {
                let mut tracer = StealingMarkTracer::new(shared.objects(), shared.index(), worker);
                let mut drained = 0usize;
                'outer: while drained < slice_budget {
                    // Drain local queue until we hit the budget or run out.
                    while drained < slice_budget {
                        let Some(next) = tracer.worker.pop() else {
                            break;
                        };
                        tracer.trace_one(next);
                        drained += 1;
                    }
                    if drained >= slice_budget {
                        break;
                    }
                    // Local queue empty — try to steal from siblings.
                    let (stolen, should_retry) =
                        try_steal_from_siblings(worker_idx, &stealers);
                    match stolen {
                        Some(value) => {
                            tracer.worker.push(value);
                            continue;
                        }
                        None if should_retry => {
                            std::thread::yield_now();
                            continue;
                        }
                        None => break 'outer,
                    }
                }
                (drained, tracer.into_remainder())
            }));
        }
        handles
            .into_iter()
            .map(|h| h.join().expect("parallel stealing major mark worker panicked"))
            .collect()
    });

    let mut total_drained = 0usize;
    let mut drained_slices = 0u64;
    for (drained, remainder) in outputs {
        if drained > 0 {
            total_drained = total_drained.saturating_add(drained);
            drained_slices = drained_slices.saturating_add(1);
        }
        for value in remainder {
            initial.push(value);
        }
    }
    (total_drained, drained_slices)
}

/// Run one work-stealing mark round for minor (nursery-only) marking.
fn run_stealing_round_minor(
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    initial: &mut MarkWorklist<usize>,
    worker_count: usize,
    slice_budget: usize,
) -> (usize, u64) {
    let worker_count = worker_count.max(1);
    let slice_budget = slice_budget.max(1);
    let workers = distribute_into_workers(initial, worker_count);
    let stealers: Arc<[Stealer<usize>]> = workers
        .iter()
        .map(|w| w.stealer())
        .collect::<Vec<_>>()
        .into();

    let shared = ParallelMarkShared::new(objects, index);
    let outputs: Vec<(usize, Vec<usize>)> = thread::scope(|scope| {
        let mut handles = Vec::with_capacity(worker_count);
        for (worker_idx, worker) in workers.into_iter().enumerate() {
            let stealers = Arc::clone(&stealers);
            handles.push(scope.spawn(move || {
                let mut tracer = StealingMinorTracer::new(shared.objects(), shared.index(), worker);
                let mut drained = 0usize;
                'outer: while drained < slice_budget {
                    while drained < slice_budget {
                        let Some(next) = tracer.worker.pop() else {
                            break;
                        };
                        tracer.trace_one(next);
                        drained += 1;
                    }
                    if drained >= slice_budget {
                        break;
                    }
                    let (stolen, should_retry) =
                        try_steal_from_siblings(worker_idx, &stealers);
                    match stolen {
                        Some(value) => {
                            tracer.worker.push(value);
                            continue;
                        }
                        None if should_retry => {
                            std::thread::yield_now();
                            continue;
                        }
                        None => break 'outer,
                    }
                }
                (drained, tracer.into_remainder())
            }));
        }
        handles
            .into_iter()
            .map(|h| h.join().expect("parallel stealing minor mark worker panicked"))
            .collect()
    });

    let mut total_drained = 0usize;
    let mut drained_slices = 0u64;
    for (drained, remainder) in outputs {
        if drained > 0 {
            total_drained = total_drained.saturating_add(drained);
            drained_slices = drained_slices.saturating_add(1);
        }
        for value in remainder {
            initial.push(value);
        }
    }
    (total_drained, drained_slices)
}

pub(crate) fn trace_major_ephemerons(
    objects: &[ObjectRecord],
    ephemeron_candidates: &[usize],
    tracer: &mut MarkTracer<'_>,
    worker_count: usize,
    slice_budget: usize,
) -> (u64, u64) {
    let mut mark_steps = 0u64;
    let mut mark_rounds = 0u64;
    loop {
        let mut visitor = MajorEphemeronTracer::new(tracer);
        for &index in ephemeron_candidates {
            let object = &objects[index];
            if object.is_marked() {
                object.visit_ephemerons(&mut visitor);
            }
        }
        let changed = visitor.changed;
        let tracer = visitor.finish();
        let (steps, rounds) = tracer.drain_parallel_until_empty(worker_count.max(1), slice_budget);
        mark_steps = mark_steps.saturating_add(steps);
        mark_rounds = mark_rounds.saturating_add(rounds);
        if !changed {
            break;
        }
    }
    (mark_steps, mark_rounds)
}

pub(crate) fn trace_major_ephemerons_for_candidates(
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    ephemeron_candidates: &[ObjectKey],
    tracer: &mut MarkTracer<'_>,
    worker_count: usize,
    slice_budget: usize,
) -> (u64, u64) {
    let ephemeron_candidate_indices = ephemeron_candidates
        .iter()
        .filter_map(|key| index.get(key).copied())
        .collect::<Vec<_>>();
    trace_major_ephemerons(
        objects,
        &ephemeron_candidate_indices,
        tracer,
        worker_count,
        slice_budget,
    )
}

pub(crate) fn trace_minor_ephemerons(
    objects: &[ObjectRecord],
    ephemeron_candidates: &[usize],
    tracer: &mut MinorTracer<'_>,
    worker_count: usize,
    slice_budget: usize,
) -> (u64, u64) {
    let mut mark_steps = 0u64;
    let mut mark_rounds = 0u64;
    loop {
        let changed = if worker_count.max(1) == 1 || objects.len() <= 1 {
            let mut visitor = MinorEphemeronTracer::new(tracer);
            for object in objects {
                let survives = object.space() != SpaceKind::Nursery || object.is_marked();
                if survives {
                    object.visit_ephemerons(&mut visitor);
                }
            }
            let changed = visitor.changed;
            let _tracer = visitor.finish();
            changed
        } else {
            scan_minor_ephemerons_parallel(objects, ephemeron_candidates, tracer, worker_count)
        };
        let (steps, rounds) = tracer.drain_parallel_until_empty(worker_count, slice_budget);
        mark_steps = mark_steps.saturating_add(steps);
        mark_rounds = mark_rounds.saturating_add(rounds);
        if !changed {
            break;
        }
    }
    (mark_steps, mark_rounds)
}

fn scan_minor_ephemerons_parallel(
    objects: &[ObjectRecord],
    ephemeron_candidates: &[usize],
    tracer: &mut MinorTracer<'_>,
    worker_count: usize,
) -> bool {
    let ephemeron_candidates = Arc::new(ephemeron_candidates.to_vec());
    let workers = worker_count.max(1).min(ephemeron_candidates.len().max(1));
    let chunk_size = ephemeron_candidates.len().max(1).div_ceil(workers);
    let shared = ParallelMarkShared::new(objects, tracer.index);
    let worker_outputs = thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for worker_index in 0..workers {
            let ephemeron_candidates = Arc::clone(&ephemeron_candidates);
            let start = worker_index.saturating_mul(chunk_size);
            let end = (start + chunk_size).min(ephemeron_candidates.len());
            if start >= end {
                continue;
            }
            handles.push(scope.spawn(move || {
                let mut worker = shared.minor_tracer(MarkWorklist::default());
                let changed = {
                    let mut visitor = MinorEphemeronTracer::new(&mut worker);
                    for &candidate_index in &ephemeron_candidates[start..end] {
                        let object = &shared.objects()[candidate_index];
                        let survives = object.space() != SpaceKind::Nursery || object.is_marked();
                        if survives {
                            object.visit_ephemerons(&mut visitor);
                        }
                    }
                    visitor.changed
                };
                (changed, worker.into_worklist())
            }));
        }

        let mut outputs = Vec::with_capacity(handles.len());
        for handle in handles {
            outputs.push(
                handle
                    .join()
                    .expect("parallel minor ephemeron worker panicked"),
            );
        }
        outputs
    });

    let mut changed = false;
    for (worker_changed, mut worklist) in worker_outputs {
        changed |= worker_changed;
        tracer.young_worklist.append(&mut worklist);
    }
    changed
}

pub(crate) fn process_weak_references(
    objects: &[ObjectRecord],
    weak_candidates: &[usize],
    kind: CollectionKind,
    worker_count: usize,
    forwarding: &ForwardingMap,
    index: &ObjectIndex,
) {
    let weak_candidates = Arc::new(weak_candidates.to_vec());
    let worker_count = worker_count.max(1);
    if worker_count == 1 || weak_candidates.len() <= 1 {
        let mut processor = WeakRetention::new(objects, index, forwarding, kind);
        for &index in weak_candidates.iter() {
            let object = &objects[index];
            if survives_collection_kind(kind, object) {
                object.process_weak_edges(&mut processor);
            }
        }
        return;
    }

    let workers = worker_count.min(weak_candidates.len().max(1));
    let chunk_size = weak_candidates.len().max(1).div_ceil(workers);
    let shared = ParallelWeakShared::new(objects, index, forwarding, kind);
    thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for worker_index in 0..workers {
            let weak_candidates = Arc::clone(&weak_candidates);
            let start = worker_index.saturating_mul(chunk_size);
            let end = (start + chunk_size).min(weak_candidates.len());
            if start >= end {
                continue;
            }
            handles.push(scope.spawn(move || {
                let mut processor = shared.processor();
                for &candidate_index in &weak_candidates[start..end] {
                    let object = &shared.objects()[candidate_index];
                    if survives_collection_kind(kind, object) {
                        object.process_weak_edges(&mut processor);
                    }
                }
            }));
        }
        for handle in handles {
            handle.join().expect("parallel weak worker panicked");
        }
    });
}

pub(crate) fn process_weak_references_for_candidates(
    objects: &[ObjectRecord],
    weak_candidates: &[ObjectKey],
    kind: CollectionKind,
    worker_count: usize,
    forwarding: &ForwardingMap,
    index: &ObjectIndex,
) {
    let weak_candidate_indices = weak_candidates
        .iter()
        .filter_map(|key| index.get(key).copied())
        .collect::<Vec<_>>();
    process_weak_references(
        objects,
        &weak_candidate_indices,
        kind,
        worker_count,
        forwarding,
        index,
    );
}

fn survives_collection_kind(kind: CollectionKind, object: &ObjectRecord) -> bool {
    if object.space() == SpaceKind::Immortal {
        return true;
    }
    match kind {
        CollectionKind::Minor => object.space() != SpaceKind::Nursery || object.is_marked(),
        CollectionKind::Major | CollectionKind::Full => object.is_marked(),
    }
}

#[cfg(test)]
#[path = "collector_exec_test.rs"]
mod tests;
