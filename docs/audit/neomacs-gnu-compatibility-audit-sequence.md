# Neomacs vs GNU Emacs 100% Semantic Compatibility Audit Sequence

**Date**: 2026-03-28

## Overview

The audit sequence follows the dependency graph bottom-up. You can't audit Display
until Buffer semantics are verified, since Display depends on Buffer.

This sequence is not purely linear. Basic `lread` / `load` / `require` /
autoload / bootstrap invariants are a cross-cutting prerequisite and should be
audited as soon as the VM can execute meaningful Lisp. Phase 10 below is the
final end-to-end startup audit, not the first place to think about loading.

```
Phase 1    Phase 2    Phase 3    Phase 4    Phase 5
Lisp VM →  Buffer →  I18n   →  Search →  Editing
                                Read/Print
                                File I/O

Phase 6    Phase 7    Phase 8    Phase 9    Phase 10
Window  →  Display →  Command →  Process →  Startup
Frame                              Thread     Integration
Font                               Timer
Terminal
```

---

## Phase 1: Lisp VM Core

**Why first**: Everything is built on this. A wrong `eval`, `cons`, or GC bug
cascades everywhere.

### Audit items

- **eval.c** — `Feval`, `Ffuncall`, `Fapply`, `Fprogn`, special forms
  (`if`, `cond`, `let`, `let*`, `while`, `progn`, `quote`, `function`,
  `and`, `or`, `setq`, `set`, `defvar`, `defconst`, `defun`, `lambda`,
  `macro`, `condition-case`, `unwind-protect`, `catch`, `throw`)
- **data.c** — All type predicates, `type-of`, `aref`, `aset`, `length`,
  `cons`, `list`, `car`, `cdr`, `setcar`, `setcdr`, `symbol-value`,
  `symbol-function`, `fboundp`, `boundp`, `fset`, `defalias`
- **alloc.c** — GC correctness, `garbage-collect`, `make-string`,
  `make-vector`, `make-hash-table`, weak hash tables, `finalizer`
- **fns.c** — `equal`, `eq`, `eql`, `substring`, `concat`, `mapcar`,
  `mapc`, `dolist`, `dotimes`, `sort`, `reverse`, `assoc`, `assq`,
  `rassoc`, `member`, `memq`, `delete`, `delq`, `remq`, `nreverse`,
  `nconc`, `copy-sequence`, `copy-tree`, hash table operations
- **bytecode.c** — All 150+ bytecode opcodes, `byte-compile`, stack machine
  semantics
- **bignum.c** — Integer arithmetic overflow, bignum operations
- **floatfns.c** — Float operations, `floor`, `ceiling`, `round`, `truncate`,
  `ffloor`, `fceiling`

### Method

Start with focused GNU-vs-Neomacs differential oracles for `eval`, `funcall`,
special forms, GC-visible behavior, and bytecode. Reuse GNU ERT coverage where
the harness fits, but do not assume GNU's full `make check` can be dropped onto
Neomacs unchanged.

---

## Phase 2: Buffer & Text

**Why**: Buffer is the central data structure. Display, editing, search all
depend on it.

### Audit items

- **buffer.c** — `current-buffer`, `set-buffer`, `with-current-buffer`,
  `get-buffer`, `get-buffer-create`, `generate-new-buffer`, `kill-buffer`,
  `buffer-name`, `buffer-file-name`, `buffer-modified-p`, `buffer-size`,
  `buffer-string`, `buffer-substring`, `point`, `point-min`, `point-max`,
  `point-min-marker`, `point-max-marker`, `narrow-to-region`, `widen`,
  `buffer-narrowed-p`, `goto-char`, `forward-char`, `backward-char`,
  `buffer-list`, `get-file-buffer`, `other-buffer`
- **insdel.c** — `insert`, `insert-before-markers`, `insert-char`,
  `insert-buffer-substring`, `delete-region`, `delete-char`,
  `delete-backward-char`, `delete-and-extract-region`, gap motion semantics,
  modification hooks (`before-change-functions`, `after-change-functions`)
