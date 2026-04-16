# Symbol-with-Pos Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement GNU Emacs's `symbol-with-pos` type so the byte-compiler can produce source-location warnings.

**Architecture:** New pseudo-vector type (`VecLikeType::SymbolWithPos = 14`) with two `Value` fields (`sym`, `pos`). A global flag `symbols-with-pos-enabled` makes `symbolp`, `eq`, and hash operations transparently unwrap the type. The reader wraps symbols with byte offsets when `locate_syms = true`.

**Tech Stack:** Rust (neovm-core crate), GNU Emacs Lisp compatibility layer

**Spec:** `docs/superpowers/specs/2026-04-16-symbol-with-pos-design.md`

---

### Task 1: Add SymbolWithPos pseudo-vector type and struct

**Files:**
- Modify: `neovm-core/src/tagged/header.rs:132-152` (VecLikeType enum)
- Modify: `neovm-core/src/tagged/value.rs:684-699` (type predicates)

- [ ] **Step 1: Add SymbolWithPos variant to VecLikeType enum**

In `neovm-core/src/tagged/header.rs`, add to the `VecLikeType` enum after `Bignum = 13`:

```rust
    /// Symbol with source position (like GNU's PVEC_SYMBOL_WITH_POS).
    /// Wraps a bare symbol + byte offset for byte-compiler diagnostics.
    SymbolWithPos = 14,
```

- [ ] **Step 2: Define SymbolWithPosObj struct**

In `neovm-core/src/tagged/header.rs`, after the existing pseudo-vector structs (after `BignumObj`), add:

```rust
/// A symbol annotated with its source byte offset.
/// Mirrors GNU `struct Lisp_Symbol_With_Pos` (`lisp.h:958`).
/// Both fields are `TaggedValue` (GC-traced), matching GNU's LISPSIZE=2.
#[repr(C)]
pub struct SymbolWithPosObj {
    pub header: VecLikeHeader,
    /// The bare symbol. Must always be a plain symbol (TAG_SYMBOL).
    pub sym: TaggedValue,
    /// Source byte offset. Must always be a fixnum.
    pub pos: TaggedValue,
}
```

- [ ] **Step 3: Add is_symbol_with_pos predicate**

In `neovm-core/src/tagged/value.rs`, after the `is_hash_table` method (around line 699), add:

```rust
    /// True if this value is a symbol-with-pos pseudo-vector.
    #[inline]
    pub fn is_symbol_with_pos(self) -> bool {
        self.veclike_type() == Some(VecLikeType::SymbolWithPos)
    }
```

- [ ] **Step 4: Add accessor methods**

In `neovm-core/src/tagged/value.rs`, near the other accessor methods, add:

```rust
    /// If this is a symbol-with-pos, return a reference to the object.
    pub fn as_symbol_with_pos(&self) -> Option<&SymbolWithPosObj> {
        if self.is_symbol_with_pos() {
            Some(unsafe { &*(self.as_veclike_ptr()? as *const SymbolWithPosObj) })
        } else {
            None
        }
    }

    /// If this is a symbol-with-pos, return the bare symbol Value.
    pub fn as_symbol_with_pos_sym(&self) -> Option<TaggedValue> {
        self.as_symbol_with_pos().map(|swp| swp.sym)
    }

    /// If this is a symbol-with-pos, return the position as i64.
    pub fn as_symbol_with_pos_pos(&self) -> Option<i64> {
        self.as_symbol_with_pos().and_then(|swp| swp.pos.as_fixnum())
    }
```

- [ ] **Step 5: Build and verify**

Run: `cargo build 2>&1 | tail -5`
Expected: Compiles with no errors (warnings OK).

- [ ] **Step 6: Commit**

```bash
git add neovm-core/src/tagged/header.rs neovm-core/src/tagged/value.rs
git commit -m "Add SymbolWithPos pseudo-vector type and struct"
```

---

### Task 2: Add GC allocation and tracing for SymbolWithPos

