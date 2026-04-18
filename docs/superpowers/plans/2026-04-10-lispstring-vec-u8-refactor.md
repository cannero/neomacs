# LispString Vec<u8> Refactor — Match GNU Emacs String Architecture

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace neomacs's `LispString { data: String, multibyte: bool }` with `LispString { data: Vec<u8>, size: usize, size_byte: i64 }` matching GNU Emacs's `struct Lisp_String` exactly, eliminating the entire sentinel encoding system and all Latin-1 workarounds.

**Architecture:** GNU Emacs stores strings as raw byte arrays in its internal encoding (UTF-8 superset: standard UTF-8 for Unicode 0x00-0x10FFFF, plus overlong C0/C1 sequences for raw bytes 0x80-0xFF). Neomacs currently uses Rust `String` (valid UTF-8 only) with Private Use Area sentinel codepoints to represent non-UTF-8 data. This refactor changes the backing store to `Vec<u8>` and implements GNU's `STRING_CHAR`/`CHAR_STRING` encoding/decoding directly. The 3,400+ `Value::string(&str)` call sites are migrated mechanically via a compatibility shim that encodes UTF-8 input into Emacs internal encoding (which is a no-op for standard Unicode text).

**Tech Stack:** Rust, neovm-core, neomacs-layout-engine, neomacs-tui-tests (regression safety net: 9/10 passing)

**Regression safety:** Run `cargo nextest run -p neomacs-tui-tests` after each phase. Currently 9/10 passing — no regressions allowed.

## Current Status (2026-04-18)

The checklist below is partially stale. Large parts of phases 1, 2, and 6 are already landed in `neovm-core`, and follow-up work should use the current code plus the local GNU Emacs tree as the reference implementation.

- `LispString` now stores `Vec<u8>` plus cached `size` / `size_byte`, mirroring GNU `struct Lisp_String` shape instead of the old Rust `String` + sentinel scheme.
- `emacs_char` is implemented and wired through string decoding/encoding, raw-byte conversion, and multibyte character counting.
- pdump string records already carry `(data, size, size_byte)` rather than reconstructing from UTF-8-only storage.
- `BufferText::from_lisp_string` and `BufferText::replace_lisp_string` now rebuild gap storage directly from raw Lisp string bytes, preserving unibyte 0x80..0xFF payloads without a Rust `String` round-trip.
- GNU-aligned fast paths are now in place for `string-make-multibyte` and `string-make-unibyte`: unchanged inputs return the original Lisp object instead of allocating a replacement string.
- `set-buffer-multibyte` now records the GNU-style special undo entry `(apply set-buffer-multibyte ...)` instead of clearing `buffer-undo-list`.
- `find-file-name-handler` now matches handler regexps against the original Lisp filename bytes, matching GNU `src/fileio.c` instead of decoding through a Rust runtime string first.
- `expand-file-name`, `file-name-directory`, `file-name-nondirectory`, `file-name-as-directory`, `directory-file-name`, and `unhandled-file-name-directory` now preserve GNU-style unibyte/multibyte results for raw-byte-sensitive file names instead of rebuilding through `Value::string`.
- `file-truename` now follows a GNU-shaped Lisp-level symlink chase in Neomacs, and `file-symlink-p` now hits the Unix filesystem through `LispString`/`PathBuf` byte-preserving helpers so raw unibyte file names and link targets survive intact.
- The `check_file_access` family in `src/fileio.c` is now mirrored more closely: `access-file`, `file-exists-p`, `file-readable-p`, `file-writable-p`, `file-accessible-directory-p`, `file-executable-p`, `file-directory-p`, `file-regular-p`, `file-modes`, and `set-file-modes` now reach the OS through byte-preserving `LispString`/`PathBuf` helpers instead of first decoding to a runtime `String`.
- `make-directory-internal`, `delete-file`, `delete-file-internal`, `delete-directory`, and `delete-directory-internal` now also resolve and hit the filesystem through the byte-preserving file-name path, including raw unibyte file names in error payloads where Neomacs now has the resolved Lisp filename available.
- `copy-file`, `rename-file`, `add-name-to-file`, and `make-symbolic-link` now follow GNU's `expand_cp_target` shape more closely: handler dispatch happens after GNU-style filename expansion, directory targets pick up the source basename, raw unibyte file names reach the OS through `LispString`/`PathBuf`, and `make-symbolic-link` preserves the link target bytes instead of resolving them through `default-directory`.
- `file-newer-than-file-p` now mirrors GNU's `expand_and_dir_to_file` flow before handler dispatch and stat, and `set-file-times` now expands to a Lisp filename before handler dispatch and uses the byte-preserving path boundary instead of rebuilding the path through a runtime `String`.
- `insert-file-contents` and `write-region` now expand the Lisp filename before handler dispatch, return/store resolved Lisp filename values without rebuilding them through `Value::string`, and reach the Unix filesystem through the byte-preserving `LispString`/`PathBuf` boundary for raw unibyte file names.
- `file-system-info` now also expands to a Lisp filename before handler dispatch and uses the byte-preserving path boundary, so raw unibyte directory names no longer have to round-trip through a runtime `String`.
- `find-file-noselect` now resolves and stores visited file names as Lisp strings, compares existing visited buffers against the resolved `LispString` path instead of a runtime string, and can reopen raw unibyte file names without losing the filename bytes.
- Backup and auto-save file-name plumbing now stays on `LispString` through name derivation and the Unix path boundary: backup names are computed from raw Lisp filename bytes, redirected backup directories are expanded relative to the visited file like GNU `files.el`, `make-auto-save-file-name` preserves raw visited/prefix bytes, and `do-auto-save` writes raw buffer bytes to raw auto-save file names without a runtime-string round-trip.
- `make-process`, `start-process`, `start-file-process`, and the underlying child-spawn path now keep command vectors and `process-environment` entries as Lisp strings up to the OS boundary: raw unibyte argv/env bytes survive process record storage, `getenv-internal` now matches GNU's raw-byte `process-environment` scan instead of decoding through UTF-8 first, and child processes now inherit Lisp `process-environment` overrides via byte-preserving `OsString` conversion at spawn time.
- `call-process`, `process-file`, and the `process-lines*` family now also keep program / arg / infile / file-destination values as Lisp strings until the spawn boundary, while `call-process-region` now writes raw string-input bytes to child stdin instead of forcing a runtime `String` conversion first. Raw unibyte argv and file-name bytes now survive the synchronous subprocess path through GNU-shaped `callproc.c` boundaries.

