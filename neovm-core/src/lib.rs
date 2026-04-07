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
mod tests {
    use super::*;
    use neovm_host_abi::{Affinity, ChannelId, EffectClass, PrimitiveDescriptor, TaskPriority};

    #[derive(Default)]
    struct DummyHost;

    impl HostAbi for DummyHost {
        fn primitive_descriptor(&self, _primitive: PrimitiveId) -> PrimitiveDescriptor {
            PrimitiveDescriptor {
                name: "dummy",
                affinity: Affinity::WorkerSafe,
                effect: EffectClass::PureRead,
                can_trigger_gc: false,
                can_reenter_elisp: false,
                deterministic: true,
            }
        }

        fn call_primitive(
            &mut self,
            _isolate: IsolateId,
            _primitive: PrimitiveId,
            _args: &[LispValue],
        ) -> Result<LispValue, Signal> {
            Ok(LispValue::default())
        }

        fn clone_snapshot(&self, _request: SnapshotRequest) -> Result<SnapshotBlob, HostError> {
            Ok(SnapshotBlob::default())
        }

        fn submit_patch(&mut self, _request: PatchRequest) -> Result<PatchResult, HostError> {
            Ok(PatchResult::Applied { new_revision: 1 })
        }
    }

    #[derive(Default)]
    struct MockScheduler;

    impl TaskScheduler for MockScheduler {
        fn spawn_task(&self, _form: LispValue, _opts: TaskOptions) -> Result<TaskHandle, Signal> {
            Ok(TaskHandle(42))
        }

        fn task_cancel(&self, handle: TaskHandle) -> bool {
            handle.0 == 42
        }

        fn task_status(&self, handle: TaskHandle) -> Option<TaskStatus> {
            if handle.0 == 42 {
                Some(TaskStatus::Completed)
            } else {
                None
            }
        }

        fn task_await(
            &self,
            handle: TaskHandle,
            _timeout: Option<Duration>,
        ) -> Result<LispValue, TaskError> {
            if handle.0 == 42 {
                Ok(LispValue {
                    bytes: vec![1, 2, 3],
                })
            } else {
                Err(TaskError::TimedOut)
            }
        }

        fn select(&self, _ops: &[SelectOp], _timeout: Option<Duration>) -> SelectResult {
            SelectResult::Ready {
                op_index: 0,
                value: Some(LispValue { bytes: vec![9] }),
            }
        }
    }

    #[test]
    fn vm_delegates_task_apis_to_scheduler() {
        crate::test_utils::init_test_tracing();
        let vm = Vm::with_scheduler(DummyHost, MockScheduler);
        let handle = vm
            .spawn_task(
                LispValue::default(),
                TaskOptions {
                    name: Some("test".to_string()),
                    priority: TaskPriority::Interactive,
                    affinity: Affinity::WorkerSafe,
                    timeout: None,
                },
            )
            .expect("spawn should succeed");

        assert_eq!(handle, TaskHandle(42));
        assert_eq!(vm.task_status(handle), Some(TaskStatus::Completed));
        assert!(vm.task_cancel(handle));
        assert_eq!(
            vm.task_await(handle, Some(Duration::from_millis(10)))
                .expect("await should return result")
                .bytes,
            vec![1, 2, 3]
        );
        assert!(matches!(
            vm.select(&[SelectOp::Recv(ChannelId(1))], None),
            SelectResult::Ready { op_index: 0, .. }
        ));
    }

    #[test]
    fn noop_scheduler_rejects_spawn() {
        crate::test_utils::init_test_tracing();
        let vm = Vm::new(DummyHost);
        let err = vm
            .spawn_task(LispValue::default(), TaskOptions::default())
            .expect_err("noop scheduler should reject task spawn");
        assert_eq!(err.symbol, "scheduler-unavailable");
    }

    #[test]
    fn noop_scheduler_task_cancel_returns_false() {
        crate::test_utils::init_test_tracing();
        let sched = NoopScheduler;
        assert!(!sched.task_cancel(TaskHandle(999)));
    }

    #[test]
    fn noop_scheduler_task_status_returns_none() {
        crate::test_utils::init_test_tracing();
        let sched = NoopScheduler;
        assert_eq!(sched.task_status(TaskHandle(1)), None);
    }

    #[test]
    fn noop_scheduler_task_await_returns_timed_out() {
        crate::test_utils::init_test_tracing();
        let sched = NoopScheduler;
        let err = sched.task_await(TaskHandle(1), None).unwrap_err();
        assert!(matches!(err, TaskError::TimedOut));
    }

    #[test]
    fn noop_scheduler_select_returns_timed_out() {
        crate::test_utils::init_test_tracing();
        let sched = NoopScheduler;
        let result = sched.select(&[], None);
        assert!(matches!(result, SelectResult::TimedOut));
    }

    #[test]
    fn vm_into_parts() {
        crate::test_utils::init_test_tracing();
        let vm = Vm::with_scheduler(DummyHost, MockScheduler);
        let (host, sched) = vm.into_parts();
        // Verify we got back our types (they're unit structs, just check we can use them)
        let _ = host;
        let _ = sched;
    }

    #[test]
    fn vm_into_host() {
        crate::test_utils::init_test_tracing();
        let vm = Vm::new(DummyHost);
        let _host = vm.into_host();
    }

    #[test]
    fn vm_call_primitive_delegates() {
        crate::test_utils::init_test_tracing();
        let mut vm = Vm::new(DummyHost);
        let result = vm
            .call_primitive(IsolateId(0), PrimitiveId(0), &[])
            .unwrap();
        assert_eq!(result, LispValue::default());
    }

    #[test]
    fn vm_primitive_descriptor_delegates() {
        crate::test_utils::init_test_tracing();
        let vm = Vm::new(DummyHost);
        let desc = vm.primitive_descriptor(PrimitiveId(0));
        assert_eq!(desc.name, "dummy");
    }

    #[test]
    fn task_handle_eq_hash() {
        crate::test_utils::init_test_tracing();
        use std::collections::HashSet;
        let h1 = TaskHandle(1);
        let h2 = TaskHandle(1);
        let h3 = TaskHandle(2);
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
        let mut set = HashSet::new();
        set.insert(h1);
        assert!(set.contains(&h2));
        assert!(!set.contains(&h3));
    }

    #[test]
    fn scheduler_config_default() {
        crate::test_utils::init_test_tracing();
        let cfg = SchedulerConfig::default();
        assert_eq!(cfg.worker_threads, 1);
    }
}
