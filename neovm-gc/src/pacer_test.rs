use super::*;

fn cycle_with_pause(pause_nanos: u64) -> CollectionStats {
    CollectionStats {
        collections: 1,
        major_collections: 1,
        pause_nanos,
        ..CollectionStats::default()
    }
}

fn cycle_with_pause_and_reclaim(pause_nanos: u64, reclaim_prepare_nanos: u64) -> CollectionStats {
    CollectionStats {
        collections: 1,
        major_collections: 1,
        pause_nanos,
        reclaim_prepare_nanos,
        ..CollectionStats::default()
    }
}

#[test]
fn ewma_update_seeds_with_first_observation() {
    assert_eq!(ewma_update(0.0, 100.0, 0.2), 100.0);
}

#[test]
fn ewma_update_blends_observations_with_alpha() {
    // previous=100, observed=200, alpha=0.5 → 0.5*200 + 0.5*100 = 150
    assert_eq!(ewma_update(100.0, 200.0, 0.5), 150.0);
}

#[test]
fn ewma_update_alpha_one_takes_latest() {
    assert_eq!(ewma_update(100.0, 200.0, 1.0), 200.0);
}

#[test]
fn ewma_update_small_alpha_gives_inertia() {
    // alpha=0.1: latest contributes 10%.
    let blended = ewma_update(100.0, 200.0, 0.1);
    assert!((blended - 110.0).abs() < 0.001);
}

#[test]
fn compute_target_growth_floor() {
    assert_eq!(compute_target_growth(0, 1.5, 1024), 1024);
}

#[test]
fn compute_target_growth_scales_live_bytes() {
    // 1000 * 1.5 = 1500, above floor of 100, returns 1500.
    assert_eq!(compute_target_growth(1000, 1.5, 100), 1500);
}

#[test]
fn compute_max_safe_growth_zero_when_no_mark_rate() {
    assert_eq!(compute_max_safe_growth(0.0, 0.01), 0);
}

#[test]
fn compute_max_safe_growth_returns_byte_budget() {
    // 1_000_000 bytes/sec * 0.01 sec = 10_000 bytes.
    assert_eq!(compute_max_safe_growth(1_000_000.0, 0.01), 10_000);
}

#[test]
fn compute_cpu_aware_growth_uses_go_pacer_formula() {
    // L=1000, r_alloc=2000, r_mark=4000, c=0.25
    // G = 1000 * 2000 * 0.75 / (0.25 * 4000)
    //   = 1_500_000 / 1_000
    //   = 1500
    assert_eq!(compute_cpu_aware_growth(1000, 2000.0, 4000.0, 0.25), 1500);
}

#[test]
fn compute_cpu_aware_growth_zero_when_no_alloc_rate() {
    assert_eq!(compute_cpu_aware_growth(1000, 0.0, 4000.0, 0.25), 0);
}

#[test]
fn compute_cpu_aware_growth_zero_when_no_mark_rate() {
    assert_eq!(compute_cpu_aware_growth(1000, 2000.0, 0.0, 0.25), 0);
}

#[test]
fn compute_cpu_aware_growth_zero_when_cpu_fraction_at_or_outside_unit_open_interval() {
    assert_eq!(compute_cpu_aware_growth(1000, 2000.0, 4000.0, 0.0), 0);
    assert_eq!(compute_cpu_aware_growth(1000, 2000.0, 4000.0, 1.0), 0);
    assert_eq!(compute_cpu_aware_growth(1000, 2000.0, 4000.0, -0.5), 0);
    assert_eq!(compute_cpu_aware_growth(1000, 2000.0, 4000.0, 1.5), 0);
}

#[test]
fn compute_cpu_aware_growth_zero_when_inputs_non_finite() {
    assert_eq!(
        compute_cpu_aware_growth(1000, f64::INFINITY, 4000.0, 0.25),
        0
    );
    assert_eq!(compute_cpu_aware_growth(1000, f64::NAN, 4000.0, 0.25), 0);
    assert_eq!(compute_cpu_aware_growth(1000, 2000.0, f64::NAN, 0.25), 0);
}

