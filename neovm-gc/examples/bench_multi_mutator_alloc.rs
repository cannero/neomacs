//! Standalone allocation throughput benchmark for the
//! single-mutator and multi-mutator paths.
//!
//! Usage:
//!
//! ```text
//! cargo run --release --example bench_multi_mutator_alloc -p neovm-gc
//! ```
//!
//! Reports allocations-per-second for the single-mutator
//! path (the crate's historical baseline) and for 1, 2, 4, and
//! 8 concurrent mutator threads after the multi-mutator
//! refactor (`Heap::mutator(&self)` + `MutatorLocal::tlab` +
//! `Arc<RwLock<HeapCore>>`). The numbers are not a
//! statistically rigorous benchmark — there is no warm-up
//! loop, no median across runs, no criterion-style
//! statistical analysis. They exist to:
//!
//! 1. Validate that the refactor did not catastrophically
//!    regress single-mutator throughput.
//! 2. Confirm that multi-mutator throughput actually scales
//!    with thread count under the new lock model.
//! 3. Surface obvious lock contention if the
//!    `Arc<RwLock<HeapCore>>` write lock turns into a
//!    bottleneck.
//!
//! Each inner loop drops the handle scope every `BATCH_SIZE`
//! allocations so the root stack does not grow without
//! bound. Allocations are small `Leaf(u64)` records that go
//! straight through the nursery TLAB fast path.
//!
//! Run with `--release` so the allocation hot-path call
//! chain is fully inlined; debug builds report wildly
//! pessimistic numbers.

use neovm_gc::{Heap, HeapConfig, Relocator, Trace, Tracer};
use neovm_gc::spaces::NurseryConfig;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

/// Build a heap config with a huge nursery so the minor GC
/// does not fire during the benchmark and the pacer's soft
/// minor trigger does not shorten the run prematurely.
/// The goal is to isolate the allocation hot path from
/// collection effects.
fn bench_heap_config() -> HeapConfig {
    HeapConfig {
        nursery: NurseryConfig {
            // 256 MiB semispace: way larger than any
            // individual benchmark run will need.
            semispace_bytes: 256 * 1024 * 1024,
            ..NurseryConfig::default()
        },
        ..HeapConfig::default()
    }
}

#[derive(Debug)]
#[allow(dead_code)]
struct Leaf(u64);

unsafe impl Trace for Leaf {
    fn trace(&self, _tracer: &mut dyn Tracer) {}
    fn relocate(&self, _relocator: &mut dyn Relocator) {}
}

/// Number of allocations per handle scope. After this many
/// allocations the scope is dropped, releasing all the
/// transient roots so the root stack does not grow without
/// bound. Chosen so a batch is large enough to amortize the
/// scope setup cost but small enough that every batch fits
/// trivially inside the default nursery.
const BATCH_SIZE: u64 = 1_000;

/// Total allocations per measurement run. Keep small enough
/// that the benchmark completes in a few seconds per
/// configuration but large enough to be statistically
/// meaningful. At the current single-mutator rate of
/// ~12K alloc/sec, 20K allocations is ~1.7s per run.
const ALLOCATIONS_PER_RUN: u64 = 20_000;

fn bench_single_mutator(allocations: u64) -> f64 {
    let heap = Heap::new(bench_heap_config());
    let mut mutator = heap.mutator();

    let start = Instant::now();
    let mut remaining = allocations;
    while remaining > 0 {
        let batch = remaining.min(BATCH_SIZE);
        let mut scope = mutator.handle_scope();
        for i in 0..batch {
            mutator
                .alloc(&mut scope, Leaf(i))
                .expect("alloc leaf");
        }
        drop(scope);
        remaining -= batch;
    }
    let elapsed = start.elapsed();

    (allocations as f64) / elapsed.as_secs_f64()
}

fn bench_multi_mutator(thread_count: usize, allocations_per_thread: u64) -> f64 {
    let heap = Arc::new(Heap::new(bench_heap_config()));
    let mut handles = Vec::with_capacity(thread_count);

    let start = Instant::now();
    for worker_id in 0..thread_count {
        let heap = Arc::clone(&heap);
        handles.push(thread::spawn(move || {
            let mut mutator = heap.mutator();
            let mut remaining = allocations_per_thread;
            while remaining > 0 {
                let batch = remaining.min(BATCH_SIZE);
                let mut scope = mutator.handle_scope();
                for i in 0..batch {
                    let label = (worker_id as u64) * 1_000_000 + i;
                    mutator
                        .alloc(&mut scope, Leaf(label))
                        .expect("alloc concurrent leaf");
                }
                drop(scope);
                remaining -= batch;
            }
        }));
    }
    for handle in handles {
        handle.join().expect("join worker");
    }
    let elapsed = start.elapsed();

    let total = (thread_count as u64) * allocations_per_thread;
    (total as f64) / elapsed.as_secs_f64()
}

fn format_rate(rate: f64) -> String {
    if rate >= 1_000_000.0 {
        format!("{:>7.2} M alloc/sec", rate / 1_000_000.0)
    } else if rate >= 1_000.0 {
        format!("{:>7.2} K alloc/sec", rate / 1_000.0)
    } else {
        format!("{:>7.2}   alloc/sec", rate)
    }
}

fn main() {
    println!("neovm-gc allocation throughput bench");
    println!("=====================================");
    println!("(each measurement runs {} allocations per thread)", ALLOCATIONS_PER_RUN);
    println!();

    // Single-mutator baseline.
    let single = bench_single_mutator(ALLOCATIONS_PER_RUN);
    println!("single-mutator   : {}", format_rate(single));
    println!();

    // Multi-mutator baseline at 1 thread. Should match the
    // single-mutator path closely because there is no lock
    // contention with only one thread.
    let m1 = bench_multi_mutator(1, ALLOCATIONS_PER_RUN);
    println!(" 1 thread        : {}", format_rate(m1));

    // Multi-mutator with 2/4/8 threads. The speedup column
    // is relative to the 1-thread multi-mutator baseline,
    // so it captures the scaling behavior of the new lock
    // model. Perfect scaling is N; below-linear scaling is
    // normal because each allocation briefly acquires the
    // heap write lock for bookkeeping.
    for n in [2usize, 4] {
        let rate = bench_multi_mutator(n, ALLOCATIONS_PER_RUN);
        let speedup = rate / m1;
        println!(
            " {} threads       : {}  ({:.2}x vs 1 thread)",
            n,
            format_rate(rate),
            speedup,
        );
    }

    println!();
    println!("Note: these figures are indicative, not rigorous.");
    println!("      For reproducible comparisons, use criterion.");
}
