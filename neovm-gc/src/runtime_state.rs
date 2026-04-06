use crate::object::ObjectRecord;

#[derive(Debug, Default)]
pub(crate) struct RuntimeState {
    pending_finalizers: Vec<ObjectRecord>,
    finalizers_run: u64,
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
