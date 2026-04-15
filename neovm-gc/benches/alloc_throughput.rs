//! Allocation throughput benchmarks.
//!
//! Measures per-allocation cost for the single-mutator path
//! across several object sizes and across the two dominant
//! HandleScope usage patterns:
//!
//! 1. **Long-lived scope**: one `HandleScope` for the entire
//!    batch. The root stack holds every allocated object for
//!    the duration of the measurement. Measures the steady-
//!    state per-alloc cost when the application keeps
//!    allocated objects alive (tree building, list building,
//!    etc.).
//! 2. **Scoped batches**: drop and recreate the `HandleScope`
//!    every 1000 allocations. Matches the shape of workloads
//!    that discard most allocated objects quickly (tight
//!    inner loops, temporary computation).
//!
//! Each bench is parameterized on batch size so the scaling
//! shape is visible. A flat curve (ns/alloc roughly constant
//! across batch sizes) confirms the allocation path is O(1)
//! per call. A rising curve flags a regression back into the
//! O(n) hot path that the Stage 0 investigation fixed.
//!
//! Runs with `cargo bench --bench alloc_throughput -p
//! neovm-gc`. Results land in `target/criterion/`.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use neovm_gc::Heap;
use std::time::{Duration, Instant};

#[path = "common/mod.rs"]
mod common;
use common::*;

const BATCH_SIZES: &[u64] = &[1_000, 10_000, 50_000];

fn bench_small_leaf_long_scope(c: &mut Criterion) {
    let mut group = c.benchmark_group("alloc_throughput/small_leaf/long_scope");
    for &batch in BATCH_SIZES {
        group.throughput(Throughput::Elements(batch));
        group.bench_with_input(BenchmarkId::from_parameter(batch), &batch, |b, &batch| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let heap = Heap::new(fast_alloc_config());
                    let mut mutator = heap.mutator();
                    let mut scope = mutator.handle_scope();
                    let start = Instant::now();
                    for i in 0..batch {
                        black_box(mutator.alloc(&mut scope, SmallLeaf(i)).expect("alloc"));
                    }
                    total += start.elapsed();
                }
                total
            });
        });
    }
    group.finish();
}

fn bench_small_leaf_scoped_batches(c: &mut Criterion) {
    let mut group = c.benchmark_group("alloc_throughput/small_leaf/scoped_batches");
    const SCOPE_SIZE: u64 = 1_000;
    for &batch in BATCH_SIZES {
        group.throughput(Throughput::Elements(batch));
        group.bench_with_input(BenchmarkId::from_parameter(batch), &batch, |b, &batch| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let heap = Heap::new(fast_alloc_config());
                    let mut mutator = heap.mutator();
                    let start = Instant::now();
                    let mut remaining = batch;
                    while remaining > 0 {
                        let step = remaining.min(SCOPE_SIZE);
                        let mut scope = mutator.handle_scope();
                        for i in 0..step {
                            black_box(mutator.alloc(&mut scope, SmallLeaf(i)).expect("alloc"));
                        }
                        drop(scope);
                        remaining -= step;
                    }
                    total += start.elapsed();
                }
                total
            });
        });
    }
    group.finish();
}

fn bench_medium_leaf(c: &mut Criterion) {
    let mut group = c.benchmark_group("alloc_throughput/medium_leaf");
    const BATCH: u64 = 10_000;
    group.throughput(Throughput::Elements(BATCH));
    group.bench_function("long_scope", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let heap = Heap::new(fast_alloc_config());
                let mut mutator = heap.mutator();
                let mut scope = mutator.handle_scope();
                let start = Instant::now();
                for _ in 0..BATCH {
                    black_box(
                        mutator
                            .alloc(&mut scope, MediumLeaf([0u8; 128]))
                            .expect("alloc"),
                    );
                }
                total += start.elapsed();
            }
            total
        });
    });
    group.finish();
}

fn bench_large_leaf(c: &mut Criterion) {
    let mut group = c.benchmark_group("alloc_throughput/large_leaf");
    const BATCH: u64 = 1_000;
    group.throughput(Throughput::Elements(BATCH));
    group.bench_function("long_scope", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let heap = Heap::new(fast_alloc_config());
                let mut mutator = heap.mutator();
                let mut scope = mutator.handle_scope();
                let start = Instant::now();
                for _ in 0..BATCH {
                    black_box(
                        mutator
                            .alloc(&mut scope, LargeLeaf([0u8; 1024]))
                            .expect("alloc"),
                    );
                }
                total += start.elapsed();
            }
            total
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_small_leaf_long_scope,
    bench_small_leaf_scoped_batches,
    bench_medium_leaf,
    bench_large_leaf,
);
criterion_main!(benches);
