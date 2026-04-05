# Value-Native Reader Design

## Problem

Neomacs's parser (`parser.rs`, 1233 lines) produces `Expr` — a Rust AST enum. This
requires an `Expr→Value` conversion step (`quote_to_runtime_value`), an `OpaqueValuePool`
for embedding Values in Expr trees, and a `value_to_expr` round-trip after macro expansion.
GNU Emacs's reader (`lread.c`) produces `Lisp_Object` directly — no intermediate AST.

The Expr layer is the root cause of:
- Stack overflow during bootstrap (nested GC scopes per eager expansion level)
- `.neobc` cache complexity (caching Expr forms to avoid re-parsing)
- `OpaqueValuePool` (bridging Values into Expr trees)
- `macro_expansion_cache` (caching expansions to avoid Expr→Value re-conversion)
- ~5500 lines of dead code (dual Expr/Value eval paths, conversion functions)

## Goal

Replace the Expr-based parser with a Value-native reader that produces `Value` directly,
matching GNU Emacs's `read0` in `lread.c`. Then use it in a streaming `readevalloop`
that reads one form, evaluates it, reads the next — exactly like GNU's `readevalloop`.

## Architecture

```
CURRENT:
  .el file → parser.rs → Vec<Expr> (all forms)
    → for each Expr:
      → source_literal_to_runtime_value → Value
      → eager_expand_toplevel_forms (recursive, GC scope per level)
      → macro_expansion_cache check
      → eval_sub(Value)
      → neobc cache write

NEW (matches GNU):
  .el file → reader.rs → Value (one form)
    → readevalloop_eager_expand_eval (flat, one GC scope)
    → eval_sub(Value)
    → read next form
```

## New Module: reader.rs

### Public API

```rust
/// Read one Lisp form from input starting at `pos`.
/// Returns the Value and the byte offset after the form.
/// Returns None at end of input.
///
/// Requires `&mut Context` because reading allocates on the tagged heap
/// (cons cells, strings, vectors, symbols).
pub fn read_one(
    eval: &mut Context,
    input: &str,
    pos: usize,
) -> Result<Option<(Value, usize)>, ReadError>

/// Read all forms from input (convenience for tests).
pub fn read_all(
    eval: &mut Context,
    input: &str,
) -> Result<Vec<Value>, ReadError>
```

### Value construction per syntax

| Syntax | Reader produces |
|--------|----------------|
| `42` | `Value::fixnum(42)` |
| `3.14` | `Value::make_float(3.14)` |
| `"hello"` | `Value::string("hello")` |
| `foo` | `Value::symbol(intern("foo"))` |
| `:key` | `Value::keyword("key")` |
| `?a` | `Value::fixnum(97)` |
| `t` | `Value::T` |
| `nil` | `Value::NIL` |
| `(a b c)` | cons chain: `cons(a, cons(b, cons(c, NIL)))` |
| `(a . b)` | `cons(a, b)` |
| `[a b c]` | `Value::make_vector(vec![a, b, c])` |
| `'x` | `cons(quote, cons(x, NIL))` |
| `` `x `` | `cons(backquote, cons(x, NIL))` |
| `,x` | `cons(comma, cons(x, NIL))` |
| `,@x` | `cons(comma-at, cons(x, NIL))` |
| `#'x` | `cons(function, cons(x, NIL))` |
| `#[...]` | byte-code vector literal |
| `#s(hash-table ...)` | hash table literal |
| `#N=EXPR` / `#N#` | shared structure (read labels) |

### GC Safety During Reading

While building a list `(a b c)`, the reader allocates `a`, then `b`, then `c`, then
builds cons cells. If GC runs between allocating `a` and building the cons, `a` could
be collected.

Solution: root intermediate values via `eval.push_temp_root(value)` while building
compound structures. Pop when the compound value is complete (the cons chain itself
keeps the elements alive).

For deeply nested structures, the rooting is implicit — the outer cons keeps inner
values alive once constructed. Only the currently-being-built level needs explicit rooting.

### Implementation Strategy

The reader reuses the same parsing logic as `parser.rs` (whitespace skipping, string
escape handling, number parsing, hash syntax) but outputs `Value` instead of `Expr`.
This is a mechanical translation:

```rust
// parser.rs (current)
fn parse_list_or_dotted(&mut self) -> Result<Expr, ParseError> {
    let mut items = Vec::new();
    loop {
        // ... read items ...
        items.push(self.parse_expr()?);
    }
    Ok(Expr::List(items))
}

// reader.rs (new)
fn read_list_or_dotted(&mut self, eval: &mut Context) -> Result<Value, ReadError> {
    let mut items = Vec::new();
    loop {
        // ... read items ...
        let item = self.read_form(eval)?;
        eval.push_temp_root(item);
        items.push(item);
    }
    let result = Value::list(items);
    for _ in &items { eval.pop_temp_root(); }
    Ok(result)
}
```

