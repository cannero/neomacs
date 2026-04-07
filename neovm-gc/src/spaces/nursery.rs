use std::collections::HashMap;
use std::thread;

use crate::collector_exec::ForwardingRelocator;
use crate::descriptor::{MovePolicy, Relocator};
use crate::heap::AllocError;
use crate::index_state::{ForwardingMap, HeapIndexState};
use crate::object::{ObjectRecord, SpaceKind};
use crate::root::RootStack;
use crate::spaces::nursery_arena::{NurseryState, WorkerEvacuationArena};
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
    nursery: &mut NurseryState,
) -> Result<EvacuationOutcome, AllocError> {
    if nursery_config.parallel_minor_workers > 1 && objects.len() > 1 {
        return evacuate_marked_nursery_parallel(
            objects,
            indexes,
            old_gen,
            old_config,
            nursery_config,
            stats,
            nursery,
        );
    }
    evacuate_marked_nursery_serial(
        objects,
        indexes,
        old_gen,
        old_config,
        nursery_config,
        stats,
        nursery,
    )
}

fn evacuate_marked_nursery_serial(
    objects: &mut Vec<ObjectRecord>,
    indexes: &mut HeapIndexState,
    old_gen: &mut OldGenState,
    old_config: &OldGenConfig,
    nursery_config: &NurseryConfig,
    stats: &mut HeapStats,
    nursery: &mut NurseryState,
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
            // Survivors that remain in the nursery are copied into the
            // to-space arena via bump allocation. Promotions into the old
            // generation route through the block allocator first so the
            // physical bytes live inside an `OldBlock`. Both fall back to
            // a system allocation if the relevant arena/block pool can't
            // service the layout, mirroring the direct-allocation path.
            let new_record = if target_space == SpaceKind::Nursery {
                let layout = core::alloc::Layout::from_size_align(
                    object.total_size(),
                    object.layout_align(),
                )
                .map_err(|_| AllocError::LayoutOverflow)?;
                match nursery.try_alloc_in_to_space(layout) {
                    Some(base) => unsafe {
                        object.evacuate_to_arena_slot(target_space, base)?
                    },
                    None => object.evacuate_to_space(target_space)?,
                }
            } else if target_space == SpaceKind::Old {
                let layout = core::alloc::Layout::from_size_align(
                    object.total_size(),
                    object.layout_align(),
                )
                .map_err(|_| AllocError::LayoutOverflow)?;
                match old_gen.try_alloc_in_block(old_config, layout) {
                    Some((placement, base)) => {
                        let mut record = unsafe {
                            object.evacuate_to_arena_slot(target_space, base)?
                        };
                        record.set_old_block_placement(placement);
                        record
                    }
                    None => object.evacuate_to_space(target_space)?,
                }
            } else {
                object.evacuate_to_space(target_space)?
            };
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

/// Per-worker output of the parallel nursery evacuation phase.
#[derive(Default)]
struct EvacuationWorkerOutput {
    /// `(ObjectRecord, target_space)` pairs the worker successfully
    /// evacuated. For survivors targeted at the old gen, the placement
    /// has not yet been recorded — that happens serially after join.
    survivors: Vec<(ObjectRecord, SpaceKind)>,
    /// Forwarding entries to merge into the global forwarding map.
    forwarding: Vec<(crate::descriptor::ObjectKey, crate::descriptor::GcErased)>,
}

// Shared read-only view of the source object slice that is broadcast
// across evacuation worker threads. Each worker only reads from a
// disjoint chunk and only writes to its own per-worker arena slab plus
// its own local output Vecs, so there is no inter-worker shared
// mutable state.
#[derive(Clone, Copy)]
struct ParallelEvacShared<'a> {
    objects_ptr: *const ObjectRecord,
    objects_len: usize,
    nursery_config: &'a NurseryConfig,
}

// Safety: each worker reads object slots from a disjoint index range
// and only mutates per-thread state (its arena slab + local Vecs).
// The CAS forwarding install on `ObjectHeader` is the only shared
// mutation and it uses atomic operations.
unsafe impl Send for ParallelEvacShared<'_> {}
unsafe impl Sync for ParallelEvacShared<'_> {}

impl<'a> ParallelEvacShared<'a> {
    fn new(objects: &'a [ObjectRecord], nursery_config: &'a NurseryConfig) -> Self {
        Self {
            objects_ptr: objects.as_ptr(),
            objects_len: objects.len(),
            nursery_config,
        }
    }

    fn objects(&self) -> &'a [ObjectRecord] {
        unsafe { core::slice::from_raw_parts(self.objects_ptr, self.objects_len) }
    }
}