## GNU Alignment Notes (Local Source Audit)

Reference tree: `/home/exec/Projects/github.com/emacs-mirror/emacs/`

- `src/alloc.c`: GNU has both auto-detecting `make_string` and explicit constructors (`make_unibyte_string`, `make_multibyte_string`, `make_specified_string`). In Neomacs, `Value::string` should remain the valid-UTF-8 convenience entry point; raw-byte-sensitive paths should keep using explicit unibyte/multibyte constructors.
- `src/fns.c`: `string-make-multibyte` is identity for multibyte inputs and for unibyte ASCII inputs. Only unibyte non-ASCII bytes allocate a new multibyte string. `string-make-unibyte` is identity for unibyte inputs and truncates multibyte character codes to the low byte otherwise.
- `src/insdel.c`: buffer insertion converts text at the insertion boundary via `copy_text(from_multibyte, to_multibyte)` instead of globally normalizing Lisp string storage. Neomacs should keep conversion decisions at this boundary.
- `src/editfns.c`: `make_buffer_string_both` copies raw bytes out of the buffer gap into a Lisp string and labels the result based on buffer multibyteness. Buffer substring helpers should continue to be byte-faithful first.
- `src/buffer.c`: `set-buffer-multibyte` preserves the underlying bytes, then remaps char/byte positions, markers, overlays, and interval boundaries across the rewritten view of the same text. The remaining Neomacs audit should keep matching that shape instead of introducing higher-level string reinterpretation shortcuts.
- `src/fileio.c`: `find-file-name-handler` matches regexps against the incoming Lisp filename directly, while `file-name-directory`, `file-name-nondirectory`, `file-name-as-directory`, `directory-file-name`, and `expand-file-name` return strings with the same or reconciled multibyteness instead of normalizing through a UTF-8-only constructor.
- `lisp/files.el`: GNU `file-truename` is not a thin syscall wrapper; it iteratively resolves parent directories and symlink targets in Lisp, splicing relative link targets back onto the resolved directory without re-running `expand-file-name` on the target. The Neomacs `file-truename` path should keep moving toward that structure.
- `src/fileio.c`: `file-exists-p`, `file-readable-p`, `file-executable-p`, `file-writable-p`, `file-accessible-directory-p`, `file-directory-p`, `file-regular-p`, `file-modes`, and `set-file-modes` all expand to a Lisp filename, dispatch handlers, then call the OS through `ENCODE_FILE`. Neomacs should keep removing pre-OS runtime-string conversions from the rest of this surface.
- `src/fileio.c`: file-mutating primitives like `make-directory-internal`, `delete-file-internal`, and `delete-directory-internal` similarly expand to Lisp filenames and call the OS through `ENCODE_FILE`; the remaining Neomacs mutation builtins should converge on the same boundary handling.
- `src/fileio.c`: two-path file operations (`copy-file`, `rename-file`, `add-name-to-file`, `make-symbolic-link`) do not dispatch handlers on the raw argv pair. They first expand the source and destination Lisp filenames, apply `expand_cp_target` for directory targets, and only then dispatch handlers / cross the `ENCODE_FILE` boundary. `make-symbolic-link` also leaves the target text alone except for the interactive `~` and `/:` adjustments.
- `src/fileio.c`: `file-newer-than-file-p` uses `expand_and_dir_to_file` on both inputs before handler dispatch and stat, and `set-file-times` similarly expands to a Lisp filename before both handler lookup and `ENCODE_FILE`. The remaining Neomacs path audit should keep matching that "expand first, then dispatch / I/O" boundary.
- `src/fileio.c`: `insert-file-contents` and `write-region` both expand the file name before handler dispatch; `write-region` also passes the expanded output filename into the handler call while tracking the visit name separately. The remaining Neomacs audit should keep matching that filename/visit split instead of dispatching on raw UTF-8-only strings.
- `src/fileio.c`: `file-system-info` is another simple "expand first, dispatch on the Lisp filename, then ENCODE_FILE" surface. Neomacs should keep finishing the remaining file-name builtins in that same order.
- `lisp/files.el`: `find-file-noselect` normalizes the input file name early and keeps passing file names around as Lisp strings while it looks up existing visiting buffers and decides how to populate the new buffer. The remaining Neomacs caller paths should keep moving in that same direction rather than collapsing the visited file name back to a runtime `String`.
- `lisp/files.el`: `make-backup-file-name-1` expands redirected backup directories relative to the visited file's directory, and `make-auto-save-file-name` keeps deriving auto-save names from the current Lisp file name before any file-name handler or filesystem call. Neomacs should keep the backup/auto-save naming path in `LispString` up to the final OS boundary.
- `src/process.c` and `src/callproc.c`: GNU keeps process command vectors and `process-environment` as Lisp strings/bytes until the final spawn boundary, only converting via `ENCODE_FILE` or raw `SSDATA`/`SBYTES` access when building argv/env for the child. Neomacs should keep converging on that shape instead of parsing process commands and environment entries through Rust `String` first.
- `src/callproc.c`: synchronous subprocess entry points (`call-process`, `process-file`, `process-lines*`, and `call-process-region`) expand or validate Lisp file names early, but they still keep program names, argv entries, and input filenames as Lisp strings until the final OS boundary. String-input `call-process-region` also feeds the child the exact Lisp string bytes, not a normalized UTF-8 surrogate.

