# Value-Native Reader — Phase 1: Create reader.rs

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create `reader.rs` — a Value-native Lisp reader that produces `Value` directly on the tagged heap, matching GNU Emacs's `read0` in `lread.c`. This is a mechanical translation of the existing `parser.rs` (which produces `Expr`) to produce `Value` instead.

**Architecture:** The reader mirrors `parser.rs` method-for-method but outputs `Value` instead of `Expr`. It takes `&mut Context` (the evaluator) for heap allocation (`Value::cons`, `Value::string`, `Value::make_vector`, etc.) and GC rooting (`push_temp_root`). The reader is pure addition — `parser.rs` stays untouched.

**Tech Stack:** Rust. `neovm-core` crate. Uses existing `Value`, `Context`, `intern()`, `tagged::gc` APIs.

**Testing:** `cargo nextest run -p neovm-core reader` (redirect to file). Tests in separate `reader_test.rs`.

**Reference:** The existing `parser.rs` at `neovm-core/src/emacs_core/parser.rs` (1233 lines) is the template. Each `parse_*` method becomes a `read_*` method.

---

## File Structure

| File | Action | Purpose |
|------|--------|---------|
| `neovm-core/src/emacs_core/reader.rs` | Create | Value-native reader: `read_one`, `read_all`, all syntax support |
| `neovm-core/src/emacs_core/reader_test.rs` | Create | Tests for the reader |
| `neovm-core/src/emacs_core/mod.rs` | Modify | Add `pub mod reader;` |

---

### Task 1: Create reader.rs with core infrastructure and atoms

Create the reader module with the parsing infrastructure (whitespace, comments, bump/current/peek, error handling) and support for atoms (integers, floats, symbols, keywords, t, nil).

**Files:**
- Create: `neovm-core/src/emacs_core/reader.rs`
- Create: `neovm-core/src/emacs_core/reader_test.rs`
- Modify: `neovm-core/src/emacs_core/mod.rs`

- [ ] **Step 1: Write failing tests**

Create `neovm-core/src/emacs_core/reader_test.rs`:

```rust
use crate::emacs_core::eval::Context;
use crate::emacs_core::reader;
use crate::emacs_core::value::Value;
use crate::test_utils::runtime_startup_context;

fn read_one(input: &str) -> Value {
    let mut ctx = runtime_startup_context();
    reader::read_one(&mut ctx, input, 0)
        .expect("read_one failed")
        .expect("empty input")
        .0
}

fn read_all(input: &str) -> Vec<Value> {
    let mut ctx = runtime_startup_context();
    reader::read_all(&mut ctx, input).expect("read_all failed")
}

#[test]
fn read_integer() {
    let val = read_one("42");
    assert_eq!(val.as_fixnum(), Some(42));
}

#[test]
fn read_negative_integer() {
    let val = read_one("-7");
    assert_eq!(val.as_fixnum(), Some(-7));
}

#[test]
fn read_float() {
    let val = read_one("3.14");
    assert!((val.as_float().unwrap() - 3.14).abs() < 1e-10);
}

#[test]
fn read_symbol() {
    let val = read_one("foo");
    assert_eq!(val.as_symbol_name(), Some("foo"));
}

#[test]
fn read_nil() {
    let val = read_one("nil");
    assert!(val.is_nil());
}

#[test]
fn read_t() {
    let val = read_one("t");
    assert!(val.is_t());
}

#[test]
fn read_keyword() {
    let val = read_one(":hello");
    assert!(val.is_keyword());
}

#[test]
fn read_empty_returns_none() {
    let mut ctx = runtime_startup_context();
    let result = reader::read_one(&mut ctx, "", 0).unwrap();
    assert!(result.is_none());
}

#[test]
fn read_whitespace_only_returns_none() {
    let mut ctx = runtime_startup_context();
    let result = reader::read_one(&mut ctx, "  \n  ", 0).unwrap();
    assert!(result.is_none());
}

#[test]
fn read_comment_only_returns_none() {
    let mut ctx = runtime_startup_context();
    let result = reader::read_one(&mut ctx, "; comment\n", 0).unwrap();
    assert!(result.is_none());
}

#[test]
fn read_all_multiple_atoms() {
    let vals = read_all("1 2 3");
    assert_eq!(vals.len(), 3);
    assert_eq!(vals[0].as_fixnum(), Some(1));
    assert_eq!(vals[1].as_fixnum(), Some(2));
    assert_eq!(vals[2].as_fixnum(), Some(3));
}

#[test]
fn read_one_returns_position() {
    let mut ctx = runtime_startup_context();
    let (val, pos) = reader::read_one(&mut ctx, "42 99", 0)
        .unwrap()
        .unwrap();
    assert_eq!(val.as_fixnum(), Some(42));
    assert!(pos <= 3); // after "42" plus maybe whitespace
    let (val2, _) = reader::read_one(&mut ctx, "42 99", pos)
        .unwrap()
        .unwrap();
    assert_eq!(val2.as_fixnum(), Some(99));
}
```