- **marker.c** — `make-marker`, `set-marker`, `marker-position`,
  `marker-buffer`, `copy-marker`, insertion-type, marker relocation during
  insertion/deletion
- **intervals.c / textprop.c** — `put-text-property`, `get-text-property`,
  `text-properties-at`, `next-single-property-change`,
  `previous-single-property-change`, `next-property-change`,
  `remove-text-properties`, `add-text-properties`, `set-text-properties`,
  `text-property-any`, `text-property-not-all`
- **itree.c** — Overlay operations, `make-overlay`, `delete-overlay`,
  `move-overlay`, `overlay-put`, `overlay-get`, `overlays-at`,
  `overlays-in`, overlay ordering
- **region-cache.c** — Region cache correctness

### Method

Compare buffer operations point-by-point. Gap buffer semantics must match
exactly. Marker relocation is particularly tricky.

---

## Phase 3: I18n / Character / Coding

**Why**: Text encoding errors corrupt data silently. Composite/bidi affect
display.

### Audit items

- **character.c** — `char-before`, `char-after`, `following-char`,
  `preceding-char`, `char-width`, `string-width`, multibyte handling
- **charset.c** — `charsetp`, `charset-info`, `encode-char`, `decode-char`,
  `split-char`
- **coding.c** — `encode-coding-string`, `decode-coding-string`,
  `encode-coding-region`, `decode-coding-region`, coding system detection,
  EOL conversion, `set-buffer-file-coding-system`,
  `set-terminal-coding-system`, `set-keyboard-coding-system`
- **composite.c** — `compose-region`, `decompose-region`,
  `composition-get-gstring`, lgstring/lglyph
- **bidi.c** — Bidirectional reordering, paragraph direction,
  `bidi-paragraph-direction`, `bidi-string-direction`, character
  directional properties
- **ccl.c** — CCL program execution (used by some coding systems)
- **category.c / casefiddle.c / casetab.c** — `upcase`, `downcase`,
  `capitalize`, `upcase-region`, `downcase-region`, category tables

### Method

Test with multilingual text (CJK, Arabic/Hebrew bidi, emoji, combining
chars). Compare encoding/decoding round-trips.

---

## Phase 4: Search, Syntax, Read/Print, File I/O

### Audit items

- **search.c / regex-emacs.c** — `search-forward`, `search-backward`,
  `re-search-forward`, `re-search-backward`, `replace-match`,
  `match-string`, `match-beginning`, `match-end`, `looking-at`,
  `looking-back`, `string-match`, `replace-regexp-in-string`, all regexp
  syntax (lazy quantifiers, backreferences, char classes, Unicode
  categories)
- **syntax.c** — `syntax-table`, `forward-word`, `backward-word`,
  `forward-sexp`, `backward-sexp`, `forward-list`, `backward-list`,
  `up-list`, `down-list`, `parse-partial-sexp`, `scan-lists`,
  `scan-sexps`, syntax properties, multibyte syntax
- **lread.c** — `read`, `read-from-string`, `read-buffer`, `intern`,
  `intern-soft`, `obarray`, `mapatoms`, load-path, `load`, `require`,
  `provide`, `autoload`, `load-file`, `eval-buffer`, `eval-region`,
  `load-source-file-function`, recursive-load limits, load history
- **print.c** — `prin1`, `prin1-to-string`, `princ`, `print`, `terpri`,
  `write-char`, circle notation, readable output
- **doc.c** — `documentation`, `Snarf-documentation`
- **fileio.c** — `find-file`, `save-buffer`, `write-file`,
  `insert-file-contents`, `write-region`, `file-exists-p`,
  `file-readable-p`, `file-writable-p`, `file-directory-p`,
  `directory-files`, `expand-file-name`, `file-name-directory`,
  `file-name-nondirectory`, `file-name-as-directory`,
  `directory-file-name`, `copy-file`, `rename-file`, `delete-file`,
  `make-directory`, `delete-directory`, file name handlers, auto-save