#[test]
fn compute_cpu_aware_growth_larger_with_lower_cpu_fraction() {
    // The formula is G ∝ (1-c)/c, which is monotonically
    // *decreasing* in c. A smaller GC CPU budget means GC must
    // run *less* often, which means letting the heap grow
    // *more* between collections.
    let g_higher_budget = compute_cpu_aware_growth(10_000, 1000.0, 1000.0, 0.5);
    let g_lower_budget = compute_cpu_aware_growth(10_000, 1000.0, 1000.0, 0.10);
    assert!(
        g_lower_budget > g_higher_budget,
        "expected smaller GC CPU budget (c=0.10) to produce \
         a larger growth budget than c=0.5, but \
         g_lower_budget={} g_higher_budget={}",
        g_lower_budget,
        g_higher_budget
    );
}

#[test]
fn pacer_threshold_clamped_by_target_gc_cpu_fraction() {
    // Configure the other two constraints so they cannot win:
    //   - heap_growth_target_ratio is huge → target_growth wide
    //   - target_pause is huge → max_safe_growth wide
    // Only the CPU-aware growth constrains the threshold.
    // ewma_alpha=1.0 makes the EWMA take the latest sample.
    let pacer = Pacer::new(PacerConfig {
        target_gc_cpu_fraction: 0.10,
        heap_growth_target_ratio: 1_000.0,
        target_pause: Duration::from_secs(1),
        min_trigger_bytes: 1,
        ewma_alpha: 1.0,
        ..PacerConfig::default()
    });
    let now = Instant::now();
    // First cycle: pause=1ms, live=10_000 → mark_rate = 10_000 / 0.001
    // = 10_000_000 bps. No allocation rate sample yet.
    let cycle = cycle_with_pause(1_000_000);
    pacer.record_completed_cycle_at(&cycle, 10_000, now);

    // Allocate 5_000 bytes between cycles, then complete the
    // second cycle 1 second later. alloc_rate = 5_000 bps. The
    // space here doesn't matter — the cpu-aware path only reads
    // the major-side counter.
    pacer.record_allocation(5_000, SpaceKind::Old);
    pacer.record_completed_cycle_at(&cycle, 10_000, now + Duration::from_secs(1));

    let stats = pacer.stats();
    // Expected G_cpu
    //   = 10_000 * 5_000 * 0.9 / (0.10 * 10_000_000)
    //   = 45_000_000 / 1_000_000
    //   = 45
    // The other two constraints offer 10_000 * 1000 = 10_000_000
    // and 10_000_000 * 1.0 = 10_000_000 respectively, so the
    // CPU-aware constraint wins.
    assert_eq!(stats.next_major_trigger_bytes, 10_000 + 45);
}

#[test]
fn pacer_threshold_falls_back_to_target_growth_when_cpu_fraction_disabled() {
    // target_gc_cpu_fraction=1.0 disables the CPU-aware constraint
    // (the formula collapses to 0). target_growth then wins.
    let pacer = Pacer::new(PacerConfig {
        target_gc_cpu_fraction: 1.0,
        heap_growth_target_ratio: 0.5,
        target_pause: Duration::from_secs(100),
        min_trigger_bytes: 1,
        ewma_alpha: 1.0,
        ..PacerConfig::default()
    });
    let now = Instant::now();
    let cycle = cycle_with_pause(1_000_000);
    pacer.record_completed_cycle_at(&cycle, 10_000, now);
    pacer.record_allocation(5_000, SpaceKind::Old);
    pacer.record_completed_cycle_at(&cycle, 10_000, now + Duration::from_secs(1));

    let stats = pacer.stats();
    // target_growth = 10_000 * 0.5 = 5_000
    // max_safe_growth = 10_000_000 * 100 = 1_000_000_000 (loses)
    // cpu_aware_growth = 0 (skipped)
    // chosen = 5_000
    assert_eq!(stats.next_major_trigger_bytes, 10_000 + 5_000);
}

