# Symbol-with-Pos Design Spec

**Date:** 2026-04-16
**Goal:** Implement GNU Emacs's `symbol-with-pos` infrastructure so the byte-compiler can produce source-location warnings.

## Background

GNU Emacs's byte-compiler reads source files with `read-positioning-symbols`, which wraps every symbol in a `symbol-with-pos` object carrying the byte offset where the symbol was found. When the compiler emits a warning, it searches the form tree for the nearest symbol-with-pos and extracts the byte offset to produce `line:column` diagnostics.

Neomacs currently stubs `read-positioning-symbols` to delegate to plain `read`, losing all position information. This spec adds the full infrastructure matching GNU Emacs's design.

## 1. Data Structure

Add `SymbolWithPos = 14` to the `VecLikeType` enum in `header.rs`.

Define a new heap-allocated pseudo-vector:

```rust
#[repr(C)]
pub struct SymbolWithPosObj {
    pub header: VecLikeHeader,   // gc + type tag
    pub sym: Value,              // a bare symbol (GC-traced)
    pub pos: Value,              // a fixnum (GC-traced)
}
```

Both `sym` and `pos` are `Value` (matching GNU's `Lisp_Object` with LISPSIZE=2, RESTSIZE=0). This means the GC traces both fields, matching GNU exactly. The `sym` field always holds a bare symbol. The `pos` field always holds a fixnum (byte offset into source).

### Allocation

`build_symbol_with_pos(sym: Value, pos: Value) -> Value` allocates via the existing `alloc_veclike` path, sets the type tag to `VecLikeType::SymbolWithPos`, and initializes the two fields. Symbol-with-pos objects cannot be dumped to pdump — the pdump converter must error if it encounters one (they are ephemeral, created only during read/compile).

### ValueKind

Add a variant to `ValueKind`:

```rust
SymbolWithPos,  // discriminated via VecLikeType::SymbolWithPos
```

### Accessors on TaggedValue/Value

```rust
fn is_symbol_with_pos(&self) -> bool
fn as_symbol_with_pos(&self) -> Option<&SymbolWithPosObj>
fn as_symbol_with_pos_sym(&self) -> Option<Value>   // bare symbol
fn as_symbol_with_pos_pos(&self) -> Option<i64>     // fixnum as i64
```

## 2. Global Control Flag

`symbols-with-pos-enabled` — a Lisp boolean variable, default `nil`.

Store as a field on the `Context` (evaluator) for cheap access. The byte-compiler binds it to `t` during compilation via a `let` form in `bytecomp.el`.

Also add `print-symbols-bare` — a separate Lisp boolean variable, default `nil`. Controls whether the printer shows the bare symbol name or the full `#<symbol NAME at POS>` form.

## 3. Transparency Rules

All transparency is conditional on `symbols-with-pos-enabled` being non-nil.

### When flag = true (during byte-compilation)

| Operation | Behavior |
|-----------|----------|
| `symbolp` | Returns t for both bare symbols and symbol-with-pos |
| `eq` | Unwraps both operands to bare symbol before comparing |
| `eql` | Same as eq for symbol operands |
| `equal` | Unwraps both operands to bare symbol before comparing |
| Hash key (eq/eql test) | Unwraps to `HashKey::Symbol(bare_id)` |
| `symbol-name` | Unwraps, returns bare symbol's name |
| `symbol-value` | Unwraps, returns bare symbol's value |
| `symbol-function` | Unwraps, returns bare symbol's function |
| `set`, `fset`, `put`, `get` | Unwraps symbol argument |
| `intern`, `intern-soft` | Unwraps if given symbol-with-pos |
| `boundp`, `fboundp` | Unwraps symbol argument |
| `symbol-plist` | Unwraps symbol argument |
| Bytecode VM `Op::Eq` | Unwraps before comparing |

### When flag = false (normal operation)

Symbol-with-pos is an opaque pseudo-vector:
- `symbolp` returns nil
- `eq` does raw bit comparison (two symbol-with-pos objects are only eq if same pointer)
- `equal` compares structurally: both `sym` AND `pos` must match
- Hash tables hash by pointer, not by bare symbol

## 4. EQ Implementation

In `eq_value()` (`value.rs`):

```rust
pub fn eq_value(ctx: &Context, left: &Value, right: &Value) -> bool {
    if left.bits() == right.bits() {
        return true;
    }
    if !ctx.symbols_with_pos_enabled {
        return false;
    }
    // slow path: unwrap symbol-with-pos
    let l = if left.is_symbol_with_pos() { left.as_symbol_with_pos_sym().unwrap() } else { *left };
    let r = if right.is_symbol_with_pos() { right.as_symbol_with_pos_sym().unwrap() } else { *right };
    l.bits() == r.bits()
}
```

The fast path (flag=false) is a single branch after the bit comparison. The slow path only runs during byte-compilation.

The bytecode VM's `Op::Eq` handler must also use this function instead of raw bit comparison.

## 5. Hash Key Computation

In `to_eq_key()` (`value.rs`):

When `symbols_with_pos_enabled` is true and the value is a symbol-with-pos, unwrap to the bare symbol's `HashKey::Symbol(id)` before returning. This ensures hash table lookups treat symbol-with-pos identically to bare symbols during compilation.

## 6. Equal Comparison

In `equal_value()`:

- If `symbols_with_pos_enabled` is true: unwrap both operands if they are symbol-with-pos, then compare as bare symbols.
- If `symbols_with_pos_enabled` is false: compare symbol-with-pos structurally — both `sym` and `pos` fields must match via `BASE_EQ` (matching GNU's behavior in `internal_equal`).

## 7. Reader Integration

### value_reader.rs

The `Reader` struct already tracks `pos: usize` (byte offset). Add a `locate_syms: bool` field.

When `locate_syms` is true and the reader produces a symbol:
- Record `start_pos` before reading the symbol token
- After interning, wrap: `build_symbol_with_pos(sym, make_fixnum(start_pos))`
- Do NOT wrap `nil`: `if locate_syms && !result.is_nil()`
- DO wrap `t`, keywords, and all other symbols

When entering vector (`[...]`) or record reading: save and restore `locate_syms` (GNU does this to control whether vector contents get positions).

### reader.rs / builtin_read

Add a `locate_syms` parameter to the internal read path:

```rust
pub fn builtin_read_impl(ctx: &mut Context, args: Vec<Value>, locate_syms: bool) -> EvalResult
```

- `read` calls with `locate_syms = false`
- `read-positioning-symbols` calls with `locate_syms = true`

The buffer-reading path must pass `locate_syms` through to the `Reader` struct.

## 8. Lisp API Functions

All currently stubbed. Replace stubs with real implementations.

| Function | Args | Behavior |
|----------|------|----------|
| `symbol-with-pos-p` | `(obj)` | Return t if obj is a symbol-with-pos pseudo-vector. Ignores `symbols-with-pos-enabled` flag. |
| `bare-symbol-p` | `(obj)` | Return t if obj is a bare symbol (tag = TAG_SYMBOL). Ignores flag. |
| `bare-symbol` | `(sym)` | If sym is symbol-with-pos, return the bare symbol field. If sym is a bare symbol, return it. Otherwise signal `wrong-type-argument`. |
| `symbol-with-pos-pos` | `(swp)` | If swp is symbol-with-pos, return the pos field (fixnum). Otherwise signal `wrong-type-argument`. |
| `position-symbol` | `(sym pos)` | Create a new symbol-with-pos. `sym` must be a symbol (or symbol-with-pos, in which case extract bare symbol). `pos` must be a fixnum, or a symbol-with-pos (extract its pos). |
| `remove-pos-from-symbol` | `(obj)` | If obj is symbol-with-pos, return the bare symbol. Otherwise return obj unchanged. No error. |

## 9. Printer

In `print.rs`, add a case for `VecLikeType::SymbolWithPos`:

```rust
VecLikeType::SymbolWithPos => {
    let swp = value.as_symbol_with_pos().unwrap();
    if ctx.print_symbols_bare {
        // Print just the bare symbol name
        write_value_stateful(&swp.sym, ...);
    } else {
        write!(f, "#<symbol ");
        write_value_stateful(&swp.sym, ...);
        write!(f, " at {}", swp.pos.as_fixnum().unwrap());
        write!(f, ">");
    }
}
```

`print-symbols-bare` is checked, NOT `symbols-with-pos-enabled`. The byte-compiler sets `print-symbols-bare` to `t` in its `let` bindings.

## 10. Symbolp Predicate

`builtin_symbolp` must become context-dependent:

```rust
pub fn builtin_symbolp(ctx: &Context, arg: Value) -> EvalResult {
    let is_sym = arg.is_symbol()
        || (ctx.symbols_with_pos_enabled && arg.is_symbol_with_pos());
    Ok(Value::bool_val(is_sym))
}
```

All other predicates that check `is_symbol()` in hot paths where symbol-with-pos could appear must add the same conditional check. This includes the evaluator's symbol dispatch and the VM's symbol-related opcodes.

## 11. Symbol-Accepting Builtins

Many builtins accept a symbol argument (e.g., `symbol-name`, `symbol-value`, `set`, `put`, `get`, `boundp`, `fboundp`, `intern`). Each must unwrap symbol-with-pos to bare symbol when `symbols-with-pos-enabled` is true.

Add a helper:

```rust
fn unwrap_symbol(ctx: &Context, val: Value) -> Value {
    if ctx.symbols_with_pos_enabled && val.is_symbol_with_pos() {
        val.as_symbol_with_pos_sym().unwrap()
    } else {
        val
    }
}
```

Apply at the entry point of each symbol-accepting builtin.

## 12. Pdump

Symbol-with-pos objects MUST NOT appear in pdump snapshots. The pdump serializer should signal an error if it encounters `VecLikeType::SymbolWithPos`, matching GNU's behavior. These objects are ephemeral — created during reading, used during compilation, garbage-collected after.

## 13. Byte-Compiler Integration

No Rust code changes needed. The existing Elisp in `bytecomp.el` already:
- Binds `(let ((symbols-with-pos-enabled t) (print-symbols-bare t)) ...)`
- Calls `read-positioning-symbols` to read source forms
- Uses `byte-compile--first-symbol-with-pos` to search form trees for position info
- Calls `symbol-with-pos-pos` to extract byte offsets
- Converts byte offsets to line:column for warning messages

Once the Rust infrastructure works, the byte-compiler gets source-position warnings automatically.

## 14. GC Tracing

The `SymbolWithPosObj` contains two `Value` fields that must be traced. Add a `GcTrace` implementation (or equivalent) that traces both `sym` and `pos`. Since these are pseudo-vectors with LISPSIZE=2, the existing veclike GC tracing path should handle them if it iterates over Lisp-typed slots — verify this during implementation.

## 15. Files Changed

| File | Changes |
|------|---------|
| `tagged/header.rs` | Add `SymbolWithPos = 14` to `VecLikeType`, define `SymbolWithPosObj` struct |
| `tagged/value.rs` | Add `SymbolWithPos` to `ValueKind`, add accessors, update `kind()` dispatch |
| `tagged/gc.rs` | Add GC tracing for `SymbolWithPosObj` fields |
| `emacs_core/value.rs` | Update `eq_value`, `eql_value`, `equal_value`, `to_eq_key`, `to_eql_key` |
| `emacs_core/eval.rs` | Add `symbols_with_pos_enabled` field to Context, add `print_symbols_bare` field, add `unwrap_symbol` helper |
| `emacs_core/value_reader.rs` | Add `locate_syms` field to Reader, wrap symbols with position when true |
| `emacs_core/reader.rs` | Thread `locate_syms` through `builtin_read`, implement real `read-positioning-symbols` |
| `emacs_core/print.rs` | Add `SymbolWithPos` case, check `print_symbols_bare` |
| `emacs_core/builtins/types.rs` | Update `symbolp` to be context-dependent |
| `emacs_core/builtins/symbols.rs` | Implement `position-symbol`, `bare-symbol`, `symbol-with-pos-pos`, `symbol-with-pos-p`, `bare-symbol-p`, `remove-pos-from-symbol` |
| `emacs_core/builtins/mod.rs` | Update registration for changed builtins |
| `emacs_core/bytecode/vm.rs` | Update `Op::Eq` to use context-aware eq, unwrap symbols in symbol-related opcodes |
| `emacs_core/pdump/convert.rs` | Error on `SymbolWithPos` during serialization |
