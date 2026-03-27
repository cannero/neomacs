# NeoVM Compatibility Oracle Scaffold

This directory provides a minimal GNU Emacs oracle harness for compatibility testing.

## Goal

Capture canonical GNU Emacs results for a corpus of Elisp forms, then compare NeoVM
results against that baseline once evaluator execution is wired in.

## Files

- `oracle_eval.el`: batch-mode evaluator used as the GNU Emacs oracle
- `run-oracle.sh`: runs all forms from a corpus file, prints TSV output, and surfaces oracle stderr/stdout diagnostics when prefixed case output is missing
- `run-neovm.sh`: runs NeoVM worker-runtime compatibility runner and prints TSV output
- `compare-results.sh`: diffs oracle TSV vs NeoVM TSV
- `check-tracked-lists.sh`: validates `cases/tracked-lists.txt` manifest shape and references
- `check-builtin-registry-sync.sh`: checks `builtin_registry.rs` exactly matches names dispatched by `builtins.rs`
- `check-builtin-registry-fboundp.sh`: checks `fboundp` parity for all names in `builtin_registry.rs`
- `check-builtin-registry-func-arity.sh`: checks `func-arity` parity for all core names in `builtin_registry.rs`
- `check-builtin-registry-autoload-metadata.sh`: checks startup autoload metadata tuple parity (`fboundp`, autoload file, autoload docstring first line, autoload interactive slot, autoload type slot) for all core names in `builtin_registry.rs`
- `check-builtin-registry-primitive-any-coverage.sh`: checks oracle `primitive-any` builtin universe coverage is fully represented by both NeoVM runtime and `builtin_registry.rs`
- `check-startup-doc-stub-coverage.sh`: checks startup integer-doc symbol coverage of `STARTUP_VARIABLE_DOC_STUBS`
- `check-startup-doc-string-coverage.sh`: checks startup string-doc symbol coverage of `STARTUP_VARIABLE_DOC_STRING_PROPERTIES`
- `check-startup-variable-documentation-counts.sh`: checks startup `variable-documentation` property-count and runtime-resolution count parity (oracle vs NeoVM)
- `report-oracle-builtin-coverage.sh`: reports oracle builtin universe size (default: primitive subrs) and current coverage by both NeoVM runtime and `builtin_registry.rs` core names
- `cases/startup-doc-stub-extra-allowlist.txt`: allowlisted startup doc stubs intentionally beyond oracle integer-doc set
- `bench-load-cache.sh`: runs cold/warm/post-edit `.neoc` load benchmark reporting via `load_cache_bench`
- `cases/default.list`: default `check-all-neovm` corpus order (one case per line)
- `cases/neovm-only.list`: NeoVM-only policy corpus order
- `cases/legacy-elc-literal.list`: opt-in `.elc` literal compatibility corpus order
- `cases/introspection.list`: focused callable/special-form introspection corpus order
- `cases/thread.list`: focused thread primitive corpus order
- `cases/startup-doc.list`: focused startup documentation parity corpus order
- `cases/tracked-lists.txt`: source-of-truth list-of-lists used by inventory/progress validation
- `cases/builtin-registry-fboundp-allowlist.txt`: intentional `fboundp` drift allowlist for registry parity checks
- `cases/builtin-registry-func-arity-allowlist.txt`: intentional `func-arity` drift allowlist for registry parity checks
- `cases/builtin-registry-autoload-metadata-allowlist.txt`: intentional startup autoload metadata drift allowlist for registry parity checks
- `cases/builtin-registry-sync-allowlist.txt`: intentional evaluator-dispatch names excluded from core registry startup policy
- `cases/core.forms`: starter corpus for expression and error behavior
- `cases/input-batch-readers.forms`: batch-mode input reader compatibility corpus

## Usage

```bash
test/neovm/vm-compat/run-oracle.sh test/neovm/vm-compat/cases/core.forms
test/neovm/vm-compat/run-neovm.sh test/neovm/vm-compat/cases/core.forms
```