- [ ] **Step 2: Add module declaration**

In `neovm-core/src/emacs_core/mod.rs`, add:
```rust
pub mod reader;
```

- [ ] **Step 3: Create reader.rs with infrastructure + atom reading**

Create `neovm-core/src/emacs_core/reader.rs`. The implementer should:

1. Read `parser.rs` completely to understand the structure
2. Copy the infrastructure methods (`bump`, `current`, `peek_at`, `skip_ws_and_comments`, `expect`, `error`, `is_dot_separator`, `parse_decimal_usize`) — these are the same since they don't produce Expr
3. Translate `parse_atom` → `read_atom`: instead of returning `Expr::Int(n)`, return `Value::fixnum(n)`. Instead of `Expr::Symbol(id)`, return `Value::from_sym_id(id)`. Instead of `Expr::Float(f)`, return `Value::make_float(f)`. Handle `t` → `Value::T`, `nil` → `Value::NIL`, keywords → `Value::keyword_id(id)`.
4. Implement `read_form` (the main dispatch, equivalent to `parse_expr`) but initially only handle atoms — return errors for lists, vectors, strings, etc.
5. Implement `read_one` and `read_all` as public API wrappers

The Reader struct:
```rust
pub struct Reader<'a> {
    input: &'a str,
    pos: usize,
    read_labels: HashMap<usize, Value>,
}
```

Note: the reader does NOT take `&mut Context` in its struct — it takes it as a parameter to `read_form` and other methods that allocate. This avoids lifetime conflicts.

```rust
pub fn read_one(
    eval: &mut Context,
    input: &str,
    start: usize,
) -> Result<Option<(Value, usize)>, ReadError> {
    let mut reader = Reader::new(&input[start..]);
    if !reader.skip_ws_and_comments() {
        return Ok(None);
    }
    let val = reader.read_form(eval)?;
    Ok(Some((val, start + reader.pos)))
}

pub fn read_all(
    eval: &mut Context,
    input: &str,
) -> Result<Vec<Value>, ReadError> {
    let mut reader = Reader::new(input);
    let mut forms = Vec::new();
    while reader.skip_ws_and_comments() {
        forms.push(reader.read_form(eval)?);
    }
    Ok(forms)
}
```

The `ReadError` type can reuse or mirror `ParseError`.

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p neovm-core reader 2>&1 > /tmp/test-output.log; tail -20 /tmp/test-output.log`

Expected: All atom tests pass.

- [ ] **Step 5: Commit**

```bash
git add neovm-core/src/emacs_core/reader.rs neovm-core/src/emacs_core/reader_test.rs neovm-core/src/emacs_core/mod.rs
git commit -m "feat: add Value-native reader with atom support"
```

---

### Task 2: Add list, dotted pair, and vector reading

Extend the reader to handle `(a b c)`, `(a . b)`, and `[a b c]`.

**Files:**
- Modify: `neovm-core/src/emacs_core/reader.rs`
- Modify: `neovm-core/src/emacs_core/reader_test.rs`

- [ ] **Step 1: Write failing tests**

Append to `reader_test.rs`:

```rust
#[test]
fn read_simple_list() {
    let val = read_one("(a b c)");
    assert!(val.is_cons());
    assert_eq!(val.cons_car().as_symbol_name(), Some("a"));
    assert_eq!(val.cons_cdr().cons_car().as_symbol_name(), Some("b"));
    assert_eq!(val.cons_cdr().cons_cdr().cons_car().as_symbol_name(), Some("c"));
    assert!(val.cons_cdr().cons_cdr().cons_cdr().is_nil());
}

