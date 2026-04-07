use super::*;

#[test]
fn empty_pause_stats_report_zero_samples() {
    let stats = PauseStats::default();
    let snapshot = stats.snapshot();
    assert_eq!(snapshot.sample_count, 0);
    assert_eq!(snapshot.total_samples, 0);
    assert_eq!(snapshot.max_nanos, 0);
    assert_eq!(snapshot.p99_nanos, 0);
}

#[test]
fn single_sample_percentiles_are_sample_value() {
    let mut stats = PauseStats::default();
    stats.record(500);
    let snapshot = stats.snapshot();
    assert_eq!(snapshot.sample_count, 1);
    assert_eq!(snapshot.min_nanos, 500);
    assert_eq!(snapshot.max_nanos, 500);
    assert_eq!(snapshot.mean_nanos, 500);
    assert_eq!(snapshot.p50_nanos, 500);
    assert_eq!(snapshot.p95_nanos, 500);
    assert_eq!(snapshot.p99_nanos, 500);
    assert_eq!(snapshot.total_samples, 1);
}

#[test]
fn percentiles_are_monotonically_nondecreasing() {
    let mut stats = PauseStats::default();
    for value in [100, 200, 300, 400, 500, 600, 700, 800, 900, 1000] {
        stats.record(value);
    }
    let snapshot = stats.snapshot();
    assert_eq!(snapshot.sample_count, 10);
    assert_eq!(snapshot.min_nanos, 100);
    assert_eq!(snapshot.max_nanos, 1000);
    assert_eq!(snapshot.mean_nanos, 550);
    assert!(snapshot.p50_nanos <= snapshot.p95_nanos);
    assert!(snapshot.p95_nanos <= snapshot.p99_nanos);
    assert!(snapshot.p99_nanos <= snapshot.max_nanos);
}

#[test]
fn window_overflow_evicts_oldest_samples_but_keeps_total_count() {
    let mut stats = PauseStats::with_capacity(4);
    for value in [10, 20, 30, 40, 50, 60] {
        stats.record(value);
    }
    let snapshot = stats.snapshot();
    // Window capacity is 4; most recent 4 samples are [30, 40, 50, 60].
    assert_eq!(snapshot.sample_count, 4);
    assert_eq!(snapshot.window_capacity, 4);
    assert_eq!(snapshot.total_samples, 6);
    assert_eq!(snapshot.min_nanos, 30);
    assert_eq!(snapshot.max_nanos, 60);
    assert_eq!(snapshot.mean_nanos, 45);
}

#[test]
fn reset_clears_window_but_not_capacity() {
    let mut stats = PauseStats::with_capacity(8);
    for value in 0..8u64 {
        stats.record(value);
    }
    stats.reset();
    let snapshot = stats.snapshot();
    assert_eq!(snapshot.sample_count, 0);
    assert_eq!(snapshot.total_samples, 0);
    assert_eq!(snapshot.window_capacity, 8);
}