fn evacuate_marked_nursery_parallel(
    objects: &mut Vec<ObjectRecord>,
    indexes: &mut HeapIndexState,
    old_gen: &mut OldGenState,
    old_config: &OldGenConfig,
    nursery_config: &NurseryConfig,
    stats: &mut HeapStats,
    nursery: &mut NurseryState,
) -> Result<EvacuationOutcome, AllocError> {
    let worker_count = nursery_config
        .parallel_minor_workers
        .max(1)
        .min(objects.len().max(1));
    if worker_count <= 1 {
        return evacuate_marked_nursery_serial(
            objects,
            indexes,
            old_gen,
            old_config,
            nursery_config,
            stats,
            nursery,
        );
    }

    let total = objects.len();
    let chunk_size = total.div_ceil(worker_count);
    let worker_arenas = nursery.split_to_space_into_worker_arenas(worker_count);

    let shared = ParallelEvacShared::new(objects.as_slice(), nursery_config);

    // Each worker returns its evacuation output and the (now-mutated)
    // arena it bump-allocated into so the main thread can fold the
    // per-worker cursors back into the unified to-space cursor.
    type WorkerResult = Result<(EvacuationWorkerOutput, WorkerEvacuationArena), AllocError>;
    let worker_results: Vec<WorkerResult> = thread::scope(|scope| {
        let mut handles = Vec::with_capacity(worker_count);
        for (worker_index, arena) in worker_arenas.into_iter().enumerate() {
            let shared = shared;
            let start = worker_index.saturating_mul(chunk_size);
            let end = start.saturating_add(chunk_size).min(total);
            handles.push(scope.spawn(move || -> WorkerResult {
                let mut arena = arena;
                let mut output = EvacuationWorkerOutput::default();
                if start < end {
                    evacuate_chunk(shared, start, end, &mut arena, &mut output)?;
                }
                Ok((output, arena))
            }));
        }
        handles
            .into_iter()
            .map(|h| h.join().expect("parallel nursery evacuation worker panicked"))
            .collect()
    });

    let mut survivors: Vec<(ObjectRecord, SpaceKind)> = Vec::new();
    let mut forwarding = HashMap::new();
    let mut merged_arenas: Vec<WorkerEvacuationArena> = Vec::with_capacity(worker_count);
    for result in worker_results {
        let (output, arena) = result?;
        for (key, erased) in output.forwarding {
            forwarding.insert(key, erased);
        }
        survivors.extend(output.survivors);
        merged_arenas.push(arena);
    }
    nursery.merge_worker_arenas(&merged_arenas);

    // Serial promotion bookkeeping for survivors targeted at the old
    // generation. The system-allocated copy itself was already
    // performed in the worker, but `OldGenState::allocate_placement`
    // and `record_object` mutate shared state and run on the main
    // thread.
    let mut promoted_bytes = 0usize;
    let mut records = Vec::with_capacity(survivors.len());
    for (mut new_record, target_space) in survivors {
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

fn evacuate_chunk(
    shared: ParallelEvacShared<'_>,
    start: usize,
    end: usize,
    arena: &mut WorkerEvacuationArena,
    output: &mut EvacuationWorkerOutput,
) -> Result<(), AllocError> {
    let objects = shared.objects();
    let nursery_config = shared.nursery_config;
    for object in &objects[start..end] {
        if object.space() != SpaceKind::Nursery || !object.is_marked() {
            continue;
        }
        let target_space = target_space_for_survivor(
            object.header().desc().move_policy,
            object.header().age(),
            nursery_config.promotion_age,
        );
        let new_record = if target_space == SpaceKind::Nursery {
            let layout = core::alloc::Layout::from_size_align(
                object.total_size(),
                object.layout_align(),
            )
            .map_err(|_| AllocError::LayoutOverflow)?;
            match arena.try_alloc(layout) {
                Some(base) => {
                    let candidate = unsafe {
                        object.try_evacuate_to_arena_slot(target_space, base)?
                    };
                    match candidate {
                        Some(record) => record,
                        None => {
                            // CAS lost: another worker already
                            // evacuated this object. Skip — the
                            // winning worker will publish the
                            // forwarding entry and ObjectRecord.
                            continue;
                        }
                    }
                }
                None => {
                    // Slab exhausted: fall back to a system
                    // allocation so the copy still succeeds. The
                    // serial path uses the same fallback when the
                    // unified to-space cursor would overflow.
                    object.evacuate_to_space(target_space)?
                }
            }
        } else {
            // Old / pinned promotions go through the system
            // allocator. The actual placement bookkeeping
            // (allocate_placement / record_object) is deferred to
            // the main thread because OldGenState is not
            // partitioned across workers.
            object.evacuate_to_space(target_space)?
        };
        new_record.set_marked(true);
        output
            .forwarding
            .push((object.object_key(), new_record.erased()));
        output.survivors.push((new_record, target_space));
    }
    Ok(())
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