## Remaining Work

- Remove more `runtime_string_from_lisp_string` style adapters from core buffer/string paths so byte-preserving logic stays in `LispString`/`BufferText`.
- Keep auditing buffer conversion helpers against GNU `copy_text`, `make_buffer_string_both`, and `set-buffer-multibyte`, especially around markers, overlays, and text property remapping.
- Continue the file-name audit at the remaining filesystem boundaries beyond the predicate/access/mode/create-delete/two-path/mtime/insert-write/filesystem-info/find-file/backup/auto-save/process/callproc work, especially the remaining shell-command and reader/load helpers that still cross through runtime `String` values before OS calls.
- Treat the original phased checklist below as historical implementation guidance; update individual checkbox items only when the remaining slices are actually revisited.

---

## Phase Overview

| Phase | What | Risk | Effort |
|---|---|---|---|
| 1 | New LispString internals + compatibility shim | HIGH | Core change |
| 2 | Emacs character encoding module | LOW | New code |
| 3 | Migrate string builtins (aref, length, concat, etc.) | MEDIUM | 13 functions |
| 4 | Migrate reader and .elc loader | MEDIUM | 2 files |
| 5 | Remove sentinel encoding system | LOW | Delete code |
| 6 | Align buffer text and string encoding | MEDIUM | Bridge code |
| 7 | Final cleanup and verification | LOW | Polish |

---

## Key Design Decisions

### D1: Emacs Internal Encoding

GNU Emacs uses a UTF-8 superset. The encoding is:

| Character Range | Bytes | Encoding |
|---|---|---|
| U+0000-U+007F | 1 | `0xxxxxxx` (ASCII, same as UTF-8) |
| U+0080-U+07FF | 2 | `110xxxxx 10xxxxxx` (same as UTF-8) |
| U+0800-U+FFFF | 3 | `1110xxxx 10xxxxxx 10xxxxxx` (same as UTF-8) |
| U+10000-U+10FFFF | 4 | `11110xxx 10xxxxxx 10xxxxxx 10xxxxxx` (same as UTF-8) |
| U+110000-U+1FFFFF | 4 | `11110xxx 10xxxxxx 10xxxxxx 10xxxxxx` (extended) |
| U+200000-U+3FFF7F | 5 | `11111000 10xxxxxx 10xxxxxx 10xxxxxx 10xxxxxx` |
| Raw byte 0x80-0xFF | 2 | `1100000x 10xxxxxx` (**overlong C0/C1**, illegal in standard UTF-8) |

The critical difference from UTF-8: **raw bytes** 0x80-0xFF are encoded as overlong 2-byte sequences. `BYTE8_TO_CHAR(b) = b + 0x3FFF00`. In byte form: raw byte `0x80` → `[0xC0, 0x80]`, raw byte `0xFF` → `[0xC1, 0xBF]`.

### D2: Compatibility Shim Strategy

Most of the 3,400+ `Value::string(&str)` calls pass valid UTF-8 text (error messages, symbol names, paths). For standard Unicode, Emacs internal encoding == UTF-8. So `Value::string("hello")` can simply copy the bytes into `Vec<u8>` — zero overhead.

The shim: `Value::string(s: impl Into<String>)` converts the Rust `String` to bytes and stores them. `Value::as_str()` returns `Option<&str>` — succeeds if the bytes are valid UTF-8 (the common case), returns `None` for strings containing overlong C0/C1 sequences (rare). Call sites that need raw bytes use the new `as_bytes()` instead.

### D3: Phased Migration

Each phase produces a working, testable codebase. The compatibility shim means phases 1-2 can land without touching all 3,400+ call sites. Phases 3-5 gradually eliminate the shim and sentinel encoding.

