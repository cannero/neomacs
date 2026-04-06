use core::marker::PhantomData;
use core::slice;
use std::sync::Arc;
use std::thread;

use crate::descriptor::{EphemeronVisitor, GcErased, ObjectKey, Relocator, Tracer, WeakProcessor};
use crate::index_state::{ForwardingMap, ObjectIndex};
use crate::mark::MarkWorklist;
use crate::object::{ObjectRecord, SpaceKind};
use crate::plan::CollectionKind;
use crate::root::RootStack;

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
        let Some(record) = self.record_for(object) else {
            return None;
        };
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
                let shared = shared;
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

        let mut worker_lists = vec![core::mem::take(&mut self.worklist)];
        while worker_lists.len() < workers {
            let Some((split_index, split_len)) = worker_lists
                .iter()
                .enumerate()
                .map(|(index, list)| (index, list.len()))
                .max_by_key(|(_, len)| *len)
            else {
                break;
            };
            if split_len <= 1 {
                break;
            }
            let stolen = worker_lists[split_index].split_half();
            worker_lists.push(stolen);
        }

        if worker_lists.len() == 1 {
            let mut only_worker = MarkTracer::with_worklist(
                self.objects,
                self.index,
                worker_lists.pop().expect("single worker list"),
            );
            let drained = only_worker.drain_one_slice(slice_budget);
            self.worklist = only_worker.into_worklist();
            return (drained, u64::from(drained > 0));
        }

        let mut drained_objects = 0usize;
        let mut drained_slices = 0u64;
        let shared = ParallelMarkShared::new(self.objects, self.index);
        let worker_outputs = thread::scope(|scope| {
            let mut handles = Vec::with_capacity(worker_lists.len());
            for worker_list in worker_lists {
                let shared = shared;
                handles.push(scope.spawn(move || {
                    let mut worker = shared.tracer(worker_list);
                    let drained = worker.drain_one_slice(slice_budget);
                    let remainder = worker.into_worklist();
                    (drained, remainder)
                }));
            }

            let mut outputs = Vec::with_capacity(handles.len());
            for handle in handles {
                outputs.push(handle.join().expect("parallel mark worker panicked"));
            }
            outputs
        });

        for (drained, mut remainder) in worker_outputs {
            if drained > 0 {
                drained_objects = drained_objects.saturating_add(drained);
                drained_slices = drained_slices.saturating_add(1);
            }
            self.worklist.append(&mut remainder);
        }

        (drained_objects, drained_slices)
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

        let mut worker_lists = vec![core::mem::take(&mut self.young_worklist)];
        while worker_lists.len() < workers {
            let Some((split_index, split_len)) = worker_lists
                .iter()
                .enumerate()
                .map(|(index, list)| (index, list.len()))
                .max_by_key(|(_, len)| *len)
            else {
                break;
            };
            if split_len <= 1 {
                break;
            }
            let stolen = worker_lists[split_index].split_half();
            worker_lists.push(stolen);
        }

        if worker_lists.len() == 1 {
            let mut only_worker = MinorTracer::with_worklist(
                self.objects,
                self.index,
                worker_lists.pop().expect("single worker list"),
            );
            let drained = only_worker.drain_one_slice(slice_budget);
            self.young_worklist = only_worker.into_worklist();
            return (drained, u64::from(drained > 0));
        }

        let shared = ParallelMarkShared::new(self.objects, self.index);
        let worker_outputs = thread::scope(|scope| {
            let mut handles = Vec::with_capacity(worker_lists.len());
            for worker_list in worker_lists {
                let shared = shared;
                handles.push(scope.spawn(move || {
                    let mut worker = shared.minor_tracer(worker_list);
                    let drained = worker.drain_one_slice(slice_budget);
                    let remainder = worker.into_worklist();
                    (drained, remainder)
                }));
            }

            let mut outputs = Vec::with_capacity(handles.len());
            for handle in handles {
                outputs.push(handle.join().expect("parallel minor worker panicked"));
            }
            outputs
        });

        let mut drained_objects = 0usize;
        let mut drained_slices = 0u64;
        for (drained, mut remainder) in worker_outputs {
            if drained > 0 {
                drained_objects = drained_objects.saturating_add(drained);
                drained_slices = drained_slices.saturating_add(1);
            }
            self.young_worklist.append(&mut remainder);
        }

        (drained_objects, drained_slices)
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
            let shared = shared;
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
            let shared = shared;
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