#[test]
fn pacer_threshold_first_cycle_unaffected_by_cpu_fraction() {
    // The first cycle has no allocation-rate sample, so the CPU
    // constraint must skip and not falsely clamp the threshold.
    let pacer = Pacer::new(PacerConfig {
        target_gc_cpu_fraction: 0.01,
        heap_growth_target_ratio: 0.5,
        target_pause: Duration::from_secs(100),
        min_trigger_bytes: 1,
        ewma_alpha: 1.0,
        ..PacerConfig::default()
    });
    let cycle = cycle_with_pause(1_000_000);
    pacer.record_completed_cycle_at(&cycle, 10_000, Instant::now());

    let stats = pacer.stats();
    // No alloc-rate sample yet, so cpu-aware returns 0 and is
    // skipped. target_growth = 5_000 wins over the very wide
    // max_safe_growth.
    assert_eq!(stats.next_major_trigger_bytes, 10_000 + 5_000);
}

#[test]
fn pacer_default_threshold_starts_at_min_trigger() {
    let pacer = Pacer::new(PacerConfig::default());
    let stats = pacer.stats();
    assert_eq!(stats.observed_cycles, 0);
    assert_eq!(stats.last_live_bytes, 0);
    assert_eq!(stats.next_major_trigger_bytes, 1024 * 1024);
}

#[test]
fn pacer_mark_rate_uses_pause_minus_reclaim_prepare_time() {
    // Use pause=10ms with reclaim_prepare=8ms so mark-only time
    // is 2ms. With live=1000 bytes after the cycle, mark rate
    // should be 1000 / 0.002 = 500_000 bps. If the pacer used
    // pause_nanos directly, it would compute 1000 / 0.010 =
    // 100_000 bps -- five times lower.
    //
    // ewma_alpha=1.0 makes the EWMA take the latest sample
    // exactly so the test reads the observed rate directly.
    let pacer = Pacer::new(PacerConfig {
        ewma_alpha: 1.0,
        ..PacerConfig::default()
    });
    let cycle = cycle_with_pause_and_reclaim(10_000_000, 8_000_000);
    pacer.record_completed_cycle(&cycle, 1000);
    let stats = pacer.stats();
    assert_eq!(
        stats.mark_rate_bps, 500_000,
        "expected mark_rate_bps to be live / (pause - reclaim_prepare) \
         = 1000 / 0.002s = 500_000, got {}",
        stats.mark_rate_bps
    );
}

#[test]
fn pacer_mark_rate_falls_back_to_pause_nanos_when_reclaim_zero() {
    // When reclaim_prepare_nanos == 0, the subtraction yields the
    // full pause_nanos and the mark rate matches the original
    // (pre-improvement) behavior.
    let pacer = Pacer::new(PacerConfig {
        ewma_alpha: 1.0,
        ..PacerConfig::default()
    });
    let cycle = cycle_with_pause(10_000_000);
    pacer.record_completed_cycle(&cycle, 1000);
    let stats = pacer.stats();
    assert_eq!(stats.mark_rate_bps, 100_000);
}

#[test]
fn pacer_record_completed_cycle_updates_threshold() {
    let pacer = Pacer::new(PacerConfig {
        min_trigger_bytes: 256,
        ..PacerConfig::default()
    });
    let cycle = cycle_with_pause(1_000_000);
    pacer.record_completed_cycle(&cycle, 4096);
    let stats = pacer.stats();
    assert_eq!(stats.observed_cycles, 1);
    assert_eq!(stats.last_live_bytes, 4096);
    // 4096 + max(256, min(target_growth, max_safe_growth))
    assert!(stats.next_major_trigger_bytes >= 4096 + 256);
}

#[test]
fn pacer_overshoot_increments_count_when_pause_exceeds_target() {
    let pacer = Pacer::new(PacerConfig {
        target_pause: Duration::from_millis(1),
        ..PacerConfig::default()
    });
    let cycle = cycle_with_pause(2_000_000);
    pacer.record_completed_cycle(&cycle, 1024);
    let stats = pacer.stats();
    assert_eq!(stats.overshoot_count, 1);
}

