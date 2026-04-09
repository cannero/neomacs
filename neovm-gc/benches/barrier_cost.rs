//! Write-barrier cost benchmarks.
//!
//! Measures the cost of `Mutator::store_edge` and
//! `Mutator::post_write_barrier` across the short-circuit
//! cases the barrier path tries to exploit. Each bench
//! constructs the minimal object graph that forces the
//! barrier into a specific path:
//!
//! * **no_new_value**: `store_edge` with `None` as the new
//!   value. The barrier short-circuits on the "target is
//!   not a managed reference" check.
//! * **old_to_nursery**: `store_edge` writing a nursery-
//!   target reference into an old-gen owner. Exercises the
//!   per-block card-table fast path.
//! * **old_to_old**: `store_edge` writing an old-gen target
//!   into an old-gen owner. No remembered-set tracking
//!   needed; the barrier should short-circuit after the
//!   space check.
//! * **nursery_to_nursery**: `store_edge` on a nursery
//!   owner. The barrier short-circuits on the "owner is
//!   not in old gen" check.
//!
//! A regression in the barrier path that breaks short-
//! circuiting would show up as one of these benches
//! getting much slower.
//!
//! Runs with `cargo bench --bench barrier_cost -p
//! neovm-gc`.

use criterion::{BatchSize, Criterion, Throughput, black_box, criterion_group, criterion_main};
use neovm_gc::{EdgeCell, Heap};

#[path = "common/mod.rs"]
mod common;
use common::*;

fn bench_store_edge_no_new_value(c: &mut Criterion) {
    let mut group = c.benchmark_group("barrier_cost/store_edge/no_new_value");
    group.throughput(Throughput::Elements(1));
    group.bench_function("short_circuit", |b| {
        let heap = Heap::new(fast_alloc_config());
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        let owner = mutator
            .alloc(
                &mut scope,
                Node {
                    label: 0,
                    next: EdgeCell::default(),
                },
            )
            .expect("alloc owner");
        b.iter_batched(
            || (),
            |_| {
                mutator.store_edge(&owner, 0, |n| &n.next, None);
                black_box(());
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_store_edge_nursery_to_nursery(c: &mut Criterion) {
    let mut group = c.benchmark_group("barrier_cost/store_edge/nursery_to_nursery");
    group.throughput(Throughput::Elements(1));
    group.bench_function("short_circuit", |b| {
        let heap = Heap::new(fast_alloc_config());
        let mut mutator = heap.mutator();
        let mut scope = mutator.handle_scope();
        let owner = mutator
            .alloc(
                &mut scope,
                Node {
                    label: 0,
                    next: EdgeCell::default(),
                },
            )
            .expect("alloc owner");
        let target = mutator
            .alloc(
                &mut scope,
                Node {
                    label: 1,
                    next: EdgeCell::default(),
                },
            )
            .expect("alloc target");
        let target_gc = target.as_gc();
        b.iter_batched(
            || (),
            |_| {
                mutator.store_edge(&owner, 0, |n| &n.next, Some(target_gc));
                black_box(());
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_store_edge_no_new_value,
    bench_store_edge_nursery_to_nursery,
);
criterion_main!(benches);