Use official Emacs explicitly by setting `NEOVM_FORCE_ORACLE_PATH`, for example:

```bash
NEOVM_FORCE_ORACLE_PATH=/nix/store/hql3zwz5b4ywd2qwx8jssp4dyb7nx4cb-emacs-30.2/bin/emacs \
  test/neovm/vm-compat/run-oracle.sh test/neovm/vm-compat/cases/core.forms
```

Output columns:

1. source line number
2. input form
3. oracle result (`OK <value>` or `ERR <signal+data>`)

## Baseline Workflow

```bash
cd test/neovm/vm-compat
make record   # writes cases/core.expected.tsv
make check    # diffs fresh oracle output against expected
```

When NeoVM produces TSV output for the same corpus:

```bash
cd test/neovm/vm-compat
make compare NEOVM_OUT=cases/core.neovm.tsv
make check-neovm
```

`compare-results.sh` checks index/form/result equality and reports drift.
Set `STRICT_FORM=1` to fail on form-printing differences too.

Run all checked-in corpora in one shot:

```bash
cd test/neovm/vm-compat
make check-list-integrity
make validate-case-lists
make check-all
make check-all-neovm
```

When adding a new case list file, register it in `cases/tracked-lists.txt` so
`validate-case-lists`, `case-inventory`, and `compat-progress` all include it.

`case-inventory` fails on unreferenced `.forms` files by default; set
`NEOVM_ALLOW_UNREFERENCED_FORMS=1` when you only want a non-failing report.

List default case order without running:

```bash
cd test/neovm/vm-compat
make list-cases
```

Get a compact status snapshot (case counts + explicit stub count + startup doc coverage + startup variable-doc property/runtime-resolution count parity + builtin registry counts + oracle builtin coverage (registry + runtime) + allowlist size):

```bash
cd test/neovm/vm-compat
make compat-progress
```

Emit the same snapshot in JSON (useful for scripts/dashboards):

```bash
cd test/neovm/vm-compat
make compat-progress-json
```

Report oracle builtin universe coverage directly (including first-page previews
for registry and runtime gaps):

```bash
cd test/neovm/vm-compat
make report-oracle-builtin-coverage
```

Select the oracle builtin universe mode when needed (use either
`ORACLE_BUILTIN_UNIVERSE` or `ORACLE_BUILTIN_UNIVERSE_MODE`; if both are set,
`ORACLE_BUILTIN_UNIVERSE` takes precedence):

- `ORACLE_BUILTIN_UNIVERSE=primitive-any` (default): primitive subrs + primitive special forms
- `ORACLE_BUILTIN_UNIVERSE=primitive-subr`: primitive subrs only (`subr-primitive-p` and not special forms)
- `ORACLE_BUILTIN_UNIVERSE=subr-or-special`: broad startup surface (`subr` or `special-form`)

List explicit comment-annotated function stubs in the Rust Elisp modules
for quick daily progress tracking:

```bash
cd test/neovm/vm-compat
make compat-stub-index
```

You can also emit JSON when you want machine-readable snapshots for dashboards:

```bash
cd test/neovm/vm-compat
make compat-stub-index-json
```

Enforce a zero-stub budget for explicit compatibility markers
(set `NEOVM_STUB_BUDGET` to a non-zero value if you intentionally keep
some markers):

```bash
cd test/neovm/vm-compat
NEOVM_STUB_BUDGET=0 make check-stub-budget
```

Validate startup integer-doc coverage against the GNU Emacs oracle
(reports counts, fails on missing startup stubs, and enforces an allowlisted
extra-stub set):

```bash
cd test/neovm/vm-compat
make check-startup-doc-stub-coverage
```

Validate startup string-doc coverage against the GNU Emacs oracle
(reports counts, fails on missing startup string docs, and enforces exact
set equality with `STARTUP_VARIABLE_DOC_STRING_PROPERTIES`):

```bash
cd test/neovm/vm-compat
make check-startup-doc-string-coverage
```