#[test]
fn pacer_no_overshoot_when_pause_within_target() {
    let pacer = Pacer::new(PacerConfig {
        target_pause: Duration::from_millis(10),
        ..PacerConfig::default()
    });
    let cycle = cycle_with_pause(1_000_000);
    pacer.record_completed_cycle(&cycle, 1024);
    let stats = pacer.stats();
    assert_eq!(stats.overshoot_count, 0);
}

#[test]
fn pacer_record_allocation_returns_continue_below_threshold() {
    let pacer = Pacer::new(PacerConfig {
        min_trigger_bytes: 4096,
        ..PacerConfig::default()
    });
    let decision = pacer.record_allocation(64, SpaceKind::Old);
    assert_eq!(decision, PacerDecision::Continue);
}

#[test]
fn pacer_record_allocation_returns_trigger_major_when_threshold_exceeded() {
    let pacer = Pacer::new(PacerConfig {
        min_trigger_bytes: 256,
        ..PacerConfig::default()
    });
    let decision = pacer.record_allocation(512, SpaceKind::Old);
    assert_eq!(decision, PacerDecision::TriggerMajor);
}

#[test]
fn pacer_minor_disabled_when_soft_threshold_zero() {
    // The default config has nursery_soft_trigger_bytes=0, so
    // even very large nursery allocations never produce a
    // TriggerMinor decision.
    let pacer = Pacer::new(PacerConfig {
        min_trigger_bytes: usize::MAX,
        heap_growth_target_ratio: 1.5,
        ..PacerConfig::default()
    });
    let decision = pacer.record_allocation(1024 * 1024, SpaceKind::Nursery);
    assert_eq!(decision, PacerDecision::Continue);
}

#[test]
fn pacer_record_allocation_returns_trigger_minor_when_nursery_soft_threshold_exceeded() {
    let pacer = Pacer::new(PacerConfig {
        nursery_soft_trigger_bytes: 1024,
        // huge so the major path never fires.
        min_trigger_bytes: usize::MAX,
        heap_growth_target_ratio: 1.5,
        ..PacerConfig::default()
    });
    // Below the soft threshold.
    assert_eq!(
        pacer.record_allocation(512, SpaceKind::Nursery),
        PacerDecision::Continue
    );
    // Crossing the soft threshold (cumulative 1536 >= 1024).
    assert_eq!(
        pacer.record_allocation(1024, SpaceKind::Nursery),
        PacerDecision::TriggerMinor
    );
}

#[test]
fn pacer_minor_threshold_only_counts_nursery_allocations() {
    let pacer = Pacer::new(PacerConfig {
        nursery_soft_trigger_bytes: 1024,
        min_trigger_bytes: usize::MAX,
        heap_growth_target_ratio: 1.5,
        ..PacerConfig::default()
    });
    // Old allocations do not advance the nursery soft counter.
    assert_eq!(
        pacer.record_allocation(8192, SpaceKind::Old),
        PacerDecision::Continue
    );
    // A small nursery allocation is still well below the
    // 1024-byte soft threshold.
    assert_eq!(
        pacer.record_allocation(64, SpaceKind::Nursery),
        PacerDecision::Continue
    );
}

#[test]
fn pacer_record_completed_minor_cycle_resets_nursery_counter() {
    let pacer = Pacer::new(PacerConfig {
        nursery_soft_trigger_bytes: 1024,
        min_trigger_bytes: usize::MAX,
        heap_growth_target_ratio: 1.5,
        ..PacerConfig::default()
    });
    pacer.record_allocation(2048, SpaceKind::Nursery);
    // Sanity check: counter has accumulated.
    assert_eq!(pacer.stats().nursery_bytes_since_last_minor, 2048);
    pacer.record_completed_minor_cycle();
    // After the reset the counter is empty and a small alloc
    // returns Continue.
    let stats = pacer.stats();
    assert_eq!(stats.nursery_bytes_since_last_minor, 0);
    assert_eq!(stats.observed_minor_cycles, 1);
    assert_eq!(
        pacer.record_allocation(64, SpaceKind::Nursery),
        PacerDecision::Continue
    );
}