## Streaming readevalloop in load.rs

### New load loop

```rust
fn readevalloop(
    eval: &mut Context,
    content: &str,
    macroexpand_fn: Option<Value>,
) -> Result<(), EvalError> {
    let mut pos = 0;
    loop {
        let Some((form, next_pos)) = reader::read_one(eval, content, pos)? else {
            break;
        };
        pos = next_pos;

        if let Some(mexp) = macroexpand_fn {
            // GNU's readevalloop_eager_expand_eval:
            // 1. One-level macroexpand
            // 2. If progn, iterate subforms (not recurse)
            // 3. Full macroexpand + eval
            readevalloop_eager_expand_eval(eval, form, mexp)?;
        } else {
            // .elc path: no macro expansion needed
            eval.eval_sub(form)?;
        }

        eval.gc_safe_point_exact();
    }
    Ok(())
}
```

### Eager expansion (matching GNU lread.c:2136)

```rust
fn readevalloop_eager_expand_eval(
    eval: &mut Context,
    form: Value,
    macroexpand: Value,
) -> Result<Value, EvalError> {
    // Step 1: one-level expand
    let expanded = eval.apply(macroexpand, vec![form, Value::NIL])?;

    // Step 2: if (progn ...), iterate subforms
    if expanded.is_cons() && expanded.cons_car().is_symbol_named("progn") {
        let mut cursor = expanded.cons_cdr();
        while cursor.is_cons() {
            let subform = cursor.cons_car();
            readevalloop_eager_expand_eval(eval, subform, macroexpand)?;
            cursor = cursor.cons_cdr();
        }
        return Ok(Value::NIL);
    }

    // Step 3: full expand then eval
    let fully_expanded = eval.apply(macroexpand, vec![expanded, Value::T])?;
    eval.eval_sub(fully_expanded)
}
```

This matches GNU's `readevalloop_eager_expand_eval` exactly. No macro cache, no neobc,
no source_literal_cache. Just expand and eval.

## What Stays (temporarily)

- `parser.rs` — still used by `file_compile.rs`, tests, neobc cache
- `Expr` enum — still used by above
- `quote_to_runtime_value` — still used by file_compile.rs
- `eval_expr` — still used by some test paths

These become dead code cleanup targets (sub-project 3) after the reader is proven
working via bootstrap.

## What's Removed After Reader Works

| Component | Lines | Reason |
|-----------|-------|--------|
| `source_literal_cache` | ~30 | No Expr→Value caching needed |
| `OpaqueValuePool` + `Expr::OpaqueValueRef` | ~80 | No Values in Expr trees |
| `value_to_expr()` | ~60 | No round-trip conversion |
| `quote_to_runtime_value()` / `quote_to_value()` | ~150 | Reader produces Value directly |
| `eval_expr()` / `eval_inner()` / `eval_list()` | ~500 | Expr eval path dead |
| 19 `sf_*()` Expr special form handlers | ~1500 | Value handlers are sole path |
| `try_special_form_id()` Expr dispatch | ~50 | Only Value dispatch needed |
| `eager_expand_toplevel_forms()` in load.rs | ~120 | Replaced by GNU-style expand |
| `macro_expansion_cache` | ~50 | GNU doesn't cache |
| `neobc` cache system | ~2800 | No on-disk expansion cache |
| `eval_runtime_form()` | ~10 | Direct eval_sub instead |
| `eval_generated_loaddefs_form()` fast path | ~100 | GNU doesn't special-case |
| **Total** | **~5500** | |

## Success Criteria

1. `reader::read_one` reads all syntax that `parser.rs` supports
2. `load.rs` uses streaming `readevalloop` with `read_one` for .el files
3. Bootstrap (`neovm_loadup_bootstrap` test) progresses past the stack overflow
4. .elc loading still works (uses existing path, no reader change)
5. All existing tests pass

## Phased Implementation

### Phase 1: Create reader.rs

- Mechanical translation of parser.rs: same logic, outputs Value instead of Expr
- `read_one(eval, input, pos)` API
- Tests: verify read_one produces correct Values for all syntax
- parser.rs stays untouched

### Phase 2: Streaming readevalloop

- New load path in load.rs using `reader::read_one`
- `readevalloop_eager_expand_eval` matching GNU
- Switch .el loading to use the new path
- Verify bootstrap progresses further

### Phase 3: Dead code removal

- Remove Expr eval path, OpaqueValuePool, caches
- ~5500 lines deleted
- All tests updated to use reader instead of parser