---

## Task 1: New emacs_char module — Emacs character encoding

**Files:**
- Create: `neovm-core/src/emacs_core/emacs_char.rs`
- Modify: `neovm-core/src/emacs_core/mod.rs` (add `pub mod emacs_char;`)
- Test: `neovm-core/src/emacs_core/emacs_char_test.rs`

This module implements the Emacs internal character encoding, mirroring GNU's `character.h` / `character.c`. It is the foundation for everything else.

- [ ] **Step 1: Write failing tests for character encoding**

```rust
// neovm-core/src/emacs_core/emacs_char_test.rs

use super::emacs_char::*;

#[test]
fn ascii_roundtrip() {
    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];
    for c in 0..0x80u32 {
        let len = char_string(c, &mut buf);
        assert_eq!(len, 1);
        assert_eq!(buf[0], c as u8);
        let (decoded, dlen) = string_char(&buf[..len]);
        assert_eq!(decoded, c);
        assert_eq!(dlen, len);
    }
}

#[test]
fn two_byte_unicode_roundtrip() {
    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];
    // U+00E9 = é (2 bytes in UTF-8)
    let len = char_string(0xE9, &mut buf);
    assert_eq!(len, 2);
    assert_eq!(&buf[..2], &[0xC3, 0xA9]); // standard UTF-8
    let (decoded, dlen) = string_char(&buf[..len]);
    assert_eq!(decoded, 0xE9);
    assert_eq!(dlen, 2);
}

#[test]
fn three_byte_unicode_roundtrip() {
    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];
    // U+2018 = LEFT SINGLE QUOTATION MARK (3 bytes in UTF-8)
    let len = char_string(0x2018, &mut buf);
    assert_eq!(len, 3);
    assert_eq!(&buf[..3], &[0xE2, 0x80, 0x98]);
    let (decoded, dlen) = string_char(&buf[..len]);
    assert_eq!(decoded, 0x2018);
    assert_eq!(dlen, 3);
}

#[test]
fn four_byte_unicode_roundtrip() {
    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];
    // U+1F344 = 🍄 (4 bytes in UTF-8)
    let len = char_string(0x1F344, &mut buf);
    assert_eq!(len, 4);
    let (decoded, dlen) = string_char(&buf[..len]);
    assert_eq!(decoded, 0x1F344);
    assert_eq!(dlen, 4);
}

#[test]
fn raw_byte_roundtrip() {
    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];
    // Raw byte 0x80 → char code 0x3FFF80, encoded as [C0, 80]
    let char_code = byte8_to_char(0x80);
    assert_eq!(char_code, 0x3FFF80);
    assert!(char_byte8_p(char_code));
    let len = char_string(char_code, &mut buf);
    assert_eq!(len, 2);
    assert_eq!(&buf[..2], &[0xC0, 0x80]);
    let (decoded, dlen) = string_char(&buf[..len]);
    assert_eq!(decoded, char_code);
    assert_eq!(dlen, 2);
    assert_eq!(char_to_byte8(decoded), 0x80);
}

#[test]
fn raw_byte_ff_roundtrip() {
    let mut buf = [0u8; MAX_MULTIBYTE_LENGTH];
    let char_code = byte8_to_char(0xFF);
    assert_eq!(char_code, 0x3FFFFF);
    let len = char_string(char_code, &mut buf);
    assert_eq!(len, 2);
    assert_eq!(&buf[..2], &[0xC1, 0xBF]);
    let (decoded, dlen) = string_char(&buf[..len]);
    assert_eq!(decoded, char_code);
    assert_eq!(dlen, 2);
}

#[test]
fn multibyte_length() {
    // "hello" = 5 ASCII bytes
    assert_eq!(chars_in_multibyte(b"hello"), 5);
    // U+2018 = 3 bytes, 1 char
    assert_eq!(chars_in_multibyte(&[0xE2, 0x80, 0x98]), 1);
    // "a" + U+2018 + "b" = 1+3+1 = 5 bytes, 3 chars
    assert_eq!(chars_in_multibyte(&[0x61, 0xE2, 0x80, 0x98, 0x62]), 3);
    // raw byte 0x80 = [C0, 80] = 2 bytes, 1 char
    assert_eq!(chars_in_multibyte(&[0xC0, 0x80]), 1);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p neovm-core emacs_char`
Expected: FAIL — module doesn't exist yet

- [ ] **Step 3: Implement emacs_char module**