#[test]
fn read_empty_list() {
    let val = read_one("()");
    assert!(val.is_nil());
}

#[test]
fn read_nested_list() {
    let val = read_one("(a (b c) d)");
    assert!(val.is_cons());
    let inner = val.cons_cdr().cons_car();
    assert!(inner.is_cons());
    assert_eq!(inner.cons_car().as_symbol_name(), Some("b"));
}

#[test]
fn read_dotted_pair() {
    let val = read_one("(a . b)");
    assert!(val.is_cons());
    assert_eq!(val.cons_car().as_symbol_name(), Some("a"));
    assert_eq!(val.cons_cdr().as_symbol_name(), Some("b"));
}

#[test]
fn read_dotted_list() {
    let val = read_one("(a b . c)");
    assert!(val.is_cons());
    assert_eq!(val.cons_car().as_symbol_name(), Some("a"));
    assert_eq!(val.cons_cdr().cons_car().as_symbol_name(), Some("b"));
    assert_eq!(val.cons_cdr().cons_cdr().as_symbol_name(), Some("c"));
}

#[test]
fn read_vector() {
    let val = read_one("[1 2 3]");
    assert!(val.is_vector());
    let data = val.as_vector_data().unwrap();
    assert_eq!(data.len(), 3);
    assert_eq!(data[0].as_fixnum(), Some(1));
    assert_eq!(data[1].as_fixnum(), Some(2));
    assert_eq!(data[2].as_fixnum(), Some(3));
}

#[test]
fn read_empty_vector() {
    let val = read_one("[]");
    assert!(val.is_vector());
    assert_eq!(val.as_vector_data().unwrap().len(), 0);
}
```

- [ ] **Step 2: Implement list and vector reading**

In `reader.rs`, add `read_list_or_dotted` and `read_vector` methods. Translate from `parser.rs` `parse_list_or_dotted` and `parse_vector`:

For lists: build cons chain from items. `(a b c)` → `Value::cons(a, Value::cons(b, Value::cons(c, Value::NIL)))`. Use `Value::list(items)` for proper lists, manual cons chain for dotted lists.

For vectors: collect items into `Vec<Value>`, then `Value::make_vector(items)`.

GC rooting: while building a list, root each item via `eval.push_temp_root(item)`. After building the cons chain, the chain itself keeps elements alive, so pop the roots.

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p neovm-core reader 2>&1 > /tmp/test-output.log; tail -20 /tmp/test-output.log`

- [ ] **Step 4: Commit**

```bash
git add neovm-core/src/emacs_core/reader.rs neovm-core/src/emacs_core/reader_test.rs
git commit -m "feat(reader): add list, dotted pair, and vector reading"
```

---

### Task 3: Add string and character literal reading

Translate `parse_string` and `parse_char_literal` from parser.rs.

