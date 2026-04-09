//! Multi-mutator scaling benchmarks.
//!
//! Measures aggregate allocation throughput across N
//! concurrent mutator threads. Each thread builds its own
//! `Mutator` against a shared `Heap` and allocates into
//! its own `MutatorLocal::tlab`. The only coordination
//! point is the `Arc<RwLock<HeapCore>>` write lock that
//! every allocation briefly acquires for bookkeeping.
//!
//! The expected shape (post-Stage-0 fix, current
//! single-lock architecture):
//!
//! * **1 thread**: uncontended baseline. Throughput is
//!   close to the single-mutator bench from
//!   `alloc_throughput.rs`.
//! * **2 threads**: the write lock serializes the
//!   bookkeeping portion of every alloc. Aggregate
//!   throughput is sub-linear — scaling factor roughly
//!   1.2-1.8x depending on the lock acquisition cost
//!   relative to the TLAB bump cost.
//! * **4 threads**: contention dominates. Scaling factor
//!   may be below 1.0x (i.e., 4 threads slower than 1).
//!
//! A regression that *improves* the 2/4/8 thread numbers
//! indicates the fine-grained locks work (DESIGN.md
//! Appendix A step 9) is paying off. A regression that
//! *worsens* them flags the lock scope leaking outside
//! its intended bounds.
//!
//! Runs with `cargo bench --bench multi_mutator_scaling -p
//! neovm-gc`.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use neovm_gc::Heap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[path = "common/mod.rs"]
mod common;
use common::*;

/// Allocations per worker thread per iteration. Chosen to
/// be large enough that thread-startup cost doesn't
/// dominate, but small enough that the bench completes
/// quickly.
const ALLOCS_PER_THREAD: u64 = 10_000;

fn bench_multi_mutator_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("multi_mutator_scaling/alloc");
    for &n_threads in &[1usize, 2, 4, 8] {
        let total = (n_threads as u64) * ALLOCS_PER_THREAD;
        group.throughput(Throughput::Elements(total));
        group.bench_with_input(
            BenchmarkId::from_parameter(n_threads),
            &n_threads,
            |b, &n| {
                b.iter_custom(|iters| {
                    let mut total_elapsed = Duration::ZERO;
                    for _ in 0..iters {
                        let heap = Arc::new(Heap::new(fast_alloc_config()));
                        let start = Instant::now();
                        let mut handles = Vec::with_capacity(n);
                        for worker_id in 0..n {
                            let heap = Arc::clone(&heap);
                            handles.push(thread::spawn(move || {
                                let mut mutator = heap.mutator();
                                let mut scope = mutator.handle_scope();
                                for i in 0..ALLOCS_PER_THREAD {
                                    let label =
                                        (worker_id as u64) * 1_000_000 + i;
                                    mutator
                                        .alloc(&mut scope, SmallLeaf(label))
                                        .expect("alloc");
                                }
                            }));
                        }
                        for h in handles {
                            h.join().expect("join worker");
                        }
                        total_elapsed += start.elapsed();
                    }
                    total_elapsed
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_multi_mutator_scaling);
criterion_main!(benches);