**Files:**
- Modify: `neovm-core/src/tagged/gc.rs:900-938` (alloc functions)
- Modify: `neovm-core/src/tagged/gc.rs:1174-1268` (trace_veclike)

- [ ] **Step 1: Add alloc_symbol_with_pos function**

In `neovm-core/src/tagged/gc.rs`, after the `alloc_bignum` function, add:

```rust
    /// Allocate a symbol-with-pos object.
    /// `sym` must be a bare symbol, `pos` must be a fixnum.
    pub fn alloc_symbol_with_pos(&mut self, sym: TaggedValue, pos: TaggedValue) -> TaggedValue {
        let obj = Box::new(SymbolWithPosObj {
            header: VecLikeHeader::new(VecLikeType::SymbolWithPos),
            sym,
            pos,
        });
        let ptr = Box::into_raw(obj);
        self.link_veclike(ptr as *mut VecLikeHeader);
        self.allocated_count += 1;
        self.note_allocation_bytes(size_of::<SymbolWithPosObj>());
        unsafe { TaggedValue::from_veclike_ptr(ptr as *const VecLikeHeader) }
    }
```

Add `use crate::tagged::header::SymbolWithPosObj;` to the imports if not already covered by a wildcard.

- [ ] **Step 2: Add GC tracing for SymbolWithPos**

In `neovm-core/src/tagged/gc.rs`, in the `trace_veclike` function, add a new match arm before the catch-all arm (the one with `Buffer | Window | Frame | Timer | Marker | Subr | Bignum`):

```rust
            VecLikeType::SymbolWithPos => {
                let obj = ptr as *const SymbolWithPosObj;
                let sym = unsafe { (*obj).sym };
                let pos = unsafe { (*obj).pos };
                if sym.is_heap_object() {
                    self.gray_queue.push(sym);
                }
                if pos.is_heap_object() {
                    self.gray_queue.push(pos);
                }
            }
```

Note: `sym` is a bare symbol (tag=000, not heap) and `pos` is a fixnum (not heap), so these pushes will rarely fire. But tracing them matches GNU's LISPSIZE=2 and is correct if someone stores unexpected values.

- [ ] **Step 3: Build and verify**

Run: `cargo build 2>&1 | tail -5`
Expected: Compiles cleanly.

- [ ] **Step 4: Commit**

```bash
git add neovm-core/src/tagged/gc.rs
git commit -m "Add GC allocation and tracing for SymbolWithPos"
```

---

### Task 3: Add global flags and unwrap_symbol helper

**Files:**
- Modify: `neovm-core/src/emacs_core/eval.rs:1236-1300` (Context struct)

- [ ] **Step 1: Add fields to Context struct**

In `neovm-core/src/emacs_core/eval.rs`, in the `Context` struct definition, add these fields (after the `noninteractive: bool` field around line 1264):

```rust
    /// When true, `symbolp`/`eq`/hash operations transparently unwrap
    /// symbol-with-pos objects. Bound to `t` by the byte-compiler.
    pub(crate) symbols_with_pos_enabled: bool,
    /// When true, the printer outputs bare symbol names for symbol-with-pos.
    pub(crate) print_symbols_bare: bool,
```

- [ ] **Step 2: Initialize fields in Context constructor**

Find the `Context::new` or equivalent constructor. Add initialization:

```rust
    symbols_with_pos_enabled: false,
    print_symbols_bare: false,
```

- [ ] **Step 3: Register as Lisp DEFVAR_BOOL variables**

Find where other bool defvars are registered (search for `noninteractive_symbol` registration or `DEFVAR_BOOL` pattern). Register both variables so Lisp code can read/write them:

```rust
    // In the defvar registration section:
    // symbols-with-pos-enabled
    let swpe_sym = intern("symbols-with-pos-enabled");
    ctx.obarray.set_symbol_value(swpe_sym, Value::NIL);
    // print-symbols-bare
    let psb_sym = intern("print-symbols-bare");
    ctx.obarray.set_symbol_value(psb_sym, Value::NIL);
```

