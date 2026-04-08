use std::sync::{Arc, Mutex, MutexGuard, TryLockError, TryLockResult};

use crate::object::{OldBlockPlacement, PendingFinalizer};
use crate::plan::RuntimeWorkStatus;
use crate::stats::HeapStats;

#[derive(Debug, Default)]
pub(crate) struct RuntimeState {
    pending_finalizers: Vec<PendingFinalizer>,
    finalizers_run: u64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RuntimeStateHandle {
    state: Arc<Mutex<RuntimeState>>,
}

impl RuntimeStateHandle {
    pub(crate) fn lock(&self) -> MutexGuard<'_, RuntimeState> {
        self.state
            .lock()
            .expect("runtime state should not be poisoned")
    }

    pub(crate) fn try_lock(&self) -> TryLockResult<MutexGuard<'_, RuntimeState>> {
        self.state.try_lock()
    }

    pub(crate) fn snapshot(&self) -> (u64, usize) {
        self.lock().snapshot()
    }

    pub(crate) fn pending_finalizer_count(&self) -> usize {
        self.lock().pending_finalizer_count()
    }

    pub(crate) fn runtime_work_status(&self) -> RuntimeWorkStatus {
        RuntimeWorkStatus::from_pending_finalizers(self.pending_finalizer_count())
    }

    pub(crate) fn apply_runtime_stats(&self, stats: &mut HeapStats) {
        let (finalizers_run, pending_finalizers) = self.snapshot();
        stats.finalizers_run = finalizers_run;
        stats.pending_finalizers = pending_finalizers;
    }

    pub(crate) fn enqueue_pending_finalizer(&self, pending: PendingFinalizer) -> u64 {
        self.lock().enqueue_pending_finalizer(pending)
    }

    /// Snapshot the `OldBlockPlacement`s of every pending finalizer record.
    /// Used by the post-sweep block reclaim path so that blocks pinned by
    /// queued finalizers stay live even though their owning record is no
    /// longer in the main `objects` vector.
    pub(crate) fn snapshot_pending_finalizer_block_placements(&self) -> Vec<OldBlockPlacement> {
        let state = self.lock();
        state
            .pending_finalizers
            .iter()
            .filter_map(|pending| pending.block_placement())
            .collect()
    }

    /// Apply a `(old block index -> new block index)` remap to every
    /// pending finalizer's `OldBlockPlacement`. Used after empty-block
    /// reclaim renumbers the surviving blocks.
    pub(crate) fn rebind_pending_finalizer_block_indices(&self, remap: &[Option<usize>]) {
        let mut state = self.lock();
        for pending in state.pending_finalizers.iter_mut() {
            let Some(placement) = pending.block_placement() else {
                continue;
            };
            let Some(&Some(new_index)) = remap.get(placement.block_index) else {
                continue;
            };
            if new_index == placement.block_index {
                continue;
            }
            pending.rebind_block(new_index);
        }
    }

    pub(crate) fn drain_pending_finalizers(&self) -> u64 {
        // Take the pending list out of the state under the lock, then
        // release the lock before running user-defined finalizer code.
        //
        // Holding the lock across `run()` would be a reentry deadlock
        // risk: a finalizer that touches the heap (e.g. by observing
        // `pending_finalizer_count()` or queueing more work) would
        // re-enter this handle through another `lock()` call.
        let pending = {
            let mut state = self.lock();
            core::mem::take(&mut state.pending_finalizers)
        };
        let mut ran = 0u64;
        for pending in pending {
            if pending.run() {
                ran = ran.saturating_add(1);
            }
        }
        if ran > 0 {
            let mut state = self.lock();
            state.finalizers_run = state.finalizers_run.saturating_add(ran);
        }
        ran
    }

    /// Run at most `max` queued finalizers and return the number
    /// that actually ran. Any finalizers beyond `max` stay queued
    /// for the next drain call.
    ///
    /// `max == 0` returns immediately with `0`.
    ///
    /// Like the unbounded drain, the heap lock is released before
    /// any finalizer body executes, so a finalizer that calls back
    /// into the heap will not re-enter this handle.
    ///
    /// Intended for VM-driven cooperative finalization: the host
    /// can run a fixed budget of finalizers per scheduler tick
    /// without committing to draining the entire queue at once.
    pub(crate) fn drain_pending_finalizers_bounded(&self, max: usize) -> u64 {
        if max == 0 {
            return 0;
        }
        // Pop at most `max` entries from the front of the queue
        // under the lock. The remainder stays in place so a
        // follow-up call can resume from where this one stopped.
        let pending = {
            let mut state = self.lock();
            let take = max.min(state.pending_finalizers.len());
            state.pending_finalizers.drain(..take).collect::<Vec<_>>()
        };
        let mut ran = 0u64;
        for pending in pending {
            if pending.run() {
                ran = ran.saturating_add(1);
            }
        }
        if ran > 0 {
            let mut state = self.lock();
            state.finalizers_run = state.finalizers_run.saturating_add(ran);
        }
        ran
    }

    pub(crate) fn with_state<R>(&self, f: impl FnOnce(&mut RuntimeState) -> R) -> R {
        let mut state = self.lock();
        f(&mut state)
    }

    pub(crate) fn try_with_state<R>(
        &self,
        f: impl FnOnce(&mut RuntimeState) -> R,
    ) -> Result<R, TryLockError<MutexGuard<'_, RuntimeState>>> {
        let mut state = self.try_lock()?;
        Ok(f(&mut state))
    }
}

impl RuntimeState {
    pub(crate) fn snapshot(&self) -> (u64, usize) {
        (self.finalizers_run, self.pending_finalizers.len())
    }

    pub(crate) fn pending_finalizer_count(&self) -> usize {
        self.pending_finalizers.len()
    }

    pub(crate) fn enqueue_pending_finalizer(&mut self, pending: PendingFinalizer) -> u64 {
        self.pending_finalizers.push(pending);
        1
    }

    pub(crate) fn drain_pending_finalizers(&mut self) -> u64 {
        let mut ran = 0u64;
        for pending in core::mem::take(&mut self.pending_finalizers) {
            if pending.run() {
                ran = ran.saturating_add(1);
            }
        }
        self.finalizers_run = self.finalizers_run.saturating_add(ran);
        ran
    }

    /// Run at most `max` queued finalizers and return the number
    /// that actually ran. Caller-of-last-resort variant: holds
    /// `&mut self` across the run, so the caller is responsible
    /// for guaranteeing that the finalizer body cannot re-enter
    /// this `RuntimeState`. The handle-side
    /// `RuntimeStateHandle::drain_pending_finalizers_bounded`
    /// releases the lock before running user code and is the
    /// usual entry point.
    pub(crate) fn drain_pending_finalizers_bounded(&mut self, max: usize) -> u64 {
        if max == 0 {
            return 0;
        }
        let take = max.min(self.pending_finalizers.len());
        let mut ran = 0u64;
        for pending in self.pending_finalizers.drain(..take) {
            if pending.run() {
                ran = ran.saturating_add(1);
            }
        }
        self.finalizers_run = self.finalizers_run.saturating_add(ran);
        ran
    }
}

#[cfg(test)]
#[path = "runtime_state_test.rs"]
mod tests;
