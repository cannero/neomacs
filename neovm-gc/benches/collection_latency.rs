//! Collection latency benchmarks.
//!
//! Measures the wall-clock cost of minor and major
//! collection cycles triggered by `Mutator::collect`. Each
//! bench sets up a heap with a known live set, runs one
//! collection, and reports the cycle duration.
//!
//! Criterion's statistical analysis gives a median plus
//! outlier detection, which is the right shape for pause
//! latency — the important number for a GC consumer is
//! "what's the typical pause under this workload" plus
//! "what's the tail" (P95/P99).
//!
//! Each bench uses `iter_custom` to control setup-per-
//! iteration carefully: the heap is reconstructed from
//! scratch for every measured cycle so the starting state
//! is deterministic.
//!
//! Runs with `cargo bench --bench collection_latency -p
//! neovm-gc`.

use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use neovm_gc::{CollectionKind, Heap};
use std::time::{Duration, Instant};

#[path = "common/mod.rs"]
mod common;
use common::*;

fn bench_minor_gc_small_nursery(c: &mut Criterion) {
    // A minor cycle with a modest nursery load. Most
    // allocations die; the survivor set is small.
    let mut group = c.benchmark_group("collection_latency/minor/small");
    group.throughput(Throughput::Elements(1));
    group.bench_function("drop_all", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let heap = Heap::new(fast_alloc_config());
                let mut mutator = heap.mutator();
                {
                    let mut scope = mutator.handle_scope();
                    for i in 0..1_000u64 {
                        mutator
                            .alloc(&mut scope, SmallLeaf(i))
                            .expect("alloc");
                    }
                }
                // Scope is dropped; all 1000 leaves are
                // unreachable. The minor cycle should
                // reclaim them in bulk.
                let start = Instant::now();
                black_box(
                    mutator
                        .collect(CollectionKind::Minor)
                        .expect("minor collect"),
                );
                total += start.elapsed();
            }
            total
        });
    });
    group.finish();
}

fn bench_minor_gc_all_survive(c: &mut Criterion) {
    // Every nursery allocation survives and must be copied
    // to the to-space (or promoted to old gen if old
    // enough).
    let mut group = c.benchmark_group("collection_latency/minor/all_survive");
    group.throughput(Throughput::Elements(1));
    group.bench_function("1000_survivors", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let heap = Heap::new(fast_alloc_config());
                let mut mutator = heap.mutator();
                let mut scope = mutator.handle_scope();
                for i in 0..1_000u64 {
                    mutator
                        .alloc(&mut scope, SmallLeaf(i))
                        .expect("alloc");
                }
                // Scope is still alive: every allocation
                // is rooted. The minor cycle must evacuate
                // all 1000.
                let start = Instant::now();
                black_box(
                    mutator
                        .collect(CollectionKind::Minor)
                        .expect("minor collect"),
                );
                total += start.elapsed();
                // Scope drop at end of iteration releases
                // roots for the next iteration.
                drop(scope);
            }
            total
        });
    });
    group.finish();
}

fn bench_major_gc_small(c: &mut Criterion) {
    // Major cycle on a small old-gen population. First
    // force survivors into old gen via a minor cycle,
    // then trigger a major on the populated old gen.
    let mut group = c.benchmark_group("collection_latency/major/small");
    group.throughput(Throughput::Elements(1));
    group.bench_function("1000_old_survivors", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let heap = Heap::new(fast_alloc_config());
                let mut mutator = heap.mutator();
                let mut scope = mutator.handle_scope();
                for i in 0..1_000u64 {
                    mutator
                        .alloc(&mut scope, SmallLeaf(i))
                        .expect("alloc");
                }
                // Run two minor cycles to age survivors
                // into the old gen (default promotion_age
                // is 2).
                let _ = mutator.collect(CollectionKind::Minor);
                let _ = mutator.collect(CollectionKind::Minor);
                // Now measure one major cycle.
                let start = Instant::now();
                black_box(
                    mutator
                        .collect(CollectionKind::Major)
                        .expect("major collect"),
                );
                total += start.elapsed();
                drop(scope);
            }
            total
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_minor_gc_small_nursery,
    bench_minor_gc_all_survive,
    bench_major_gc_small,
);
criterion_main!(benches);