- **dired.c** — `directory-files-and-attributes`, `file-attributes`,
  `file-newer-than-file-p`
- **filelock.c** — File locking, `lock-buffer`, `unlock-buffer`

### Method

Regex conformance tests against GNU's test suite. File I/O: create temp
files, compare results.

---

## Phase 5: Editing Commands

### Audit items

- **editfns.c** — `region-beginning`, `region-end`, `mark`, `set-mark`,
  `push-mark`, `pop-to-mark`, `exchange-point-and-mark`, `what-line`,
  `what-cursor-position`, `line-beginning-position`,
  `line-end-position`, `count-lines`, `forward-line`,
  `buffer-substring-no-properties`, `insert-buffer-substring`, `format`,
  `message`, `user-full-name`, `current-time`, `format-time-string`,
  `decode-time`, `encode-time`
- **cmds.c** — `self-insert-command`, `newline`, `open-line`,
  `delete-blank-lines`, `transpose-chars`, `transpose-words`,
  `transpose-lines`, `zap-to-char`
- **undo.c** — `undo`, `undo-boundary`, `buffer-undo-list`, undo
  compression
- **indent.c** — `current-indentation`, `indent-to`, `move-to-column`,
  `current-column`, `indent-line-to`
- **minibuf.c** — `read-from-minibuffer`, `read-string`,
  `read-file-name`, `completing-read`, `read-buffer`, `read-command`,
  `read-variable`, `read-key-sequence`, minibuffer history

### Method

This phase mixes C primitives with Lisp wrappers. For APIs GNU owns in C, match
the primitive semantics first. For APIs GNU owns in `.el`, keep the GNU Lisp
implementation and differential-test behavior instead of reimplementing it in
Rust.

---

## Phase 6: Windowing Model

### Audit items

Separate GNU C-owned primitives in `window.c` / `frame.c` / `terminal.c` /
`font.c` from higher-level commands in `lisp/window.el`, `frame.el`, and
related Lisp files. Only the GNU C-owned surface should be reimplemented in
Rust.

- **window.c** — `selected-window`, `select-window`,
  `get-buffer-window`, `get-lru-window`, `split-window`,
  `split-window-below`, `split-window-right`, `delete-window`,
  `delete-other-windows`, `window-buffer`, `set-window-buffer`,
  `window-point`, `set-window-point`, `window-start`,
  `set-window-start`, `window-end`, `window-height`, `window-width`,
  `window-body-height`, `window-body-width`, `window-edges`,
  `window-inside-edges`, `window-pixel-edges`, `window-at`,
  `window-absolute-pixel-edges`, `window-scroll-bars`,
  `set-window-scroll-bars`, `window-fringes`, `set-window-fringes`,
  `window-vscroll`, `set-window-vscroll`, `window-prev-buffers`,
  `window-next-buffers`, `window-use-time`, window sizes,
  window combinations
- **lisp/window.el** — `split-window-below`, `split-window-right`,
  `fit-window-to-buffer`, `balance-windows`, `switch-to-buffer`,
  `window-state-get`, `window-state-put`, and related high-level window
  commands should come from GNU Lisp, not from duplicate Rust ownership
- **frame.c** — `selected-frame`, `select-frame`, `make-frame`,
  `delete-frame`, `frame-list`, `frame-parameter`,
  `set-frame-parameter`, `modify-frame-parameters`, `frame-width`,
  `frame-height`, `frame-pixel-width`, `frame-pixel-height`,
  `set-frame-size`, `set-frame-position`, `frame-visible-p`,
  `frame-live-p`, `iconify-frame`, `make-frame-visible`,
  `set-frame-name`, `frame-char-height`, `frame-char-width`,
  `frame-text-cols`, `frame-text-lines`, multi-monitor support,
  `display-pixel-width`, `display-pixel-height`,
  `display-monitor-attributes-list`
