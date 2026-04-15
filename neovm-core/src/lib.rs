pub mod buffer;
pub mod emacs_core;
pub mod encoding;
pub mod face;
pub mod gc_trace;
pub mod heap_types;
pub mod keyboard;
pub mod logging;
pub mod tagged;
#[cfg(test)]
pub mod test_utils;
pub mod window;

pub const CORE_BACKEND: &str = "rust";

use neovm_host_abi::{
    HostAbi, HostError, IsolateId, LispValue, PatchRequest, PatchResult, PrimitiveDescriptor,
    PrimitiveId, SelectOp, SelectResult, Signal, SnapshotBlob, SnapshotRequest, TaskError,
    TaskOptions,
};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TaskHandle(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    Queued,
    Running,
    Completed,
    Cancelled,
}

pub trait TaskScheduler {
    fn spawn_task(&self, form: LispValue, opts: TaskOptions) -> Result<TaskHandle, Signal>;

    fn task_cancel(&self, handle: TaskHandle) -> bool;

    fn task_status(&self, handle: TaskHandle) -> Option<TaskStatus>;

    fn task_await(
        &self,
        handle: TaskHandle,
        timeout: Option<Duration>,
    ) -> Result<LispValue, TaskError>;

    fn select(&self, ops: &[SelectOp], timeout: Option<Duration>) -> SelectResult;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopScheduler;

impl TaskScheduler for NoopScheduler {
    fn spawn_task(&self, _form: LispValue, _opts: TaskOptions) -> Result<TaskHandle, Signal> {
        Err(Signal {
            symbol: "scheduler-unavailable".to_string(),
            data: None,
        })
    }

    fn task_cancel(&self, _handle: TaskHandle) -> bool {
        false
    }

    fn task_status(&self, _handle: TaskHandle) -> Option<TaskStatus> {
        None
    }

    fn task_await(
        &self,
        _handle: TaskHandle,
        _timeout: Option<Duration>,
    ) -> Result<LispValue, TaskError> {
        Err(TaskError::TimedOut)
    }

    fn select(&self, _ops: &[SelectOp], _timeout: Option<Duration>) -> SelectResult {
        SelectResult::TimedOut
    }
}

/// Core VM shell that will later host the evaluator, GC, and JIT entry points.
///
/// The host/editor integration and task scheduler are explicitly separated so the
/// VM core stays modular and testable.
pub struct Vm<H: HostAbi, S: TaskScheduler = NoopScheduler> {
    host: H,
    scheduler: S,
}

impl<H: HostAbi> Vm<H, NoopScheduler> {
    pub fn new(host: H) -> Self {
        Self {
            host,
            scheduler: NoopScheduler,
        }
    }

    pub fn into_host(self) -> H {
        self.host
    }
}

impl<H: HostAbi, S: TaskScheduler> Vm<H, S> {
    pub fn with_scheduler(host: H, scheduler: S) -> Self {
        Self { host, scheduler }
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    pub fn scheduler(&self) -> &S {
        &self.scheduler
    }

    pub fn scheduler_mut(&mut self) -> &mut S {
        &mut self.scheduler
    }

    pub fn into_parts(self) -> (H, S) {
        (self.host, self.scheduler)
    }

    pub fn call_primitive(
        &mut self,
        isolate: IsolateId,
        primitive: PrimitiveId,
        args: &[LispValue],
    ) -> Result<LispValue, Signal> {
        self.host.call_primitive(isolate, primitive, args)
    }

    pub fn primitive_descriptor(&self, primitive: PrimitiveId) -> PrimitiveDescriptor {
        self.host.primitive_descriptor(primitive)
    }

    pub fn clone_snapshot(&self, request: SnapshotRequest) -> Result<SnapshotBlob, HostError> {
        self.host.clone_snapshot(request)
    }

    pub fn submit_patch(&mut self, request: PatchRequest) -> Result<PatchResult, HostError> {
        self.host.submit_patch(request)
    }

    pub fn spawn_task(&self, form: LispValue, opts: TaskOptions) -> Result<TaskHandle, Signal> {
        self.scheduler.spawn_task(form, opts)
    }

    pub fn task_await(
        &self,
        handle: TaskHandle,
        timeout: Option<Duration>,
    ) -> Result<LispValue, TaskError> {
        self.scheduler.task_await(handle, timeout)
    }

    pub fn task_cancel(&self, handle: TaskHandle) -> bool {
        self.scheduler.task_cancel(handle)
    }

    pub fn task_status(&self, handle: TaskHandle) -> Option<TaskStatus> {
        self.scheduler.task_status(handle)
    }

    pub fn select(&self, ops: &[SelectOp], timeout: Option<Duration>) -> SelectResult {
        self.scheduler.select(ops, timeout)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulerConfig {
    pub worker_threads: usize,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self { worker_threads: 1 }
    }
}

#[cfg(test)]
#[path = "lib_test.rs"]
mod tests;