#[test]
fn pacer_major_check_takes_priority_over_minor() {
    // When both thresholds are exceeded, TriggerMajor wins —
    // running a minor on top would be wasted work.
    let pacer = Pacer::new(PacerConfig {
        nursery_soft_trigger_bytes: 256,
        min_trigger_bytes: 1024,
        heap_growth_target_ratio: 1.5,
        ..PacerConfig::default()
    });
    let decision = pacer.record_allocation(2048, SpaceKind::Nursery);
    assert_eq!(decision, PacerDecision::TriggerMajor);
}

#[test]
fn pacer_update_config_preserves_runtime_state() {
    // Build a pacer, run a completed cycle so EWMA state and
    // counters get populated, then call update_config and
    // verify that all the runtime state survives the swap.
    let pacer = Pacer::new(PacerConfig {
        min_trigger_bytes: 256,
        ..PacerConfig::default()
    });
    let cycle = cycle_with_pause(1_000_000);
    pacer.record_completed_cycle(&cycle, 4096);
    pacer.record_allocation(128, SpaceKind::Old);

    let before = pacer.stats();
    assert_eq!(before.observed_cycles, 1);
    assert_eq!(before.last_live_bytes, 4096);
    let before_trigger = before.next_major_trigger_bytes;
    let before_mark_rate = before.mark_rate_bps;

    // Update config to a totally different growth ratio. The
    // EWMA state and observed_cycles must NOT reset.
    pacer.update_config(PacerConfig {
        min_trigger_bytes: 8192,
        heap_growth_target_ratio: 8.0,
        ..PacerConfig::default()
    });
    let after = pacer.stats();
    assert_eq!(after.observed_cycles, 1, "observed_cycles preserved");
    assert_eq!(after.last_live_bytes, 4096, "last_live_bytes preserved");
    assert_eq!(after.mark_rate_bps, before_mark_rate, "mark_rate preserved");
    // The next-trigger value is NOT recomputed by update_config
    // (only by the next record_completed_cycle), so it stays at
    // the previous cycle's value.
    assert_eq!(
        after.next_major_trigger_bytes, before_trigger,
        "next_major_trigger_bytes preserved until next cycle"
    );
    // The new config IS observable.
    let cfg = pacer.config();
    assert_eq!(cfg.min_trigger_bytes, 8192);
    assert!((cfg.heap_growth_target_ratio - 8.0).abs() < f64::EPSILON);
}

#[test]
fn pacer_clones_share_config_updates() {
    // Pacer clones share the same Arc<Mutex<PacerState>>, so
    // updating config on one handle is visible from another.
    let pacer = Pacer::new(PacerConfig::default());
    let clone = pacer.clone();
    pacer.update_config(PacerConfig {
        nursery_soft_trigger_bytes: 4321,
        ..PacerConfig::default()
    });
    assert_eq!(clone.config().nursery_soft_trigger_bytes, 4321);
    // And the clone's record_allocation honors the new config.
    let decision = clone.record_allocation(usize::MAX / 2, SpaceKind::Nursery);
    // usize::MAX / 2 with default min_trigger_bytes (1 MiB)
    // crosses the major threshold, but it should NOT trigger
    // minor first because Major has priority. Verify the
    // returned decision is sane.
    assert_ne!(decision, PacerDecision::Continue);
}

#[test]
fn pacer_decision_can_be_re_evaluated_without_advancing_state() {
    let pacer = Pacer::new(PacerConfig {
        nursery_soft_trigger_bytes: 1024,
        min_trigger_bytes: usize::MAX,
        heap_growth_target_ratio: 1.5,
        ..PacerConfig::default()
    });
    pacer.record_allocation(2048, SpaceKind::Nursery);
    // First decision call sees the trigger.
    assert_eq!(pacer.decision(), PacerDecision::TriggerMinor);
    // A second call without advancing state still sees it.
    assert_eq!(pacer.decision(), PacerDecision::TriggerMinor);
    // After a minor cycle resets the counter, decision flips.
    pacer.record_completed_minor_cycle();
    assert_eq!(pacer.decision(), PacerDecision::Continue);
}
