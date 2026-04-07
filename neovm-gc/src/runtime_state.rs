use std::sync::{Arc, Mutex, MutexGuard, TryLockError, TryLockResult};

use crate::object::ObjectRecord;
use crate::plan::RuntimeWorkStatus;
use crate::stats::HeapStats;

#[derive(Debug, Default)]
pub(crate) struct RuntimeState {
    pending_finalizers: Vec<ObjectRecord>,
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

    pub(crate) fn enqueue_pending_finalizer(&self, object: ObjectRecord) -> u64 {
        self.lock().enqueue_pending_finalizer(object)
    }

    pub(crate) fn drain_pending_finalizers(&self) -> u64 {
        // Take the pending list out of the state under the lock, then
        // release the lock before running user-defined finalizer code.
        //
        // Holding the lock across `run_finalizer()` would be a reentry
        // deadlock risk: a finalizer that touches the heap (e.g. by
        // observing `pending_finalizer_count()` or queueing more work)
        // would re-enter this handle through another `lock()` call.
        let pending = {
            let mut state = self.lock();
            core::mem::take(&mut state.pending_finalizers)
        };
        let mut ran = 0u64;
        for object in pending {
            if object.run_finalizer() {
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

    pub(crate) fn enqueue_pending_finalizer(&mut self, object: ObjectRecord) -> u64 {
        self.pending_finalizers.push(object);
        1
    }

    pub(crate) fn drain_pending_finalizers(&mut self) -> u64 {
        let mut ran = 0u64;
        for object in core::mem::take(&mut self.pending_finalizers) {
            if object.run_finalizer() {
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
