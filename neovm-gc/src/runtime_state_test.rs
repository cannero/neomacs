use super::*;

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