Validate startup runtime `variable-documentation` integer/string type counts
for both:
- raw startup property types (`get`)
- runtime doc resolution (`documentation-property`)

against both the oracle baseline and NeoVM runtime:

```bash
cd test/neovm/vm-compat
make check-startup-variable-documentation-counts
```

Set `SHOW_EXTRA_STUBS=1` to print startup stub symbols that are currently
not required by oracle integer-doc metadata.

By default the check expects any extra startup stubs to be listed in:

- `cases/startup-doc-stub-extra-allowlist.txt`

The check fails on:

- missing startup stubs (oracle symbol not present in `STARTUP_VARIABLE_DOC_STUBS`)
- unexpected extra startup stubs (extra symbol not in allowlist)
- stale allowlist entries (allowlisted symbol no longer extra)

Current expected steady state is an empty allowlist (`extra startup stubs: 0`).

Run any case list file directly (avoids passing very long `CASES=...` values):

```bash
cd test/neovm/vm-compat
make check-neovm-list LIST=cases/thread.list
make check-neovm-list LIST=cases/startup-doc.list
make check-list LIST=cases/introspection.list
make record-list LIST=cases/default.list
```

Run a regex-filtered subset from a list (fast iteration without editing list files):

```bash
cd test/neovm/vm-compat
make list-cases-filter LIST=cases/default.list PATTERN='command-remapping|key-binding'
make check-list-filter LIST=cases/default.list PATTERN='command-remapping|key-binding'
make check-neovm-filter LIST=cases/default.list PATTERN='command-remapping|key-binding'
make check-neovm-filter-strict LIST=cases/default.list PATTERN='command-remapping|key-binding'
make record-list-filter LIST=cases/default.list PATTERN='command-remapping|key-binding'
```

Run one specific case:

```bash
cd test/neovm/vm-compat
make check-one-neovm CASE=cases/symbol-function-core
```

Run the opt-in legacy `.elc` literal compatibility corpora (non-default):

```bash
cd test/neovm/vm-compat
make check-legacy-elc-neovm
```

Validate runner feature-stamp flip behavior (`default -> legacy -> default`):

```bash
cd test/neovm/vm-compat
make check-runner-feature-stamp
```

Run the focused callable-introspection suite (faster loop for `fboundp`/`symbol-function`/`indirect-function`/`functionp`/`macrop`/`func-arity`):

```bash
cd test/neovm/vm-compat
make check-introspection-neovm
```

Run the focused thread primitive suite (faster loop for `make-thread`/`thread-join`/`thread-last-error`/`thread-signal`/mutex/condition-variable semantics):

```bash
cd test/neovm/vm-compat
make check-thread-neovm
```

Run the focused startup-doc parity suite (startup doc oracle corpus + startup doc coverage/count gates):

```bash
cd test/neovm/vm-compat
make check-startup-doc-neovm
```

Run the builtin registry `fboundp` parity gate (GNU Emacs `-Q` vs NeoVM core builtin names):

```bash
cd test/neovm/vm-compat
make check-builtin-registry-fboundp
```

Run the builtin registry `func-arity` parity gate (GNU Emacs `-Q` vs NeoVM core builtin names):

```bash
cd test/neovm/vm-compat
make check-builtin-registry-func-arity
```

Run the builtin registry startup autoload-metadata parity gate (GNU Emacs `-Q` vs NeoVM core builtin names):

```bash
cd test/neovm/vm-compat
make check-builtin-registry-autoload-metadata
```

Run the full builtin registry gate bundle (dispatch/registry sync + fboundp/function-cell/func-arity/autoload-metadata/function-kind/commandp/primitive-any-coverage/extension-policy checks + extension policy case-coverage checks):

```bash
cd test/neovm/vm-compat
make check-builtin-registry-all
```

Run the builtin registry primitive-any coverage gate (primitive subrs + primitive special forms, oracle vs NeoVM runtime/registry):

```bash
cd test/neovm/vm-compat
make check-builtin-registry-primitive-any-coverage
```