The Context fields must stay in sync with the obarray values. Follow the same pattern used for `noninteractive` — check how it syncs between `ctx.noninteractive` and the symbol value. If it uses a watcher or reads from obarray each time, do the same.

- [ ] **Step 4: Add unwrap_symbol helper**

In `neovm-core/src/emacs_core/eval.rs`, add a public helper method on Context:

```rust
impl Context {
    /// If `symbols-with-pos-enabled` and `val` is a symbol-with-pos,
    /// return the bare symbol. Otherwise return `val` unchanged.
    #[inline]
    pub fn unwrap_symbol(&self, val: Value) -> Value {
        if self.symbols_with_pos_enabled && val.is_symbol_with_pos() {
            val.as_symbol_with_pos_sym().unwrap()
        } else {
            val
        }
    }
}
```

- [ ] **Step 5: Build and verify**

Run: `cargo build 2>&1 | tail -5`
Expected: Compiles cleanly.

- [ ] **Step 6: Commit**

```bash
git add neovm-core/src/emacs_core/eval.rs
git commit -m "Add symbols-with-pos-enabled flag and unwrap_symbol helper"
```

---

### Task 4: Update eq, eql, equal, and hash key computation

**Files:**
- Modify: `neovm-core/src/emacs_core/value.rs:1539-1631` (comparison functions)
- Modify: `neovm-core/src/emacs_core/value.rs:1416-1447` (hash key functions)

- [ ] **Step 1: Update eq_value to accept symbols_with_pos_enabled flag**

In `neovm-core/src/emacs_core/value.rs`, replace the `eq_value` function (lines 1539-1541):

```rust
pub fn eq_value(left: &Value, right: &Value) -> bool {
    eq_value_swp(left, right, false)
}

/// EQ comparison with optional symbol-with-pos transparency.
pub fn eq_value_swp(left: &Value, right: &Value, symbols_with_pos_enabled: bool) -> bool {
    if left.bits() == right.bits() {
        return true;
    }
    if !symbols_with_pos_enabled {
        return false;
    }
    // Slow path: unwrap symbol-with-pos
    let l = if left.is_symbol_with_pos() {
        left.as_symbol_with_pos_sym().unwrap()
    } else {
        *left
    };
    let r = if right.is_symbol_with_pos() {
        right.as_symbol_with_pos_sym().unwrap()
    } else {
        *right
    };
    l.bits() == r.bits()
}
```

Keep the no-arg `eq_value` for call sites that don't have access to the flag (backward compat).

- [ ] **Step 2: Update eql_value**

Replace the `eql_value` function (lines 1544-1552):

```rust
pub fn eql_value(left: &Value, right: &Value) -> bool {
    eql_value_swp(left, right, false)
}

pub fn eql_value_swp(left: &Value, right: &Value, symbols_with_pos_enabled: bool) -> bool {
    if eq_value_swp(left, right, symbols_with_pos_enabled) {
        return true;
    }
    match (left.kind(), right.kind()) {
        (ValueKind::Float, ValueKind::Float) => left.xfloat().to_bits() == right.xfloat().to_bits(),
        _ => false,
    }
}
```

- [ ] **Step 3: Update equal_value_inner for symbol-with-pos**

In `equal_value_inner` (line 1560), add a new match arm after the `(ValueKind::Symbol(a), ValueKind::Symbol(b))` arm (line 1578). Add a `symbols_with_pos_enabled: bool` parameter threaded through `equal_value` → `equal_value_inner`:

Add this arm in the match:

```rust
        // symbol-with-pos: when flag enabled, unwrap and compare as symbols.
        // When disabled, compare both sym AND pos fields.
        (ValueKind::Veclike(VecLikeType::SymbolWithPos), ValueKind::Veclike(VecLikeType::SymbolWithPos)) => {
            if symbols_with_pos_enabled {
                let l = left.as_symbol_with_pos_sym().unwrap();
                let r = right.as_symbol_with_pos_sym().unwrap();
                l.bits() == r.bits()
            } else {
                let l = left.as_symbol_with_pos().unwrap();
                let r = right.as_symbol_with_pos().unwrap();
                l.sym.bits() == r.sym.bits() && l.pos.bits() == r.pos.bits()
            }
        }
```