**Files:**
- Modify: `neovm-core/src/emacs_core/reader.rs`
- Modify: `neovm-core/src/emacs_core/reader_test.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn read_string() {
    let val = read_one("\"hello\"");
    assert_eq!(val.as_str(), Some("hello"));
}

#[test]
fn read_string_with_escapes() {
    let val = read_one("\"hello\\nworld\"");
    assert_eq!(val.as_str(), Some("hello\nworld"));
}

#[test]
fn read_empty_string() {
    let val = read_one("\"\"");
    assert_eq!(val.as_str(), Some(""));
}

#[test]
fn read_char_literal() {
    let val = read_one("?a");
    assert_eq!(val.as_fixnum(), Some(97));
}

#[test]
fn read_char_literal_space() {
    let val = read_one("?\\s");
    assert_eq!(val.as_fixnum(), Some(32));
}

#[test]
fn read_char_literal_newline() {
    let val = read_one("?\\n");
    assert_eq!(val.as_fixnum(), Some(10));
}
```

- [ ] **Step 2: Implement string and char reading**

Copy `parse_string`, `parse_string_char_value`, `parse_char_literal`, `parse_char_value` from parser.rs. The string parsing logic is identical — it produces a `String` which becomes `Value::string(s)`. Character literals produce an integer which becomes `Value::fixnum(n as i64)`.

Also copy the helper functions: `bytes_to_unibyte_storage_string`, `encode_nonunicode_char_for_storage` imports from `string_escape`.

- [ ] **Step 3: Run tests + Commit**

```bash
git commit -m "feat(reader): add string and character literal reading"
```

---

### Task 4: Add quote, backquote, unquote, function syntax

Translate `'x`, `` `x ``, `,x`, `,@x`, `#'x` from parser.rs.

**Files:**
- Modify: `neovm-core/src/emacs_core/reader.rs`
- Modify: `neovm-core/src/emacs_core/reader_test.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn read_quoted_symbol() {
    let val = read_one("'foo");
    // (quote foo)
    assert_eq!(val.cons_car().as_symbol_name(), Some("quote"));
    assert_eq!(val.cons_cdr().cons_car().as_symbol_name(), Some("foo"));
}

#[test]
fn read_backquote() {
    let val = read_one("`foo");
    assert_eq!(val.cons_car().as_symbol_name(), Some("\\`"));
}

#[test]
fn read_unquote() {
    let val = read_one(",foo");
    assert_eq!(val.cons_car().as_symbol_name(), Some("\\,"));
}

#[test]
fn read_splice() {
    let val = read_one(",@foo");
    assert_eq!(val.cons_car().as_symbol_name(), Some("\\,@"));
}

#[test]
fn read_function_quote() {
    let val = read_one("#'foo");
    assert_eq!(val.cons_car().as_symbol_name(), Some("function"));
    assert_eq!(val.cons_cdr().cons_car().as_symbol_name(), Some("foo"));
}
```

Note: check how parser.rs interns backquote/comma symbols — they might use different names than shown. The implementer must look at the actual `intern()` calls in parser.rs lines 117, 124, 127.

- [ ] **Step 2: Implement quote syntax**

In the `read_form` dispatch, add cases for `'`, `` ` ``, `,`, `#`. For each: read the next form, wrap in a 2-element list: `Value::list(vec![Value::symbol(intern("quote")), inner])`.

- [ ] **Step 3: Run tests + Commit**

```bash
git commit -m "feat(reader): add quote, backquote, unquote, function syntax"
```

---

### Task 5: Add hash syntax (#' #: #[ #s #N= #N# etc.)

This is the most complex part. Translate `parse_hash_syntax` from parser.rs.

**Files:**
- Modify: `neovm-core/src/emacs_core/reader.rs`
- Modify: `neovm-core/src/emacs_core/reader_test.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn read_uninterned_symbol() {
    let val = read_one("#:foo");
    assert!(val.is_symbol());
    // Uninterned symbols have unique IDs
}

#[test]
fn read_byte_code_vector() {
    let val = read_one("#[0 nil nil 1]");
    assert!(val.is_vector()); // or byte-code, depending on implementation
}

#[test]
fn read_hash_table_literal() {
    let val = read_one("#s(hash-table data (a 1 b 2))");
    // Should produce a hash table Value
}

#[test]
fn read_radix_number() {
    let val = read_one("#xff");
    assert_eq!(val.as_fixnum(), Some(255));
}

#[test]
fn read_binary_number() {
    let val = read_one("#b1010");
    assert_eq!(val.as_fixnum(), Some(10));
}

#[test]
fn read_octal_number() {
    let val = read_one("#o17");
    assert_eq!(val.as_fixnum(), Some(15));
}

#[test]
fn read_shared_structure() {
    let val = read_one("#1=(a . #1#)");
    // Circular list — first element is 'a', cdr points back to itself
    assert_eq!(val.cons_car().as_symbol_name(), Some("a"));
    // val.cons_cdr() should be eq to val (circular)
}
```