```rust
// neovm-core/src/emacs_core/emacs_char.rs
//! Emacs internal character encoding — mirrors GNU character.h/character.c.
//!
//! Characters are 22-bit integers (0 to MAX_CHAR = 0x3FFFFF).
//! Multibyte encoding is a UTF-8 superset: standard UTF-8 for Unicode
//! 0x00-0x10FFFF, plus overlong C0/C1 sequences for raw bytes 0x80-0xFF.

/// Maximum character code (22-bit).
pub const MAX_CHAR: u32 = 0x3FFFFF;

/// Maximum Unicode character code.
pub const MAX_UNICODE_CHAR: u32 = 0x10FFFF;

/// Boundary of the 5-byte character range.
pub const MAX_5_BYTE_CHAR: u32 = 0x3FFF7F;

/// Maximum bytes per multibyte character.
pub const MAX_MULTIBYTE_LENGTH: usize = 5;

/// Is this character a raw byte (0x80-0xFF encoded as 0x3FFF80-0x3FFFFF)?
#[inline]
pub const fn char_byte8_p(c: u32) -> bool {
    c > MAX_5_BYTE_CHAR
}

/// Convert a raw byte (0x80-0xFF) to its Emacs character code.
/// GNU: `BYTE8_TO_CHAR(byte) = (byte) + 0x3FFF00`
#[inline]
pub const fn byte8_to_char(byte: u8) -> u32 {
    (byte as u32) + 0x3FFF00
}

/// Convert a byte8 character code back to the raw byte.
/// GNU: `CHAR_TO_BYTE8(c) = CHAR_BYTE8_P(c) ? (c) - 0x3FFF00 : (c) & 0xFF`
#[inline]
pub const fn char_to_byte8(c: u32) -> u8 {
    if char_byte8_p(c) {
        (c - 0x3FFF00) as u8
    } else {
        (c & 0xFF) as u8
    }
}

/// Number of bytes needed to encode character `c` in multibyte form.
#[inline]
pub const fn char_bytes(c: u32) -> usize {
    if c < 0x80 { 1 }
    else if c < 0x800 { 2 }
    else if c < 0x10000 { 3 }
    else if c < 0x200000 { 4 }
    else if c < 0x400000 { 5 }
    else { 2 } // raw byte → overlong C0/C1
}

/// Encode character `c` into `buf`, return bytes written.
/// Mirrors GNU `CHAR_STRING(c, p)` from character.h.
pub fn char_string(c: u32, buf: &mut [u8]) -> usize {
    if c < 0x80 {
        buf[0] = c as u8;
        1
    } else if c < 0x800 {
        buf[0] = 0xC0 | ((c >> 6) as u8);
        buf[1] = 0x80 | ((c & 0x3F) as u8);
        2
    } else if c < 0x10000 {
        buf[0] = 0xE0 | ((c >> 12) as u8);
        buf[1] = 0x80 | (((c >> 6) & 0x3F) as u8);
        buf[2] = 0x80 | ((c & 0x3F) as u8);
        3
    } else if c < 0x200000 {
        buf[0] = 0xF0 | ((c >> 18) as u8);
        buf[1] = 0x80 | (((c >> 12) & 0x3F) as u8);
        buf[2] = 0x80 | (((c >> 6) & 0x3F) as u8);
        buf[3] = 0x80 | ((c & 0x3F) as u8);
        4
    } else if c <= MAX_5_BYTE_CHAR {
        buf[0] = 0xF8;
        buf[1] = 0x80 | (((c >> 18) & 0x0F) as u8);
        buf[2] = 0x80 | (((c >> 12) & 0x3F) as u8);
        buf[3] = 0x80 | (((c >> 6) & 0x3F) as u8);
        buf[4] = 0x80 | ((c & 0x3F) as u8);
        5
    } else {
        // Raw byte: encode as overlong C0/C1 sequence
        let byte = char_to_byte8(c);
        buf[0] = 0xC0 | ((byte >> 6) & 0x01);
        buf[1] = 0x80 | (byte & 0x3F);
        2
    }
}

/// Decode one character from multibyte `bytes`, return (charcode, bytes_consumed).
/// Mirrors GNU `STRING_CHAR_AND_LENGTH(p, len)` from character.h.
pub fn string_char(bytes: &[u8]) -> (u32, usize) {
    if bytes.is_empty() {
        return (0, 0);
    }
    let b0 = bytes[0];
    if b0 < 0x80 {
        return (b0 as u32, 1);
    }
    if b0 < 0xC0 {
        // Invalid leading byte — treat as raw byte
        return (byte8_to_char(b0), 1);
    }
    if b0 < 0xC2 {
        // Overlong C0/C1: raw byte encoding
        if bytes.len() >= 2 && (bytes[1] & 0xC0) == 0x80 {
            let raw = ((b0 & 0x01) << 6) | (bytes[1] & 0x3F);
            return (byte8_to_char(raw), 2);
        }
        return (byte8_to_char(b0), 1);
    }
    if b0 < 0xE0 {
        // 2-byte standard
        if bytes.len() >= 2 && (bytes[1] & 0xC0) == 0x80 {
            let c = ((b0 as u32 & 0x1F) << 6) | (bytes[1] as u32 & 0x3F);
            return (c, 2);
        }
        return (byte8_to_char(b0), 1);
    }
    if b0 < 0xF0 {
        // 3-byte
        if bytes.len() >= 3
            && (bytes[1] & 0xC0) == 0x80
            && (bytes[2] & 0xC0) == 0x80
        {
            let c = ((b0 as u32 & 0x0F) << 12)
                | ((bytes[1] as u32 & 0x3F) << 6)
                | (bytes[2] as u32 & 0x3F);
            return (c, 3);
        }
        return (byte8_to_char(b0), 1);
    }
    if b0 < 0xF8 {
        // 4-byte
        if bytes.len() >= 4
            && (bytes[1] & 0xC0) == 0x80
            && (bytes[2] & 0xC0) == 0x80
            && (bytes[3] & 0xC0) == 0x80
        {
            let c = ((b0 as u32 & 0x07) << 18)
                | ((bytes[1] as u32 & 0x3F) << 12)
                | ((bytes[2] as u32 & 0x3F) << 6)
                | (bytes[3] as u32 & 0x3F);
            return (c, 4);
        }
        return (byte8_to_char(b0), 1);
    }
    if b0 == 0xF8 {
        // 5-byte (Emacs extension for 0x200000-0x3FFF7F)
        if bytes.len() >= 5
            && (bytes[1] & 0xC0) == 0x80
            && (bytes[2] & 0xC0) == 0x80
            && (bytes[3] & 0xC0) == 0x80
            && (bytes[4] & 0xC0) == 0x80
        {
            let c = ((bytes[1] as u32 & 0x0F) << 18)
                | ((bytes[2] as u32 & 0x3F) << 12)
                | ((bytes[3] as u32 & 0x3F) << 6)
                | (bytes[4] as u32 & 0x3F);
            return (c, 5);
        }
        return (byte8_to_char(b0), 1);
    }
    // Invalid
    (byte8_to_char(b0), 1)
}

/// Count characters in a multibyte byte sequence.
pub fn chars_in_multibyte(bytes: &[u8]) -> usize {
    let mut count = 0;
    let mut i = 0;
    while i < bytes.len() {
        let (_, len) = string_char(&bytes[i..]);
        if len == 0 { break; }
        i += len;
        count += 1;
    }
    count
}

/// Convert a character index to a byte offset in multibyte data.
pub fn char_to_byte_pos(bytes: &[u8], char_idx: usize) -> usize {
    let mut i = 0;
    let mut chars = 0;
    while i < bytes.len() && chars < char_idx {
        let (_, len) = string_char(&bytes[i..]);
        if len == 0 { break; }
        i += len;
        chars += 1;
    }
    i
}

/// Convert a byte offset to a character index in multibyte data.
pub fn byte_to_char_pos(bytes: &[u8], byte_pos: usize) -> usize {
    let mut i = 0;
    let mut chars = 0;
    while i < bytes.len() && i < byte_pos {
        let (_, len) = string_char(&bytes[i..]);
        if len == 0 { break; }
        i += len;
        chars += 1;
    }
    chars
}

/// Try to convert Emacs internal encoding bytes to a UTF-8 string.
/// Returns None if the bytes contain overlong C0/C1 or 5-byte sequences.
pub fn try_as_utf8(bytes: &[u8]) -> Option<&str> {
    std::str::from_utf8(bytes).ok()
}

/// Convert Emacs internal encoding bytes to a Rust String.
/// Overlong C0/C1 (raw bytes) are replaced with U+FFFD.
pub fn to_utf8_lossy(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let (c, len) = string_char(&bytes[i..]);
        if len == 0 { break; }
        if char_byte8_p(c) {
            result.push('\u{FFFD}');
        } else if let Some(ch) = char::from_u32(c) {
            result.push(ch);
        } else {
            result.push('\u{FFFD}');
        }
        i += len;
    }
    result
}

/// Encode a Rust &str (valid UTF-8) into Emacs internal encoding bytes.
/// For standard Unicode, this is a no-op (just copies the bytes).
pub fn utf8_to_emacs(s: &str) -> Vec<u8> {
    // Standard UTF-8 IS valid Emacs internal encoding for Unicode chars.
    // Only raw bytes (which can't appear in valid UTF-8) would differ.
    s.as_bytes().to_vec()
}

#[cfg(test)]
#[path = "emacs_char_test.rs"]
mod tests;
```