Also add cross-type matching when flag is enabled (symbol-with-pos == bare symbol):

```rust
        // symbol-with-pos vs bare symbol when flag enabled
        (ValueKind::Symbol(_), ValueKind::Veclike(VecLikeType::SymbolWithPos))
        | (ValueKind::Veclike(VecLikeType::SymbolWithPos), ValueKind::Symbol(_))
            if symbols_with_pos_enabled =>
        {
            let l = if left.is_symbol_with_pos() { left.as_symbol_with_pos_sym().unwrap() } else { *left };
            let r = if right.is_symbol_with_pos() { right.as_symbol_with_pos_sym().unwrap() } else { *right };
            l.bits() == r.bits()
        }
```

- [ ] **Step 4: Update to_eq_key for symbol-with-pos**

In `to_eq_key` (line 1416), the `Veclike(_)` arm catches symbol-with-pos and hashes by pointer. Add a `to_eq_key_swp` variant:

```rust
    pub fn to_eq_key_swp(&self, symbols_with_pos_enabled: bool) -> HashKey {
        if symbols_with_pos_enabled && self.is_symbol_with_pos() {
            let sym = self.as_symbol_with_pos_sym().unwrap();
            return sym.to_eq_key();
        }
        self.to_eq_key()
    }
```

Similarly for `to_eql_key`:

```rust
    pub fn to_eql_key_swp(&self, symbols_with_pos_enabled: bool) -> HashKey {
        if symbols_with_pos_enabled && self.is_symbol_with_pos() {
            let sym = self.as_symbol_with_pos_sym().unwrap();
            return sym.to_eql_key();
        }
        self.to_eql_key()
    }
```

- [ ] **Step 5: Build and verify**

Run: `cargo build 2>&1 | tail -5`
Expected: Compiles cleanly (existing callers still use the no-flag versions).

- [ ] **Step 6: Commit**

```bash
git add neovm-core/src/emacs_core/value.rs
git commit -m "Add symbol-with-pos transparent unwrapping in eq/eql/equal/hash"
```

---

### Task 5: Update symbolp and builtin_eq to be context-aware

**Files:**
- Modify: `neovm-core/src/emacs_core/builtins/types.rs:28-30` (symbolp)
- Modify: `neovm-core/src/emacs_core/bytecode/vm.rs:1285-1291` (Op::Eq)

- [ ] **Step 1: Update builtin_symbolp_1**

In `neovm-core/src/emacs_core/builtins/types.rs`, replace `builtin_symbolp_1` (line 28):

```rust
pub(crate) fn builtin_symbolp_1(eval: &mut super::eval::Context, arg: Value) -> EvalResult {
    let is_sym = arg.is_symbol()
        || (eval.symbols_with_pos_enabled && arg.is_symbol_with_pos());
    Ok(Value::bool_val(is_sym))
}
```

Also update the variadic `builtin_symbolp` if it exists separately.

- [ ] **Step 2: Update Op::Eq in bytecode VM**

In `neovm-core/src/emacs_core/bytecode/vm.rs`, replace the `Op::Eq` handler (line 1285):

```rust
                Op::Eq => {
                    let len = stk!().len();
                    let b = stk!()[len - 1];
                    let a = stk!()[len - 2];
                    let result = crate::emacs_core::value::eq_value_swp(
                        &a, &b, vm.ctx().symbols_with_pos_enabled,
                    );
                    stk!()[len - 2] = if result { Value::T } else { Value::NIL };
                    stk!().pop();
                }
```

Note: `vm.ctx()` must return a reference to the Context. Check how other opcodes access the context and use the same pattern.

- [ ] **Step 3: Build and verify**

Run: `cargo build 2>&1 | tail -5`
Expected: Compiles cleanly.

- [ ] **Step 4: Commit**