Run only the dispatch/registry sync gate (uses `cases/builtin-registry-sync-allowlist.txt` for intentional startup-policy exclusions):

```bash
cd test/neovm/vm-compat
make check-builtin-registry-sync
```

Current intentional sync exclusions:
- none (`cases/builtin-registry-sync-allowlist.txt` is intentionally empty)

Show all currently allowlisted `function-kind` drift entries in detail (useful when
triaging `function-kind` allowlist scope):

```bash
cd test/neovm/vm-compat
make show-function-kind-drifts
```

Run the oracle default-path guard (ensures runner scripts stay pinned to the
hardcoded GNU Emacs oracle binary):

```bash
cd test/neovm/vm-compat
make check-oracle-default-path
```

Run the tracked list/inventory preflight (manifest validity + per-list file
checks + unreferenced-forms guard):

```bash
cd test/neovm/vm-compat
make check-list-integrity
```

Run the ERT allowlist oracle scaffold (for upstream differential bootstrapping):

```bash
cd test/neovm/vm-compat
make check-ert-allowlist
```

The oracle runner defaults to this hardcoded GNU Emacs binary:

```bash
/nix/store/hql3zwz5b4ywd2qwx8jssp4dyb7nx4cb-emacs-30.2/bin/emacs
```

Override with `NEOVM_FORCE_ORACLE_PATH` only when you intentionally need a
different oracle binary:

```bash
cd test/neovm/vm-compat
NEOVM_FORCE_ORACLE_PATH=/nix/store/hql3zwz5b4ywd2qwx8jssp4dyb7nx4cb-emacs-30.2/bin/emacs make check-ert-allowlist
```

The default fixture uses:

- allowlist file: `cases/ert-allowlist-smoke.txt`
- loaded test file: `cases/ert-allowlist-fixtures/smoke-tests.el`
- baseline output: `cases/ert-allowlist-smoke.expected.tsv`

You can override all three via `ERT_ALLOWLIST`, `ERT_LOAD_FILES`, and `ERT_EXPECTED`.

`run-neovm.sh` executes the built `elisp_compat_runner` binary directly and rebuilds it
only when relevant Rust sources are newer than the binary. Set
`NEOVM_WORKER_CARGO_FEATURES` to compile/run opt-in worker features, e.g.
`NEOVM_WORKER_CARGO_FEATURES=legacy-elc-literal`.

NeoVM-only policy cases (expected to diverge from GNU Emacs oracle baselines)
can be run separately:

```bash
cd test/neovm/vm-compat
make check-all-neovm-only
```

To refresh NeoVM-only expected baselines from NeoVM output directly:

```bash
cd test/neovm/vm-compat
make record-all-neovm-only
```

To refresh one case from NeoVM output:

```bash
cd test/neovm/vm-compat
make record-one-neovm CASE=cases/neovm-precompile-arg-errors-semantics
```

Use `record-one-neovm` only when you intentionally want NeoVM output to become
the expected baseline for that case.

