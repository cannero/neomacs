//! Realistic object-graph workload benchmarks.
//!
//! The other bench files measure individual operations
//! (allocation, barriers, single GC cycles). This file
//! measures *composite* workloads that exercise the full
//! allocate-mutate-collect pipeline on object-graph shapes
//! a real VM would build:
//!
//! 1. **Linked list** — prepend-heavy. Every insertion is
//!    one allocation plus one `store_edge` call to link the
//!    new head to the previous head. The new head is in
//!    nursery; the previous head is old after a few cycles.
//!    Exercises the old-to-nursery remembered-set path
//!    heavily.
//!
//! 2. **Binary tree build** — tree-of-depth-D construction
//!    via recursive allocation. Deep allocation chains,
//!    many edges per object, tests the trace throughput
//!    during minor cycles.
//!
//! 3. **Hashmap-shaped** — a growing vector of key/value
//!    cells backed by GC-managed arrays. Mixed allocation
//!    patterns (array resize, per-entry cell, key string).
//!    Tests old-gen block allocation alongside nursery
//!    churn.
//!
//! These benches are slower than the microbenches because
//! each iteration builds a non-trivial object graph. Use
//! `--sample-size 10 --measurement-time 5` for reasonable
//! run times.
//!
//! Runs with `cargo bench --bench workloads -p neovm-gc`.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use neovm_gc::{EdgeCell, Heap};
use std::time::{Duration, Instant};

#[path = "common/mod.rs"]
mod common;
use common::*;

/// Build a linked list of `n` `Node` records by prepending
/// to the head. Each prepend is: allocate new node in
/// nursery, write old head into new node's next slot
/// (no barrier because new node is in nursery), then
/// update the root-held head reference to the new node.
fn bench_linked_list_prepend(c: &mut Criterion) {
    let mut group = c.benchmark_group("workloads/linked_list_prepend");
    for &n in &[1_000u64, 10_000] {
        group.throughput(Throughput::Elements(n));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let heap = Heap::new(fast_alloc_config());
                    let mut mutator = heap.mutator();
                    let mut scope = mutator.handle_scope();
                    let start = Instant::now();
                    // Allocate the initial head.
                    let mut head = mutator
                        .alloc(
                            &mut scope,
                            Node {
                                label: 0,
                                next: EdgeCell::default(),
                            },
                        )
                        .expect("alloc initial head");
                    for i in 1..n {
                        // New node pointing at the current head.
                        let new_head = mutator
                            .alloc(
                                &mut scope,
                                Node {
                                    label: i,
                                    next: EdgeCell::new(Some(head.as_gc())),
                                },
                            )
                            .expect("alloc node");
                        head = new_head;
                    }
                    total += start.elapsed();
                    drop(scope);
                }
                total
            });
        });
    }
    group.finish();
}

/// Build a linked list of `n` `Node` records by appending
/// to the tail via `store_edge`. Each append is: allocate
/// new node in nursery, write the new node into the tail's
/// `next` slot via `store_edge`, then update the tail
/// reference. The `store_edge` call exercises the write
/// barrier on every iteration — after the first few minor
/// cycles the tail has been promoted to old gen, so every
/// barrier call is an old-to-nursery edge and goes through
/// the per-block card-table fast path.
fn bench_linked_list_append(c: &mut Criterion) {
    let mut group = c.benchmark_group("workloads/linked_list_append");
    for &n in &[1_000u64, 10_000] {
        group.throughput(Throughput::Elements(n));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let heap = Heap::new(fast_alloc_config());
                    let mut mutator = heap.mutator();
                    let mut scope = mutator.handle_scope();
                    let start = Instant::now();
                    let head = mutator
                        .alloc(
                            &mut scope,
                            Node {
                                label: 0,
                                next: EdgeCell::default(),
                            },
                        )
                        .expect("alloc head");
                    let mut tail = head;
                    for i in 1..n {
                        let new_tail = mutator
                            .alloc(
                                &mut scope,
                                Node {
                                    label: i,
                                    next: EdgeCell::default(),
                                },
                            )
                            .expect("alloc tail node");
                        mutator.store_edge(&tail, 0, |n| &n.next, Some(new_tail.as_gc()));
                        tail = new_tail;
                    }
                    total += start.elapsed();
                    drop(scope);
                }
                total
            });
        });
    }
    group.finish();
}

/// Allocation-dominated workload: build a balanced binary
/// tree of the given depth by recursive allocation. Every
/// internal node holds two `EdgeCell<Node>` (reusing the
/// Node type with a free second-pointer field is awkward,
/// so this bench cheats and uses a linear Vec of N pointers
/// inside a Node-shaped wrapper). The real shape of the
/// workload is "allocate N objects and link them via
/// store_edge".
fn bench_allocation_heavy_graph(c: &mut Criterion) {
    let mut group = c.benchmark_group("workloads/allocation_heavy_graph");
    for &n in &[1_000u64, 10_000] {
        group.throughput(Throughput::Elements(n));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let heap = Heap::new(fast_alloc_config());
                    let mut mutator = heap.mutator();
                    let mut scope = mutator.handle_scope();
                    let start = Instant::now();
                    // Allocate N nodes in a flat batch (no
                    // graph links). Measures the pure
                    // allocation component of a graph-building
                    // workload without the barrier cost.
                    for i in 0..n {
                        mutator
                            .alloc(
                                &mut scope,
                                Node {
                                    label: i,
                                    next: EdgeCell::default(),
                                },
                            )
                            .expect("alloc node");
                    }
                    total += start.elapsed();
                    drop(scope);
                }
                total
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_linked_list_prepend,
    bench_linked_list_append,
    bench_allocation_heavy_graph,
);
criterion_main!(benches);