- [ ] **Step 4: Register module**

Add to `neovm-core/src/emacs_core/mod.rs`:
```rust
pub mod emacs_char;
```

- [ ] **Step 5: Run tests**

Run: `cargo nextest run -p neovm-core emacs_char`
Expected: All 8 tests PASS

- [ ] **Step 6: Commit**

```bash
git add neovm-core/src/emacs_core/emacs_char.rs neovm-core/src/emacs_core/emacs_char_test.rs neovm-core/src/emacs_core/mod.rs
git commit -m "emacs_char: add Emacs internal character encoding module

Implements GNU character.h/character.c encoding in Rust:
- char_string / string_char: encode/decode multibyte
- byte8_to_char / char_to_byte8: raw byte conversion
- chars_in_multibyte / char_to_byte_pos / byte_to_char_pos
- utf8_to_emacs / try_as_utf8 / to_utf8_lossy: boundary conversion

This is the foundation for migrating LispString from Rust String
to Vec<u8> with Emacs internal encoding."
```

---

## Task 2: New LispString with Vec<u8> + compatibility shim

**Files:**
- Modify: `neovm-core/src/heap_types.rs` (LispString struct)
- Modify: `neovm-core/src/emacs_core/value.rs` (Value constructors/accessors)
- Modify: `neovm-core/src/tagged/header.rs` (StringObj)
- Test: existing tests must still pass