- **terminal.c** — `terminal-list`, `terminal-name`,
  `terminal-parameter`, `delete-terminal`
- **font.c / fontset.c** — `font-spec`, `font-get`, `font-put`,
  `list-fonts`, `find-font`, `font-xlfd-name`, `font-info`, `font-at`,
  `query-font`, `face-font`, `set-face-font`, `fontset-list`,
  `fontset-info`, `set-fontset-font`, `characterp`, font matching
  algorithm, `:family`, `:weight`, `:slant`, `:size`, `:width`

### Method

Create frames, split windows, verify pixel-level geometry matches.

---

## Phase 7: Display Engine

### Audit items

- **xdisp.c** — This is the big one. Redisplay cycle, line wrapping,
  truncation, hscroll, tab-line, header-line, mode-line, fringe
  indicators, margin display, glyph matrices, selective display,
  `overlay-arrow`, `display`, `face`, `invisible` text properties,
  display property (`display`, `space`, `align-to`, `margin`,
  `left-margin`, `right-margin`, `height`, `raise`, `image`),
  before-string/after-string overlays, `line-prefix`, `wrap-prefix`,
  `line-height`, `line-spacing`, text scaling (`text-scale-adjust`),
  variable-height faces, glyphless characters
- **dispnew.c** — Direct output, `redraw-frame`, `redraw-display`,
  scrolling optimization, `force-window-update`,
  `force-mode-line-update`
- **xfaces.c** — `face-at`, face merging algorithm, `face-attribute`,
  `set-face-attribute`, `face-remap-add-relative`,
  `face-remap-remove-relative`, `color-values`, `color-defined-p`,
  `defined-colors`, `color-supported-p`, `tty-defined-colors`,
  `display-color-p`, `display-grayscale-p`, face realization, font
  selection per-face, `:inherit`, `:extend`, `:box`, `:underline`,
  `:overline`, `:stipple`, `:inverse-video`
- **fringe.c** — `set-fringe-mode`, `fringe-columns`, custom fringe
  bitmaps, `define-fringe-bitmap`, `destroy-fringe-bitmap`,
  `fringe-bitmaps-at-pos`

### Method

Start with differential oracles for layout-independent display state
(`window-start`, `window-end`, fringes, scroll bars, face/overlay ownership),
then add visual regression tests once font and frame parity are strong enough
to make pixel comparisons meaningful.

---

## Phase 8: Command System

### Audit items

- **keyboard.c** — `read-key-sequence`, `read-key-sequence-vector`,
  `read-event`, `read-char`, `read-char-exclusive`, `read-key`,
  `read-password`, input decoding, keyboard macros, `recent-keys`,
  `this-command-keys`, `this-command-keys-vector`, `recursion-depth`,
  `recursive-edit`, `top-level`, `exit-recursive-edit`,
  `abort-recursive-edit`, `prefix-arg`, universal argument, mouse
  events, drag events, `event-basic-type`, `event-start`, `event-end`,
  `event-click-count`, `mouse-position`, `set-mouse-position`,
  `mouse-pixel-position`, input methods, `set-input-method`,
  `current-input-method`
- **keymap.c** — `make-keymap`, `make-sparse-keymap`, `define-key`,
  `lookup-key`, `global-key-binding`, `local-key-binding`,
  `current-active-maps`, `key-binding`, `where-is-internal`,
  `describe-bindings`, keymap parents, keymap inheritance,
  `keymap-canonicalize`
- **macros.c** — `start-kbd-macro`, `end-kbd-macro`,
  `call-last-kbd-macro`, `kbd-macro-termination-hook`