```bash
git add neovm-core/src/emacs_core/builtins/types.rs neovm-core/src/emacs_core/bytecode/vm.rs
git commit -m "Make symbolp and bytecode eq context-aware for symbol-with-pos"
```

---

### Task 6: Implement Lisp API functions

**Files:**
- Modify: `neovm-core/src/emacs_core/builtins/symbols.rs:2140-2229` (replace stubs)
- Modify: `neovm-core/src/emacs_core/builtins/mod.rs` (update registrations)

- [ ] **Step 1: Implement bare-symbol-p**

Find the `builtin_bare_symbol_p` stub in `symbols.rs` (search for "bare-symbol-p"). Replace with:

```rust
pub(crate) fn builtin_bare_symbol_p(args: Vec<Value>) -> EvalResult {
    expect_args("bare-symbol-p", &args, 1)?;
    Ok(Value::bool_val(args[0].is_symbol()))
}
```

- [ ] **Step 2: Implement symbol-with-pos-p**

Find the `builtin_symbol_with_pos_p` stub. Replace with:

```rust
pub(crate) fn builtin_symbol_with_pos_p(args: Vec<Value>) -> EvalResult {
    expect_args("symbol-with-pos-p", &args, 1)?;
    Ok(Value::bool_val(args[0].is_symbol_with_pos()))
}
```

- [ ] **Step 3: Implement bare-symbol**

Find the `builtin_bare_symbol` stub. Replace with:

```rust
pub(crate) fn builtin_bare_symbol(args: Vec<Value>) -> EvalResult {
    expect_args("bare-symbol", &args, 1)?;
    let arg = args[0];
    if arg.is_symbol() {
        Ok(arg)
    } else if arg.is_symbol_with_pos() {
        Ok(arg.as_symbol_with_pos_sym().unwrap())
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), arg],
        ))
    }
}
```

- [ ] **Step 4: Implement symbol-with-pos-pos**

Find the `builtin_symbol_with_pos_pos` stub. Replace with:

```rust
pub(crate) fn builtin_symbol_with_pos_pos(args: Vec<Value>) -> EvalResult {
    expect_args("symbol-with-pos-pos", &args, 1)?;
    if let Some(pos) = args[0].as_symbol_with_pos_pos() {
        Ok(Value::fixnum(pos))
    } else {
        Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbol-with-pos-p"), args[0]],
        ))
    }
}
```

- [ ] **Step 5: Implement position-symbol**

Replace `builtin_position_symbol` (line 2140):

```rust
pub(crate) fn builtin_position_symbol(
    ctx: &mut super::eval::Context,
    args: Vec<Value>,
) -> EvalResult {
    expect_args("position-symbol", &args, 2)?;
    // Extract bare symbol from arg0
    let sym = if args[0].is_symbol() {
        args[0]
    } else if args[0].is_symbol_with_pos() {
        args[0].as_symbol_with_pos_sym().unwrap()
    } else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("symbolp"), args[0]],
        ));
    };
    // Extract position from arg1
    let pos = if let Some(n) = args[1].as_fixnum() {
        Value::fixnum(n)
    } else if let Some(p) = args[1].as_symbol_with_pos_pos() {
        Value::fixnum(p)
    } else {
        return Err(signal(
            "wrong-type-argument",
            vec![Value::symbol("fixnump"), args[1]],
        ));
    };
    Ok(ctx.tagged_heap.alloc_symbol_with_pos(sym.0, pos.0).into())
}
```

Note: The signature now takes `ctx` because it allocates. Update the registration in `mod.rs` accordingly (from `|_ctx, args|` to `|ctx, args|`).

- [ ] **Step 6: Implement remove-pos-from-symbol**

Replace `builtin_remove_pos_from_symbol` (line 2226):

```rust
pub(crate) fn builtin_remove_pos_from_symbol(args: Vec<Value>) -> EvalResult {
    expect_args("remove-pos-from-symbol", &args, 1)?;
    if args[0].is_symbol_with_pos() {
        Ok(args[0].as_symbol_with_pos_sym().unwrap())
    } else {
        Ok(args[0])
    }
}
```