This is the core structural change. The key insight: for standard Unicode text (which is 99%+ of strings), Emacs internal encoding == UTF-8. So `Value::string("hello")` just copies the UTF-8 bytes into `Vec<u8>`. The compatibility shim `as_str()` checks if bytes are valid UTF-8 and returns `Option<&str>`.

- [ ] **Step 1: Change LispString struct**

In `heap_types.rs`, change:
```rust
pub struct LispString {
    data: Vec<u8>,          // Emacs internal encoding (UTF-8 superset)
    size: usize,            // character count
    size_byte: i64,         // byte count, or -1 for unibyte
}

impl LispString {
    /// Create a multibyte string from Emacs internal encoding bytes.
    pub fn from_emacs_bytes(data: Vec<u8>) -> Self {
        let size = emacs_char::chars_in_multibyte(&data);
        let size_byte = data.len() as i64;
        Self { data, size, size_byte }
    }

    /// Create a unibyte string (each byte is one character).
    pub fn from_unibyte(data: Vec<u8>) -> Self {
        let size = data.len();
        Self { data, size, size_byte: -1 }
    }

    /// Create from a Rust &str (valid UTF-8). Result is multibyte.
    /// Standard UTF-8 is valid Emacs internal encoding.
    pub fn from_utf8(s: &str) -> Self {
        let data = s.as_bytes().to_vec();
        let size = s.chars().count();
        let size_byte = data.len() as i64;
        Self { data, size, size_byte }
    }

    /// Backward-compat: create from String + multibyte flag.
    /// Used during migration; eventually remove.
    pub fn new(text: String, multibyte: bool) -> Self {
        if multibyte {
            Self::from_utf8(&text)
        } else {
            Self::from_unibyte(text.into_bytes())
        }
    }

    /// Raw bytes (Emacs internal encoding).
    pub fn as_bytes(&self) -> &[u8] { &self.data }

    /// Try to view as UTF-8 &str. Returns None if data contains
    /// overlong C0/C1 sequences (raw bytes) or 5-byte sequences.
    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.data).ok()
    }

    /// Character count. GNU: SCHARS(s).
    pub fn schars(&self) -> usize { self.size }

    /// Byte count. GNU: SBYTES(s).
    pub fn sbytes(&self) -> usize {
        if self.size_byte < 0 { self.size } else { self.data.len() }
    }

    /// Is this a multibyte string?
    pub fn is_multibyte(&self) -> bool { self.size_byte >= 0 }

    /// Backward compat: the old `multibyte` field.
    pub fn multibyte(&self) -> bool { self.is_multibyte() }
}
```

- [ ] **Step 2: Update Value string constructors**

In `value.rs`, update `Value::string()` etc. to use the new API. `Value::string(s)` calls `LispString::from_utf8(&s)`. `Value::as_str()` delegates to `LispString::as_str()`.

- [ ] **Step 3: Fix compilation errors mechanically**

The main breakage will be:
- `string.as_str()` calls that assumed infallible `&str` return — now returns `Option<&str>`
- `LispString.data` field access — now `Vec<u8>` not `String`
- Code that reads `string.multibyte` field directly — use `string.is_multibyte()`

Fix each file's compilation errors. Most `as_str()` calls can use `.as_str().unwrap_or("")` or `.as_str().unwrap()` (safe because 99%+ of strings are pure UTF-8).

- [ ] **Step 4: Run full test suite**

Run: `cargo nextest run -p neovm-core` (the full 5400+ tests)
Run: `cargo nextest run -p neomacs-tui-tests` (TUI comparison: expect 9/10)
Expected: No regressions

- [ ] **Step 5: Commit**

```bash
git commit -m "string: migrate LispString from String to Vec<u8>

Change LispString backing store from Rust String (UTF-8 only) to
Vec<u8> (Emacs internal encoding). Add size/size_byte fields matching
GNU's struct Lisp_String.

For standard Unicode text, Emacs internal encoding == UTF-8, so most
code paths are unchanged. as_str() now returns Option<&str> since the
data may contain overlong C0/C1 raw byte sequences."
```

---

## Task 3: Migrate string builtins to byte-level access

**Files:**
- Modify: `neovm-core/src/emacs_core/builtins/collections.rs` (aref, aset)
- Modify: `neovm-core/src/emacs_core/builtins/strings.rs` (concat, substring)
- Modify: `neovm-core/src/emacs_core/builtins/cons_list.rs` (length)
- Modify: `neovm-core/src/emacs_core/bytecode/vm.rs` (length_value, aref)
- Modify: `neovm-core/src/encoding.rs` (string-bytes)

Replace all `storage_char_len`, `decode_storage_char_codes`, `storage_substring`, `storage_char_to_byte` calls with direct `emacs_char` functions operating on `as_bytes()`.

- [ ] **Step 1: Migrate `aref` on strings**

In `collections.rs`, change:
```rust
// OLD:
let s = args[0].as_str().unwrap().to_owned();
let codes = decode_storage_char_codes(&s);
codes.get(idx)...

// NEW:
let bytes = args[0].as_lisp_string().unwrap().as_bytes();
let byte_pos = emacs_char::char_to_byte_pos(bytes, idx);
let (charcode, _) = emacs_char::string_char(&bytes[byte_pos..]);
Ok(Value::fixnum(charcode as i64))
```

