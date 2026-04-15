use super::*;
use crate::stats::HeapStats;

#[test]
fn runtime_state_handle_shares_state_across_clones() {
    let handle = RuntimeStateHandle::default();
    let clone = handle.clone();

    handle.with_state(|state| {
        state.finalizers_run = 7;
    });

    assert_eq!(clone.snapshot(), (7, 0));
}

#[test]
fn runtime_state_handle_try_with_state_reports_would_block_while_locked() {
    let handle = RuntimeStateHandle::default();
    let _guard = handle.lock();

    let error = handle
        .try_with_state(|state| state.finalizers_run = 1)
        .expect_err("try_with_state should report contention while the state is locked");

    assert!(matches!(error, TryLockError::WouldBlock));
}

#[test]
fn runtime_state_handle_apply_runtime_stats_reports_finalizer_counters() {
    let handle = RuntimeStateHandle::default();
    handle.with_state(|state| {
        state.finalizers_run = 5;
    });
    let mut stats = HeapStats::default();

    handle.apply_runtime_stats(&mut stats);

    assert_eq!(stats.finalizers_run, 5);
    assert_eq!(stats.pending_finalizers, 0);
}