- [ ] **Step 2: Implement hash syntax**

Translate `parse_hash_syntax`, `parse_radix_number`, `parse_hash_table_literal`, `parse_hash_skip_bytes`, `parse_bool_vector_size` from parser.rs. The hash syntax dispatch (`#x`, `#o`, `#b`, `#:`, `#[`, `#s(`, `#N=`, `#N#`, `#'`) is a large switch.

Key differences from parser.rs:
- `#[a b c d]` in parser returns `Expr::Vector`. In the reader, this should return a byte-code vector `Value`. Check how `reify_byte_code_literals` works and whether the reader should produce a raw vector or a ByteCode value.
- `#s(hash-table ...)` needs to construct a real hash table Value on the heap.
- `#N=EXPR` / `#N#` shared structure: the reader uses `read_labels: HashMap<usize, Value>` to store and recall shared values.

- [ ] **Step 3: Run tests + Commit**

```bash
git commit -m "feat(reader): add hash syntax (#: #[ #s #x #o #b #N= #N#)"
```

---

### Task 6: Verify reader handles all parser.rs syntax

**Files:** None modified — verification only.

- [ ] **Step 1: Cross-reference parser.rs test cases**

Read `neovm-core/src/emacs_core/reader_test.rs` (the existing parser tests) and verify the reader handles every syntax the parser handles. Add any missing test cases.

Search for parser tests:
```bash
grep -rn "fn test_\|fn parse_" neovm-core/src/emacs_core/reader_test.rs | head -30
```

Wait — the parser tests might be in a different file. Check:
```bash
find neovm-core/src -name "*parser*test*" -o -name "*reader*test*"
```

- [ ] **Step 2: Run full test suite**

```bash
cargo nextest run -p neovm-core reader 2>&1 > /tmp/test-output.log
tail -20 /tmp/test-output.log
```

- [ ] **Step 3: Verify compilation**

```bash
cargo check --workspace
```

- [ ] **Step 4: Commit if any fixes needed**

```bash
git add -A
git diff --cached --quiet || git commit -m "test(reader): add comprehensive syntax coverage"
```

---

### Task 7: Add read_from_string builtin integration

Wire the reader into the evaluator so `(read-from-string "42")` uses the new reader.

**Files:**
- Modify: `neovm-core/src/emacs_core/reader.rs` (if needed)
- Modify: `neovm-core/src/emacs_core/reader_test.rs`

- [ ] **Step 1: Write test**

```rust
#[test]
fn read_from_string_via_reader() {
    let mut ctx = runtime_startup_context();
    let forms = reader::read_all(&mut ctx, "(+ 1 2)").unwrap();
    assert_eq!(forms.len(), 1);
    let result = ctx.eval_sub(forms[0]).unwrap();
    assert_eq!(result.as_fixnum(), Some(3));
}

#[test]
fn read_defun_and_eval() {
    let mut ctx = runtime_startup_context();
    let forms = reader::read_all(
        &mut ctx,
        "(defun my-add (a b) (+ a b))",
    ).unwrap();
    ctx.eval_sub(forms[0]).unwrap();
    let call = reader::read_all(&mut ctx, "(my-add 3 4)").unwrap();
    let result = ctx.eval_sub(call[0]).unwrap();
    assert_eq!(result.as_fixnum(), Some(7));
}
```

- [ ] **Step 2: Run tests + Commit**

```bash
git commit -m "test(reader): verify read → eval integration"
```