- [ ] **Step 7: Update registrations in mod.rs**

Update each builtin registration in `neovm-core/src/emacs_core/builtins/mod.rs` to use the new implementations. Specifically, `position-symbol` now needs `ctx` access for allocation. Change its registration from:

```rust
|_ctx, args| builtin_position_symbol(args),
```

to:

```rust
|ctx, args| builtin_position_symbol(ctx, args),
```

- [ ] **Step 8: Build and verify**

Run: `cargo build 2>&1 | tail -5`
Expected: Compiles cleanly.

- [ ] **Step 9: Commit**

```bash
git add neovm-core/src/emacs_core/builtins/symbols.rs neovm-core/src/emacs_core/builtins/mod.rs
git commit -m "Implement symbol-with-pos Lisp API functions"
```

---

### Task 7: Update printer for SymbolWithPos

**Files:**
- Modify: `neovm-core/src/emacs_core/print.rs` (VecLikeType dispatch)

- [ ] **Step 1: Add SymbolWithPos case to printer**

In `neovm-core/src/emacs_core/print.rs`, find the VecLikeType match dispatch in the main print function. Add a new arm for `VecLikeType::SymbolWithPos`:

```rust
            VecLikeType::SymbolWithPos => {
                let swp = value.as_symbol_with_pos().unwrap();
                if state.options.print_symbols_bare {
                    write_value_stateful(&swp.sym.into(), state, out);
                } else {
                    out.push_str("#<symbol ");
                    write_value_stateful(&swp.sym.into(), state, out);
                    if let Some(pos) = swp.pos.as_fixnum() {
                        out.push_str(&format!(" at {}", pos));
                    }
                    out.push('>');
                }
            }
```

Check the exact types and function signatures used in the printer — `PrintOptions` may need a `print_symbols_bare` field. If it uses a different mechanism to access evaluator state, follow that pattern. The `state.options` structure and `write_value_stateful` signature must match existing code.

- [ ] **Step 2: Add print_symbols_bare to PrintOptions if needed**

If the printer uses `PrintOptions`, add:

```rust
pub print_symbols_bare: bool,
```

Default to `false`. The caller (byte-compiler through Elisp `let`-binding of `print-symbols-bare`) controls this.

- [ ] **Step 3: Build and verify**

Run: `cargo build 2>&1 | tail -5`
Expected: Compiles cleanly.

- [ ] **Step 4: Commit**

```bash
git add neovm-core/src/emacs_core/print.rs
git commit -m "Add SymbolWithPos printer support with print-symbols-bare"
```

---

### Task 8: Add pdump error for SymbolWithPos

**Files:**
- Modify: `neovm-core/src/emacs_core/pdump/convert.rs:158-195` (dump_value)
- Modify: `neovm-core/src/emacs_core/pdump/convert.rs:810-860` (dump_heap_object)

- [ ] **Step 1: Add error arm in dump_value**

In `neovm-core/src/emacs_core/pdump/convert.rs`, in the `dump_value` match on `ValueKind`, add before the catch-all:

```rust
            ValueKind::Veclike(VecLikeType::SymbolWithPos) => {
                panic!("symbol-with-pos objects cannot be dumped to pdump (ephemeral type)")
            }
```

- [ ] **Step 2: Add error arm in dump_heap_object**

In the `dump_heap_object` match, add:

```rust
        ValueKind::Veclike(VecLikeType::SymbolWithPos) => {
            panic!("symbol-with-pos objects cannot be dumped to pdump (ephemeral type)")
        }
```

- [ ] **Step 3: Build and verify**

Run: `cargo build 2>&1 | tail -5`
Expected: Compiles cleanly.

- [ ] **Step 4: Commit**

```bash
git add neovm-core/src/emacs_core/pdump/convert.rs
git commit -m "Error on SymbolWithPos in pdump serialization"
```

---

### Task 9: Implement read-positioning-symbols with reader position tracking

