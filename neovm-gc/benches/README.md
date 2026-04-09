# neovm-gc benchmarks

Criterion-based benchmark suite for `neovm-gc`. The suite exists to:

1. **Validate design claims** — DESIGN.md asserts the crate is generational, parallel in the nursery, concurrent in the old gen, and moving by default. These benches measure whether the running implementation actually delivers those properties.
2. **Catch regressions** — every bench has a committed baseline in `BASELINE.md`. A change that moves a number by more than ~10% has to explain why in the commit message.
3. **Focus optimization work** — absolute numbers tell you where the bottlenecks actually are, not where you assumed they were.

## Layout

```
benches/
├── common/mod.rs           # shared helpers (test types, heap configs)
├── alloc_throughput.rs     # single-mutator allocation rate vs object size
├── barrier_cost.rs         # write-barrier fast-path and short-circuits
├── collection_latency.rs   # minor + major GC pause distributions
├── multi_mutator_scaling.rs # aggregate throughput at 1/2/4/8 threads
├── BASELINE.md             # committed baseline numbers per machine
└── README.md               # this file
```

Each bench file contains one or more criterion `bench_function` / `bench_with_input` measurements, grouped under a stable name so baselines can be diffed across runs.

## Running

```bash
# Run a specific bench file
cargo bench --bench alloc_throughput -p neovm-gc

# Run with tighter sample counts (faster, noisier)
cargo bench --bench alloc_throughput -p neovm-gc -- --sample-size 10 --warm-up-time 1 --measurement-time 3

# Save a baseline for later comparison
cargo bench --bench alloc_throughput -p neovm-gc -- --save-baseline my-experiment

# Compare against a saved baseline
cargo bench --bench alloc_throughput -p neovm-gc -- --baseline my-experiment
```

Criterion writes HTML reports to `target/criterion/` automatically. Open `target/criterion/report/index.html` in a browser for plots.

## Reproducibility checklist

Benchmark numbers are only meaningful if the environment is stable. Before quoting numbers in a commit message or issue, verify:

1. **CPU frequency scaling.** Set the governor to `performance` and disable turbo boost for consistent frequencies:
   ```bash
   sudo cpupower frequency-set -g performance
   echo 0 | sudo tee /sys/devices/system/cpu/intel_pstate/no_turbo  # or similar
   ```
2. **Close other applications.** Especially browsers, VM hosts, and anything that might trigger preemption or page faults mid-run.
3. **Pin to specific cores.** Multi-threaded benches benefit from `taskset` to avoid scheduler jitter:
   ```bash
   taskset -c 0-7 cargo bench --bench multi_mutator_scaling -p neovm-gc
   ```
4. **Run twice.** If the two runs differ by more than ~5%, the environment is too noisy and results are unreliable.

## Profiling

When a bench number is surprising, drop into a profiler:

```bash
# Install once
cargo install flamegraph

# Generate a flamegraph for a specific bench
cargo flamegraph --bench alloc_throughput -p neovm-gc --release -- --bench
```

The flamegraph lands in `flamegraph.svg`. Open it in a browser and look for functions that eat more CPU than expected. The Stage 0 investigation (commit `390303d36`) is the canonical example of how profiling catches an O(n) algorithm hiding in an allocation hot path.

## What the individual benches measure

### `alloc_throughput.rs`

- `small_leaf/long_scope/{1000,10000,50000}` — allocate N `SmallLeaf` records inside one `HandleScope`. Measures the per-allocation cost when every allocation is kept alive for the batch duration.
- `small_leaf/scoped_batches/{1000,10000,50000}` — allocate N records but drop and recreate the `HandleScope` every 1000. Matches the shape of tight inner loops that discard allocated objects quickly.
- `medium_leaf/long_scope` — 10000 `MediumLeaf` (128-byte payload) allocations. Exercises the cost of copying a larger payload through the allocation path.
- `large_leaf/long_scope` — 1000 `LargeLeaf` (1 KiB payload) allocations. Still nursery-sized with the default `max_regular_object_bytes = 64 KiB` config.

**Regression flag:** if the `long_scope/50000` number gets significantly slower than `long_scope/1000` (e.g. > 20% higher per-element time), the allocation hot path is probably back to an O(n) walk. Check `collector_policy::build_plan` first — that's where Stage 0 found the quadratic loop.

### `barrier_cost.rs`

- `store_edge/no_new_value/short_circuit` — `store_edge` with `None` as the new value. The barrier should short-circuit on the "no managed reference" check.
- `store_edge/nursery_to_nursery/short_circuit` — `store_edge` from a nursery owner to a nursery target. The barrier should short-circuit on the "owner is not old-gen" check.

**Regression flag:** both of these should be dominated by lock acquisition, not by remembered-set maintenance. If either one gets slower, it means a barrier short-circuit stopped firing.

### `collection_latency.rs`

- `minor/small/drop_all` — minor cycle that reclaims 1000 dead nursery objects. Measures the cost of an empty-nursery collection.
- `minor/all_survive/1000_survivors` — minor cycle where every nursery object is reachable via a live `HandleScope`. Measures the cost of copying N survivors to the to-space (or promoting them to old gen).
- `major/small/1000_old_survivors` — major cycle on a small old-gen population. Measures the major mark+reclaim pipeline on realistic input.

**Regression flag:** these are the "pause time" numbers that matter for interactive workloads. A real VM using this crate will care about P50/P95/P99 of these numbers. Criterion reports the median + variance; for the tail percentiles, run the bench with `--sample-size 1000`.

### `multi_mutator_scaling.rs`

- `alloc/1`, `alloc/2`, `alloc/4`, `alloc/8` — aggregate allocation throughput at N concurrent mutator threads. Each thread allocates 10 000 `SmallLeaf` records into its own `MutatorLocal::tlab`. The scaling factor (throughput at N threads ÷ throughput at 1 thread) measures how badly the single `HeapCore` write lock serializes allocation bookkeeping.

**Expected shape (current single-lock architecture):** 1 thread is the uncontended baseline; 2 threads scale to roughly 0.8-0.9x per thread; 4 threads drop to 0.5-0.7x; 8 threads drop further. Any improvement vs these numbers is a sign that fine-grained locks landed (DESIGN.md Appendix A step 9). Any regression flags the lock scope leaking outside its intended bounds.

## Hypothesis-driven benching

The suite is designed to answer specific questions, not just to produce numbers. Before running a bench to validate a change, write down:

1. **What does the change intend to improve?**
2. **Which bench should move, and in which direction?**
3. **Which benches should *not* move?**

Then run the suite. If the benches that should move don't, or the benches that shouldn't do, the change doesn't do what you thought. This is how you catch accidentally-correlated effects like "fixing barriers also regressed allocations because the same inner loop was shared."