- **menu.c** — `x-popup-menu`, `popup-menu`, menu keymaps, `easy-menu`
- **callint.c** — Interactive spec parsing (`"s"`, `"n"`, `"f"`, `"b"`,
  `"B"`, `"d"`, `"r"`, `"p"`, `"P"`, `"m"`, `"*"` etc.),
  `call-interactively`, `interactive`, `funcall-interactively`

### Method

Send same keystroke sequences, compare resulting commands.

---

## Phase 9: Process, Thread, Timer

### Audit items

- **process.c** — `start-process`, `start-file-process`, `make-process`,
  `delete-process`, `process-status`, `process-exit-status`,
  `process-buffer`, `process-mark`, `accept-process-output`,
  `process-send-string`, `process-send-region`, `process-send-eof`,
  `interrupt-process`, `kill-process`, `quit-process`, `stop-process`,
  `continue-process`, `process-list`, `get-process`, `processp`,
  `set-process-buffer`, `set-process-filter`, `set-process-sentinel`,
  `set-process-coding-system`, pty vs pipe, `:connection-type`, stderr
  handler, network processes (`make-network-process`), serial processes
- **callproc.c** — `call-process`, `call-process-shell-command`,
  `process-file`, `process-file-shell-command`, `shell-command`,
  `shell-command-to-string`, environment variables, `getenv`, `setenv`
- **thread.c** — `make-thread`, `thread-join`, `thread-signal`,
  `thread-live-p`, `all-threads`, `current-thread`, `thread-name`,
  mutexes, condition variables
- **emacs-module.c** — Module API completeness
- **atimer.c / timefns.c** — `run-at-time`, `run-with-timer`,
  `run-with-idle-timer`, `cancel-timer`, `timerp`,
  `timer-incorporate`, `current-idle-time`, `current-time`,
  `time-add`, `time-subtract`, `time-less-p`, `float-time`,
  `time-to-seconds`

---

## Phase 10: Startup & Integration

### Audit items

- Bootstrap pipeline (see
  `docs/audit/bootstrap-pipeline-gnu-vs-neomacs.md`)
- `startup.el` hook ordering
- Package system
- Init file loading
- `early-init.el`
- Splash screen
- Daemon mode
- Batch mode (`--batch`)
- X resources (`--xrm`)
- Session management
- Desktop save/restore
- Image support (PNG, JPEG, SVG, WebP, GIF, TIFF, XPM)
- Tree-sitter integration
- D-Bus integration
- GnuTLS
- Native compilation (skip for Neomacs)

---

## Per-Phase Audit Method

For each phase, the workflow is:

```
1. Enumerate DEFUNs in GNU Emacs C file
   → grep '^DEFUN' emacs/src/foo.c

2. Check Neomacs Rust equivalent exists
   → grep in neovm-core/src/

3. If implemented in Rust: compare semantics
   → Write ERT test, run in both GNU Emacs and Neomacs

4. Determine GNU ownership before writing code
   → If GNU owns it in C, implement it in Rust
   → If GNU owns it in `.el`, load the GNU Lisp and avoid shadowing it with a
     Rust fallback unless bootstrap absolutely requires one

5. Document gaps
   → Missing primitives, divergent behavior, wrong defaults
```

## Gap Detection at Scale

```bash
# 1. Extract all DEFUN names from GNU Emacs
grep -h '^DEFUN' /path/to/gnu/emacs/src/*.c | \
  sed 's/.*"\([^"]*\)".*/\1/' | sort > /tmp/gnu_defuns.txt

# 2. Extract all Rust builtin names from Neomacs
grep -rh 'intern\|"defun\|Subr(' neovm-core/src/ | \
  # extract names | sort > /tmp/neomacs_builtins.txt

# 3. diff
comm -23 /tmp/gnu_defuns.txt /tmp/neomacs_builtins.txt
```

This is only a seed inventory. It will miss aliases, autoloaded Lisp entry
points, generated registrations, and post-bootstrap function-cell rewrites, so
it should not be treated as the final ownership truth.