Current NeoVM-only policy cases include source-only loading behavior (`.elc`
rejection and `.neoc` fallback safety) plus NeoVM extension behavior
(`neovm-precompile-file` cache warming, argument/error contracts, and compiled
artifact rejection, directory-input rejection, parse-error/no-cache semantics,
and default-build `#[...]` literal non-callability policy
(`cases/bytecode-literal-default-policy`).

### Extension policy notes

The extension policy currently includes:

- `string-chop-newline`
- `string-fill`
- `string-limit`
- `string-pad`

These symbols are intentional NeoVM extension symbols, not part of GNU Emacs
core compatibility.

- Oracle expectation: `(fboundp '<symbol>)` is `nil`
- NeoVM expectation: `(fboundp '<symbol>)` is `t`

The policy is enforced by:

- `cases/builtin-registry-extension-policy.txt` (declared extension set)
- `check-builtin-registry-extension-policy.sh` (oracle-vs-neovm `fboundp` gate)
- `check-builtin-registry-extension-case-coverage.sh` (policy symbols must be exercised by `cases/neovm-only.list` forms)
- `cases/neovm-precompile-function-cell-semantics` (function-cell shape lock-in)
- `cases/precompile` (runtime extension behavior lock-in)

Run the extension policy gate directly:

```bash
cd test/neovm/vm-compat
make check-builtin-registry-extension-policy
```

You can also precompile source files into NeoVM cache sidecars ahead of load:

```bash
cargo run --manifest-path rust/neovm-core/Cargo.toml --example precompile_neoc -- \
  path/to/file.el [path/to/another.el ...]
```

Run cache-load benchmark reporting cold miss, warm hit, and post-edit rebuild timing:

```bash
cd test/neovm/vm-compat
make bench-load-cache
# or override:
make bench-load-cache BENCH_SOURCE=cases/load-policy-fixtures/vm-policy-cache-probe.el BENCH_ITERS=200
```

The benchmark output includes `cold_load_ms`, `warm_load_ms`,
`warm_avg_ms(iterations=...)`, and `post_edit_rebuild_ms`.

## Batch Freeze Notes (2026-02-13)

- Queue slice completed and frozen: commits `7a688f4a` through `8de23c4f`.
- Introspection behavior now oracle-guarded for predicate boundaries, function-cell lookup, alias traversal, and arity/error signaling edges.
- Added fast introspection gate target: `make check-introspection-neovm`.
- Required periodic full gate for this batch was run: `make check-all-neovm`.

Post-freeze updates:

- Added builtin registry `fboundp` parity gate:
  - `make check-builtin-registry-fboundp`
  - core-only parity (policy-declared extension names are excluded)
  - allowlist file for remaining core drifts: `cases/builtin-registry-fboundp-allowlist.txt`
- Added CI gate step for builtin registry parity in `.github/workflows/vm-compat.yml`.
- Added `cases/input-batch-readers` corpus and wired it into default `check-all-neovm` coverage.
- Added CI gate job for `make check-ert-allowlist` in `.github/workflows/vm-compat.yml`.
- Added reader stream compatibility cases: `cases/read-from-string-edges` and `cases/read-stream-semantics`.
- Added obarray argument compatibility cases: `cases/intern-obarray-semantics` and `cases/obarray-arg-semantics`.
- Added string primitive compatibility cases:
  - `cases/split-string-semantics`
  - `cases/make-string-semantics`
  - `cases/make-string-raw-byte-semantics`
  - `cases/make-string-nonunicode-semantics`
  - `cases/string-print-unicode-semantics`
  - `cases/string-nonunicode-char-semantics`
  - `cases/string-nonunicode-indexing-semantics`
  - `cases/string-nonunicode-concat-semantics`
  - `cases/string-nonunicode-sequence-semantics`
  - `cases/string-concat-error-paths`
  - `cases/string-trim-semantics`
  - `cases/string-prefix-suffix-semantics`
  - `cases/string-join-semantics`
  - `cases/string-to-number-semantics`
  - `cases/substring-edge-semantics`
- Added append/vconcat sequence compatibility cases:
  - `cases/append-vconcat-error-paths`
  - `cases/append-tail-object-semantics`
  - `cases/vconcat-mixed-sequence-semantics`
- Added destructive sequence mutation compatibility case:
  - `cases/nreverse-destructive-semantics`
- Added destructive list concatenation compatibility case:
  - `cases/nconc-destructive-semantics`
- Added destructive element-removal compatibility case:
  - `cases/delete-delq-semantics`
- Added destructive ordering compatibility case:
  - `cases/sort-semantics`
- Added list tail/suffix compatibility case:
  - `cases/last-butlast-semantics`
  - includes arg normalization edges (`N=nil`, `number-or-marker-p`, float payload signaling)
- Added list indexing/tail-walk compatibility case:
  - `cases/nth-nthcdr-semantics`
- Added sequence element accessor compatibility case:
  - `cases/elt-semantics`
- Added array index typing compatibility case:
  - `cases/aref-aset-index-semantics`
- Added sequence copy compatibility case:
  - `cases/copy-sequence-semantics`
- Added list membership/alist lookup compatibility case:
  - `cases/member-assoc-semantics`
- Added `alist-get` traversal compatibility case:
  - `cases/alist-get-semantics`
- Added property-list mutation/error compatibility case:
  - `cases/plist-semantics`
- Added hash-table option parsing/accessor compatibility case:
  - `cases/hash-make-table-options-semantics`
- Added hash-table rehash/copy compatibility case:
  - `cases/hash-rehash-copy-semantics`
- Added dynamic-binding/unwind restoration compatibility case:
  - `cases/specpdl-dynamic-unwind-semantics`
- Added function unbinding fallback-boundary compatibility case:
  - `cases/fmakunbound-fallback-boundary`
- Added special-form function-cell override/restoration compatibility case:
  - `cases/fset-special-form-override-boundary`
- Added evaluator-callable function-cell override/restoration compatibility case:
  - `cases/fset-evaluator-callable-override-boundary`
- Added explicit `fset`-to-`nil` function-cell compatibility case:
  - `cases/fset-nil-function-cell-semantics`
- Added explicit `fset` non-callable function-cell compatibility case:
  - `cases/fset-noncallable-function-cell-semantics`
- Added explicit `fset` on symbol `t` function-cell compatibility case:
  - `cases/fset-t-function-cell-semantics`
- Added explicit `fset` on keyword-symbol function-cell compatibility case:
  - `cases/fset-keyword-function-cell-semantics`
- Added `fset` alias-cycle compatibility case for symbol-designator links (`symbol`/`keyword`/`t`):
  - `cases/fset-alias-cycle-symbol-designator-semantics`
- Added bytecode literal reader compatibility case (legacy opt-in):
  - `cases/bytecode-literal-reader-semantics`
- Added bytecode literal execution compatibility case (legacy opt-in):
  - `cases/bytecode-literal-exec-semantics`
- Added higher-order map primitive compatibility case:
  - `cases/map-family-semantics`
- Added `ignore` callable compatibility case:
  - `cases/ignore-semantics`
- Full NeoVM gate is green with these additions:
  - `make check-all-neovm`
- Added command dispatch and command identity compatibility cases:
  - `cases/commandp-builtin-command-matrix`
  - `cases/command-dispatch-default-arg-semantics`
  - `cases/transpose-words-semantics`
  - `cases/command-dispatch-line-motion-semantics`
  - `cases/command-prefix-state-return-shape`
  - `cases/keyboard-quit-command-semantics`
  - `cases/fboundp-builtin-command-matrix`
  - matrices now include baseline command names plus editing-command coverage (`backward-char`/`delete-char`/`kill-region`/`kill-ring-save`/`kill-whole-line`/`copy-region-as-kill`/`upcase-region`/`downcase-region`/`capitalize-region`/`upcase-initials-region`/`delete-indentation`/`indent-for-tab-command`/`yank`/`transpose-lines`/`transpose-words`/`scroll-up-command`/`scroll-down-command`/`recenter-top-bottom`), default-arg dispatch coverage (`delete-char`/`kill-word`/`backward-kill-word`/`downcase-word`/`upcase-word`/`capitalize-word`/`transpose-lines`/`transpose-words`/`kill-region`/`kill-ring-save`/`copy-region-as-kill` plus region-case dispatch details: `call-interactively` defaults for `upcase-region`/`downcase-region`/`capitalize-region`/`upcase-initials-region`, `command-execute` `args-out-of-range` quirk for `upcase-region`/`downcase-region`, and `command-execute` mark-dependent behavior for `capitalize-region`/`upcase-initials-region`) including region-marked dispatch and mark-missing error paths (`user-error` for `kill-region`, `error` for `kill-ring-save`/`copy-region-as-kill`), plus no-prefix interactive dispatch checks for `kill-whole-line`/`delete-indentation`/`indent-for-tab-command`, and line-motion dispatch includes both `command-execute` and `call-interactively` `other-window` no-frame fallback.