- [ ] **Step 2: Migrate `length` on strings**

In `vm.rs` `length_value()` and `cons_list.rs`:
```rust
// OLD: val.as_str().unwrap().chars().count()
// NEW: val.as_lisp_string().unwrap().schars()
```

- [ ] **Step 3: Migrate `string-bytes`**

In `encoding.rs`:
```rust
// OLD: storage_byte_len(s)
// NEW: val.as_lisp_string().unwrap().sbytes()
```

- [ ] **Step 4: Migrate `concat`**

In `strings.rs`, concatenation creates a new `Vec<u8>` by appending bytes from each argument. If any argument is multibyte, result is multibyte. Unibyte bytes 0x80-0xFF get promoted to overlong C0/C1 encoding.

- [ ] **Step 5: Migrate `substring`**

Use `char_to_byte_pos` for start/end, then `&bytes[start..end]`.

- [ ] **Step 6: Run tests, commit**

---

## Task 4: Migrate reader and .elc loader

**Files:**
- Modify: `neovm-core/src/emacs_core/value_reader.rs` (read_string)
- Modify: `neovm-core/src/emacs_core/load.rs` (.elc content handling)

- [ ] **Step 1: Remove .elc Latin-1 workaround**

In `load.rs`, change the .elc loader to store raw bytes directly:
```rust
// OLD: let content: String = raw_bytes.iter().map(|&b| b as char).collect();
// NEW: pass raw_bytes directly to a byte-aware reader
```

This is the biggest simplification — the entire `.elc` Latin-1 encoding/`maybe_recombine_latin1_as_utf8` system goes away.

- [ ] **Step 2: Update read_string for byte-aware reading**

The reader needs to work on `&[u8]` instead of `&str`. For `.elc` files, bytes go directly into the string `Vec<u8>`. For `.el` files (valid UTF-8), the behavior is unchanged.

- [ ] **Step 3: Run tests, commit**

---

## Task 5: Remove sentinel encoding system

**Files:**
- Delete or gut: `neovm-core/src/emacs_core/string_escape.rs` (653 lines)
- Modify: all files that import from `string_escape`

- [ ] **Step 1: Replace all `storage_char_len` calls with `schars()`**
- [ ] **Step 2: Replace all `decode_storage_char_codes` calls with `emacs_char::string_char` loops**
- [ ] **Step 3: Replace all `storage_char_to_byte` calls with `emacs_char::char_to_byte_pos`**
- [ ] **Step 4: Remove `encode_nonunicode_char_for_storage`, `bytes_to_unibyte_storage_string`, PUA sentinel constants**
- [ ] **Step 5: Delete or minimize `string_escape.rs`**
- [ ] **Step 6: Run tests, commit**

---

## Task 6: Align buffer text and pdump

**Files:**
- Modify: `neovm-core/src/buffer/buffer.rs` (insert, buffer_string)
- Modify: `neovm-core/src/emacs_core/pdump/convert.rs` (DumpHeapObject::Str)
- Modify: `neovm-core/src/emacs_core/pdump/types.rs` (DumpHeapObject::Str)

- [ ] **Step 1: Update pdump to serialize Vec<u8> + size + size_byte**

Change `DumpHeapObject::Str { text: String, multibyte: bool }` to `DumpHeapObject::Str { data: Vec<u8>, size: usize, size_byte: i64 }`.

- [ ] **Step 2: Update buffer insert/extract to use Emacs encoding**

Buffer's `GapBuffer` already stores `Vec<u8>`. Ensure the encoding matches: multibyte buffers use Emacs internal encoding, unibyte buffers use raw bytes.

- [ ] **Step 3: Run tests, commit, fresh-build to regenerate pdump**

---

## Task 7: Final cleanup and verification

- [ ] **Step 1: Remove `maybe_recombine_latin1_as_utf8` from value_reader.rs**
- [ ] **Step 2: Run full test suite**: `cargo nextest run -p neovm-core`, `cargo nextest run -p neomacs-tui-tests`
- [ ] **Step 3: Test doom dashboard**: `./target/debug/neomacs -nw` — verify nerd font icons render correctly
- [ ] **Step 4: Run TUI comparison**: all 10 tests should pass (the split_window_right line-wrap issue is unrelated to string encoding)
- [ ] **Step 5: Final commit and push**

---

## Risk Mitigation

1. **Phase 2 is the riskiest** — changing LispString struct breaks compilation everywhere. Use `cargo check` iteratively and fix errors file by file. Most fixes are mechanical (`as_str()` → `as_str().unwrap()`).

2. **Pdump compatibility** — changing the pdump format requires `cargo xtask fresh-build` to regenerate. Old pdump files won't load. This is acceptable since pdump format has a version check.

3. **The `.elc` reader refactor (Task 4)** may need the reader to support both `&str` and `&[u8]` input during the transition. A trait-based approach or an enum `ReaderInput { Str(&str), Bytes(&[u8]) }` can bridge this.

4. **Buffer encoding alignment (Task 6)** is the least understood area. The buffer's GapBuffer stores raw bytes but the byte↔char conversion currently uses Rust's UTF-8 decoding. This needs to switch to `emacs_char::string_char` for multibyte buffers.