**Files:**
- Modify: `neovm-core/src/emacs_core/value_reader.rs` (Reader struct, symbol wrapping)
- Modify: `neovm-core/src/emacs_core/reader.rs` (builtin_read, read-positioning-symbols)
- Modify: `neovm-core/src/emacs_core/builtins/mod.rs` (registration)

- [ ] **Step 1: Add locate_syms field to Reader**

In `neovm-core/src/emacs_core/value_reader.rs`, add a field to the `Reader` struct:

```rust
    /// When true, wrap interned symbols in symbol-with-pos objects.
    locate_syms: bool,
```

Initialize to `false` in the `Reader::new` constructor.

- [ ] **Step 2: Add constructor variant for locate_syms**

Add a method:

```rust
impl Reader {
    pub fn with_locate_syms(mut self, locate_syms: bool) -> Self {
        self.locate_syms = locate_syms;
        self
    }
}
```

Or add a parameter to `read_one_with_source_multibyte`:

```rust
pub fn read_one_with_locate_syms(
    input: &str,
    source_multibyte: bool,
    start: usize,
    locate_syms: bool,
) -> Result<Option<(Value, usize)>, ReadError> {
    let mut reader = Reader::new(input, source_multibyte);
    reader.pos = start;
    reader.locate_syms = locate_syms;
    if !reader.skip_ws_and_comments() {
        return Ok(None);
    }
    let value = reader.read_form()?;
    Ok(Some((value, reader.pos)))
}
```

- [ ] **Step 3: Wrap symbols with position in read_form**

Find where the reader produces symbol values (in `read_form` or a sub-method like `read_symbol`). At each point where a symbol is interned and returned, add wrapping logic:

```rust
// After interning a symbol to get `sym_value`:
let result = if self.locate_syms && !sym_value.is_nil() {
    // Wrap in symbol-with-pos with the byte offset where this symbol started
    let pos = Value::fixnum(symbol_start_pos as i64);
    crate::tagged::gc::with_thread_local_heap(|heap| {
        heap.alloc_symbol_with_pos(sym_value.0, pos.0).into()
    })
} else {
    sym_value
};
```

The exact integration depends on the reader's structure. The key requirements:
1. Record `symbol_start_pos = self.pos` BEFORE reading the symbol characters
2. After interning, wrap if `self.locate_syms && !result.is_nil()`
3. `t`, keywords, and all other non-nil symbols get wrapped
4. `nil` does NOT get wrapped
5. When entering vector (`[...]`) or record reading, save and restore `locate_syms` (GNU does this)

- [ ] **Step 4: Update builtin_read to accept locate_syms parameter**

In `neovm-core/src/emacs_core/reader.rs`, add an internal implementation function:

```rust
pub fn builtin_read_impl(
    ctx: &mut crate::emacs_core::eval::Context,
    args: Vec<Value>,
    locate_syms: bool,
) -> EvalResult {
    // Same body as current builtin_read, but pass locate_syms through
    // to the Reader when reading from buffers/strings
    ...
}
```

Then update `builtin_read` to call `builtin_read_impl(ctx, args, false)`.

- [ ] **Step 5: Update read-positioning-symbols registration**

In `neovm-core/src/emacs_core/builtins/mod.rs`, change the `read-positioning-symbols` registration from:

```rust
        |ctx, args| super::reader::builtin_read(ctx, args),
```

to:

```rust
        |ctx, args| super::reader::builtin_read_impl(ctx, args, true),
```

- [ ] **Step 6: Build and verify**

Run: `cargo build 2>&1 | tail -5`
Expected: Compiles cleanly.

- [ ] **Step 7: Test basic functionality**

```bash
cargo xtask fresh-build > /tmp/fb.log 2>&1 && \
NEOMACS_LOG_FILE=/dev/null timeout 15 ./target/debug/neomacs --batch --eval '
(progn
  (let ((symbols-with-pos-enabled t))
    (let ((form (read-positioning-symbols "(defun foo (x) x)")))
      (princ (format "form=%s\n" form))
      (princ (format "car type=%s\n" (type-of (car form))))
      (princ (format "symbol-with-pos-p=%s\n" (symbol-with-pos-p (car form))))
      (when (symbol-with-pos-p (car form))
        (princ (format "sym=%s pos=%s\n" (bare-symbol (car form)) (symbol-with-pos-pos (car form))))))))' 2>&1
```

Expected: `symbol-with-pos-p=t` and position info printed.

- [ ] **Step 8: Commit**

```bash
git add neovm-core/src/emacs_core/value_reader.rs neovm-core/src/emacs_core/reader.rs neovm-core/src/emacs_core/builtins/mod.rs
git commit -m "Implement read-positioning-symbols with reader position tracking"
```

---

### Task 10: Apply unwrap_symbol to symbol-accepting builtins

**Files:**
- Modify: `neovm-core/src/emacs_core/builtins/symbols.rs` (symbol-name, symbol-value, etc.)
- Modify: `neovm-core/src/emacs_core/eval.rs` (evaluator symbol dispatch)

Many builtins accept a symbol argument and must unwrap symbol-with-pos when `symbols-with-pos-enabled` is true. The `unwrap_symbol` helper from Task 3 handles this. Apply it to:

- `symbol-name`, `symbol-value`, `symbol-function`, `symbol-plist`
- `set`, `fset`, `put`, `get`
- `boundp`, `fboundp`, `default-boundp`
- `intern`, `intern-soft`
- Evaluator symbol dispatch in `eval_sub` / `apply_symbol_callable_untraced`

- [ ] **Step 1: Apply unwrap_symbol at each builtin entry point**

At the top of each symbol-accepting builtin, wrap the symbol argument:

```rust
let sym = ctx.unwrap_symbol(args[0]);
```

Then use `sym` instead of `args[0]` for the rest of the function.

The exact list of builtins to modify will depend on which ones the byte-compiler actually invokes with symbol-with-pos values. Start with the core set listed above. If byte-compilation triggers `wrong-type-argument` errors for a symbol builtin, add `unwrap_symbol` there.

- [ ] **Step 2: Build and verify**

Run: `cargo build 2>&1 | tail -5`
Expected: Compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add neovm-core/src/emacs_core/builtins/symbols.rs neovm-core/src/emacs_core/eval.rs
git commit -m "Apply unwrap_symbol to symbol-accepting builtins"
```

---

### Task 11: Test byte-compile-file with source position warnings

**Files:** No code changes — integration test.

- [ ] **Step 1: Fresh build**

```bash
cargo xtask fresh-build > /tmp/fb.log 2>&1 && echo "OK"
```

- [ ] **Step 2: Test byte-compile-file completes**

```bash
cat > /tmp/test-warn.el << 'EOF'
;;; test-warn.el --- Test warnings  -*- lexical-binding: t -*-
(defun my-test (x)
  (+ x undefined-var))
(provide 'test-warn)
;;; test-warn.el ends here
EOF
NEOMACS_LOG_FILE=/dev/null timeout 20 ./target/debug/neomacs --batch --eval '
(progn
  (byte-compile-file "/tmp/test-warn.el")
  (princ "COMPILE OK\n"))' 2>&1
```

Expected: "COMPILE OK" printed. The compiler may produce a warning about `undefined-var` — if it includes a line number, symbol-with-pos is working end-to-end.

- [ ] **Step 3: Test Doom startup**

```bash
NEOMACS_LOG_FILE=/tmp/doom_swp.log timeout 40 ./target/debug/neomacs -nw &
PID=$!; sleep 30; kill $PID 2>/dev/null; wait $PID 2>/dev/null
grep "Doom loaded" /tmp/doom_swp.log
grep "missed.tag\|Optimizer error" /tmp/doom_swp.log
```

Expected: "Doom loaded 166 packages" present, no "missed tags" errors.

- [ ] **Step 4: Commit if any fixups were needed**

```bash
git add -A && git commit -m "Fix integration issues found during symbol-with-pos testing"
```

Only commit if changes were made. If everything passes, skip this step.
