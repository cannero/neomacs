//! Evaluator — special forms, function application, and dispatch.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::abbrev::AbbrevManager;
use super::advice::VariableWatcherList;
use super::autoload::AutoloadManager;
use super::bookmark::BookmarkManager;
use super::builtins;
use super::bytecode::Compiler;
use super::category::CategoryManager;
use super::coding::CodingSystemManager;
use super::custom::CustomManager;
use super::doc::{STARTUP_VARIABLE_DOC_STRING_PROPERTIES, STARTUP_VARIABLE_DOC_STUBS};
use super::error::*;
use super::expr::Expr;
use super::interactive::InteractiveRegistry;
use super::keymap::{list_keymap_set_parent, make_list_keymap, make_sparse_list_keymap};
use super::kill_ring::KillRing;
use super::kmacro::KmacroManager;
use super::mode::ModeRegistry;
use super::process::ProcessManager;
use super::rect::RectangleState;
use super::regex::MatchData;
use super::register::RegisterManager;
use super::symbol::Obarray;
use super::threads::ThreadManager;
use super::timer::TimerManager;
use super::intern::{intern, resolve_sym, set_current_interner, StringInterner, SymId};
use super::value::*;
use crate::buffer::BufferManager;
use crate::face::FaceTable;
use crate::gc::heap::LispHeap;
use crate::gc::ObjId;
use crate::gc::GcTrace;
use crate::window::FrameManager;

/// Compute a content fingerprint of a macro call's args slice.
///
/// Used to detect ABA in the macro expansion cache: when a lambda body
/// `Rc<Vec<Expr>>` is freed and its memory reused, `tail.as_ptr()` can
/// match a stale cache entry.  The fingerprint catches this by hashing
/// a summary of the actual Expr nodes.
fn tail_fingerprint(tail: &[Expr]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tail.len().hash(&mut hasher);
    for (i, expr) in tail.iter().enumerate() {
        i.hash(&mut hasher);
        expr_fingerprint(expr, &mut hasher, 3);
    }
    hasher.finish()
}

fn expr_fingerprint(expr: &Expr, hasher: &mut impl std::hash::Hasher, depth: usize) {
    use std::hash::Hash;
    std::mem::discriminant(expr).hash(hasher);
    if depth == 0 {
        return;
    }
    match expr {
        Expr::Symbol(id) | Expr::Keyword(id) => id.0.hash(hasher),
        Expr::Int(n) => n.hash(hasher),
        Expr::Char(c) => c.hash(hasher),
        Expr::Float(f) => f.to_bits().hash(hasher),
        Expr::Str(s) => s.hash(hasher),
        Expr::Bool(b) => b.hash(hasher),
        Expr::List(items) | Expr::Vector(items) => {
            items.len().hash(hasher);
            for item in items.iter().take(4) {
                expr_fingerprint(item, hasher, depth - 1);
            }
        }
        Expr::DottedList(items, tail) => {
            items.len().hash(hasher);
            for item in items.iter().take(3) {
                expr_fingerprint(item, hasher, depth - 1);
            }
            expr_fingerprint(tail, hasher, depth - 1);
        }
        Expr::OpaqueValue(v) => {
            std::mem::discriminant(v).hash(hasher);
        }
    }
}

#[derive(Clone, Debug)]
enum NamedCallTarget {
    Obarray(Value),
    EvaluatorCallable,
    Probe,
    Builtin,
    SpecialForm,
    Void,
}

#[derive(Clone, Debug)]
enum BackquoteElement {
    Item(Value),
    Splice(Vec<Value>),
}

#[derive(Clone, Debug)]
struct NamedCallCache {
    symbol: SymId,
    function_epoch: u64,
    target: NamedCallTarget,
}

/// Limit for stored recent input events to match GNU Emacs: 300 entries.
pub(crate) const RECENT_INPUT_EVENT_LIMIT: usize = 300;

/// Collect GC roots from all thread-local statics that hold Values.
///
/// Thread-local statics are invisible to the normal GC root scan (which
/// only walks the Evaluator struct and its sub-managers).  This function
/// calls each module's `collect_*_gc_roots` helper to ensure those Values
/// are marked as live during garbage collection.
fn collect_thread_local_gc_roots(roots: &mut Vec<Value>) {
    super::value::collect_string_text_prop_gc_roots(roots);
    super::syntax::collect_syntax_gc_roots(roots);
    super::casetab::collect_casetab_gc_roots(roots);
    super::category::collect_category_gc_roots(roots);
    super::terminal::pure::collect_terminal_gc_roots(roots);
    super::font::collect_font_gc_roots(roots);
    super::ccl::collect_ccl_gc_roots(roots);
}

/// The Elisp evaluator.
///
/// # Safety: Send
/// Evaluator is inherently single-threaded (uses thread-local heap + interner).
/// `neovm-worker` moves the Evaluator to a worker thread inside
/// `Arc<Mutex<..>>`, which ensures exclusive access.
// SAFETY: Rc is !Send only because it uses non-atomic refcounting.
// Since Evaluator is always used single-threaded (guarded by Mutex when
// transferred between threads), this is safe.
unsafe impl Send for Evaluator {}

pub struct Evaluator {
    /// String interner for symbol/keyword/subr names (SymId handles).
    pub(crate) interner: Box<StringInterner>,
    /// GC-managed heap for cycle-forming Lisp objects (cons, vector, hash-table).
    pub(crate) heap: Box<LispHeap>,
    /// The obarray — unified symbol table with value cells, function cells, plists.
    pub(crate) obarray: Obarray,
    /// Dynamic binding stack (each frame is one `let`/function call scope).
    pub(crate) dynamic: Vec<OrderedSymMap>,
    /// Lexical environment: flat cons alist mirroring GNU Emacs's
    /// `Vinternal_interpreter_environment`.
    pub(crate) lexenv: Value,
    /// Features list (for require/provide).
    pub(crate) features: Vec<SymId>,
    /// Features currently being resolved through `require`.
    require_stack: Vec<SymId>,
    /// Files currently being loaded (mirrors `Vloads_in_progress` in lread.c).
    pub(crate) loads_in_progress: Vec<std::path::PathBuf>,
    /// Buffer manager — owns all live buffers and tracks current buffer.
    pub(crate) buffers: BufferManager,
    /// Match data from the last successful search/match operation.
    pub(crate) match_data: Option<MatchData>,
    /// Process manager — owns all tracked processes.
    pub(crate) processes: ProcessManager,
    /// Network manager — owns network connections, filters, and sentinels.
    /// Timer manager — owns all timers.
    pub(crate) timers: TimerManager,
    /// Variable watcher list — callbacks on variable changes.
    pub(crate) watchers: VariableWatcherList,
    /// Current buffer-local keymap (set by `use-local-map`).
    pub(crate) current_local_map: Value,
    /// Register manager — quick storage and retrieval of text, positions, etc.
    pub(crate) registers: RegisterManager,
    /// Bookmark manager — persistent named positions.
    pub(crate) bookmarks: BookmarkManager,
    /// Abbreviation manager — text abbreviation expansion.
    pub(crate) abbrevs: AbbrevManager,
    /// Autoload manager — deferred function loading.
    pub(crate) autoloads: AutoloadManager,
    /// Custom variable manager — defcustom/defgroup system.
    pub(crate) custom: CustomManager,
    /// Kill ring — clipboard/kill ring for text editing.
    pub(crate) kill_ring: KillRing,
    /// Rectangle state — stores the last killed rectangle for yank-rectangle.
    pub(crate) rectangle: RectangleState,
    /// Interactive command registry — tracks interactive commands.
    pub(crate) interactive: InteractiveRegistry,
    /// Input events consumed by read* APIs, used by `recent-keys`.
    recent_input_events: Vec<Value>,
    /// Last key sequence captured by read-key/read-key-sequence/read-event paths.
    read_command_keys: Vec<Value>,
    /// Batch-compatible input-mode interrupt flag for `current-input-mode`.
    input_mode_interrupt: bool,
    /// Frame manager — owns all frames and windows.
    pub(crate) frames: FrameManager,
    /// Mode registry — major/minor modes.
    pub(crate) modes: ModeRegistry,
    /// Thread manager — cooperative threading primitives.
    pub(crate) threads: ThreadManager,
    /// Category manager — character category tables.
    pub(crate) category_manager: CategoryManager,
    /// Keyboard macro manager — recording, playback, macro ring.
    pub(crate) kmacro: KmacroManager,
    /// Coding system manager — encoding/decoding registry.
    pub(crate) coding_systems: CodingSystemManager,
    /// Face table — global registry of named face definitions.
    pub(crate) face_table: FaceTable,
    /// Recursion depth counter.
    depth: usize,
    /// Maximum recursion depth.
    max_depth: usize,
    /// Set when allocation crosses the GC threshold; cleared by `gc_collect`.
    pub(crate) gc_pending: bool,
    /// Total number of GC collections performed.
    pub(crate) gc_count: u64,
    /// Stress-test mode: force GC at every safe point regardless of threshold.
    pub(crate) gc_stress: bool,
    /// Temporary GC roots — Values that must survive collection but aren't
    /// in any other rooted structure (e.g. intermediate results in eval_forms).
    temp_roots: Vec<Value>,
    /// Active catch tags — tracks all `catch` tags currently on the call stack.
    /// Used by `throw` to determine whether a matching catch exists: if yes,
    /// emit `Flow::Throw`; if no, signal `no-catch` immediately (matching
    /// GNU Emacs's `Fthrow` which calls `xsignal2(Qno_catch, ...)` when no
    /// catch handler is found).
    pub(crate) catch_tags: Vec<Value>,
    /// Saved lexical environments stack — when apply_lambda replaces
    /// self.lexenv with a closure's captured env, the old lexenv is pushed
    /// here so GC can still scan it.  Popped when apply_lambda restores.
    saved_lexenvs: Vec<Value>,
    /// Single-entry hot cache for named callable resolution in `funcall`/`apply`.
    named_call_cache: Option<NamedCallCache>,
    /// Monotonic `xN` counter used by macroexpand fallback paths that mirror
    /// Oracle pcase temp-symbol naming.
    pcase_macroexpand_temp_counter: usize,
    /// Cache for `quote_to_value` results keyed on `Expr` pointer identity.
    /// Ensures the same source-code literal (e.g. pcase case patterns inside
    /// a lambda body) evaluates to the same `Value` object across calls,
    /// preserving `eq` identity required by pcase's memoization cache.
    /// GC-rooted via `collect_roots`.
    pub(crate) literal_cache: HashMap<*const Expr, Value>,
    /// Cache for macro expansion results.
    ///
    /// Key: `(macro_heap_id, args_slice_ptr)` — the macro's ObjId plus the
    /// pointer to the args `&[Expr]` slice.
    ///
    /// Value: `(Rc<Expr>, u64)` — the expanded Expr tree plus a content
    /// fingerprint of the args at insertion time.  On cache hit, the
    /// fingerprint is recomputed and compared to detect ABA: when a
    /// lambda body `Rc<Vec<Expr>>` is freed during macro expansion (e.g.
    /// temporary lambdas in pcase), its memory can be reused by a new
    /// lambda body, making `tail.as_ptr()` match a stale entry whose
    /// args are completely different.  The fingerprint catches this.
    pub(crate) macro_expansion_cache: HashMap<(crate::gc::types::ObjId, usize), (Rc<Expr>, u64)>,
    /// Diagnostic counters for macro expansion cache.
    pub(crate) macro_cache_hits: u64,
    pub(crate) macro_cache_misses: u64,
    pub(crate) macro_expand_total_us: u64,
    /// When true, skip cache lookups (still populate cache for timing).
    pub(crate) macro_cache_disabled: bool,
}

impl Default for Evaluator {
    fn default() -> Self {
        Self::new()
    }
}


impl Evaluator {
    pub fn new() -> Self {
        // Create the interner and heap, set thread-locals so that Value
        // constructors (symbol, keyword, cons, list, etc.) work during init.
        let mut interner = Box::new(StringInterner::new());
        set_current_interner(&mut interner);
        let mut heap = Box::new(LispHeap::new());
        set_current_heap(&mut heap);

        // Clear any caches that hold heap-allocated Values (ObjIds) from a
        // previous heap. Critical for test isolation when multiple Evaluators
        // are created sequentially on the same thread.
        super::syntax::reset_syntax_thread_locals();
        super::casetab::reset_casetab_thread_locals();
        super::category::reset_category_thread_locals();
        super::value::reset_string_text_properties();
        super::ccl::reset_ccl_registry();
        super::dispnew::pure::reset_dispnew_thread_locals();
        super::font::clear_font_cache_state();
        super::builtins::reset_builtins_thread_locals();
        super::charset::reset_charset_registry();
        super::timefns::reset_timefns_thread_locals();

        let mut obarray = Obarray::new();
        // Mirror GNU Emacs startup: primitive names exist in the initial
        // obarray, so `(intern-soft "floatp")` etc are non-nil during
        // bootstrap macroexpansion (e.g. cl-preloaded).
        for &name in super::builtin_registry::dispatch_builtin_names() {
            obarray.intern(name);
        }
        let default_directory = std::env::current_dir()
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
            .map(|mut s| {
                if !s.ends_with('/') {
                    s.push('/');
                }
                s
            })
            .unwrap_or_else(|| "./".to_string());
        // Create all keymaps as Emacs-compatible cons-list values
        let completion_in_region_mode_map = make_sparse_list_keymap();
        let completion_list_mode_map = make_sparse_list_keymap();
        let minibuffer_local_map = make_sparse_list_keymap();
        let minibuffer_local_completion_map = make_sparse_list_keymap();
        let minibuffer_local_filename_completion_map = make_sparse_list_keymap();
        let minibuffer_local_must_match_map = make_sparse_list_keymap();
        let minibuffer_local_ns_map = make_sparse_list_keymap();
        let minibuffer_local_shell_command_map = make_sparse_list_keymap();
        let minibuffer_local_isearch_map = make_sparse_list_keymap();
        let minibuffer_inactive_mode_map = make_sparse_list_keymap();
        let minibuffer_mode_map = make_sparse_list_keymap();
        let minibuffer_visible_completions_map = make_sparse_list_keymap();
        let read_expression_map = make_sparse_list_keymap();
        let read_expression_internal_map = make_sparse_list_keymap();
        let read_char_from_minibuffer_map = make_sparse_list_keymap();
        let read_extended_command_mode_map = make_sparse_list_keymap();
        let read_regexp_map = make_sparse_list_keymap();
        let read_key_empty_map = make_sparse_list_keymap();
        let read_key_full_map = make_list_keymap();
        // Standard keymaps required by loadup.el files (normally created by C code)
        let global_map = make_list_keymap();
        let esc_map = make_sparse_list_keymap();
        let ctl_x_map = make_sparse_list_keymap();
        let special_event_map = make_sparse_list_keymap();
        let help_map = make_sparse_list_keymap();
        let mode_line_window_dedicated_keymap = make_sparse_list_keymap();
        let indent_rigidly_map = make_sparse_list_keymap();
        let text_mode_map = make_sparse_list_keymap();
        let image_slice_map = make_sparse_list_keymap();
        let tool_bar_map = make_sparse_list_keymap();
        let key_translation_map = make_sparse_list_keymap();
        let function_key_map = make_sparse_list_keymap();
        let input_decode_map = make_sparse_list_keymap();
        let local_function_key_map = make_sparse_list_keymap();

        list_keymap_set_parent(minibuffer_local_completion_map, minibuffer_local_map);
        list_keymap_set_parent(
            minibuffer_local_filename_completion_map,
            minibuffer_local_completion_map,
        );
        list_keymap_set_parent(
            minibuffer_local_must_match_map,
            minibuffer_local_completion_map,
        );
        list_keymap_set_parent(minibuffer_local_ns_map, minibuffer_local_map);
        list_keymap_set_parent(
            minibuffer_local_shell_command_map,
            minibuffer_local_map,
        );
        list_keymap_set_parent(minibuffer_local_isearch_map, minibuffer_local_map);
        list_keymap_set_parent(minibuffer_mode_map, minibuffer_local_map);
        list_keymap_set_parent(read_expression_map, minibuffer_local_map);
        list_keymap_set_parent(read_expression_internal_map, read_expression_map);
        list_keymap_set_parent(read_char_from_minibuffer_map, minibuffer_local_map);
        list_keymap_set_parent(read_extended_command_mode_map, minibuffer_local_map);
        list_keymap_set_parent(read_regexp_map, minibuffer_local_map);
        list_keymap_set_parent(
            minibuffer_visible_completions_map,
            completion_list_mode_map,
        );

        let standard_syntax_table = super::syntax::builtin_standard_syntax_table(Vec::new())
            .expect("startup seeding requires standard syntax table");

        // Set up standard global variables
        obarray.set_symbol_value("most-positive-fixnum", Value::Int(i64::MAX));
        obarray.set_symbol_value("most-negative-fixnum", Value::Int(i64::MIN));
        obarray.set_symbol_value("emacs-version", Value::string("29.1"));
        obarray.set_symbol_value("system-type", Value::symbol("gnu/linux"));
        obarray.set_symbol_value(
            "default-directory",
            Value::string(default_directory.clone()),
        );
        obarray.set_symbol_value(
            "command-line-default-directory",
            Value::string(default_directory),
        );
        let obarray_object = Value::vector(vec![Value::Nil]);
        obarray.set_symbol_value("obarray", obarray_object);
        obarray.set_symbol_value("neovm--obarray-object", obarray_object);
        obarray.make_special("obarray");
        obarray.set_symbol_value(
            "command-line-args",
            Value::list(vec![
                Value::string("neovm-worker"),
                Value::string("--batch"),
            ]),
        );
        obarray.set_symbol_value("command-line-args-left", Value::Nil);
        obarray.set_symbol_value("command-line-functions", Value::Nil);
        obarray.set_symbol_value("command-line-processed", Value::True);
        obarray.set_symbol_value("command-switch-alist", Value::Nil);
        obarray.set_symbol_value(
            "command-line-ns-option-alist",
            Value::list(vec![Value::list(vec![
                Value::string("-NSOpen"),
                Value::Int(1),
                Value::symbol("ns-handle-nxopen"),
            ])]),
        );
        obarray.set_symbol_value(
            "command-line-x-option-alist",
            Value::list(vec![Value::list(vec![
                Value::string("-display"),
                Value::Int(1),
                Value::symbol("x-handle-display"),
            ])]),
        );
        obarray.set_symbol_value("load-path", Value::Nil);
        obarray.set_symbol_value("load-history", Value::Nil);
        // In official Emacs, load-suffixes is (".elc" ".el"), but neomacs
        // only supports .el.
        obarray.set_symbol_value(
            "load-suffixes",
            Value::list(vec![Value::string(".el")]),
        );
        // load-file-rep-suffixes: suffixes for alternate representations of
        // the same file (e.g., compressed ".gz").  Default is just ("").
        obarray.set_symbol_value(
            "load-file-rep-suffixes",
            Value::list(vec![Value::string("")]),
        );
        // file-coding-system-alist: needed by jka-cmpr-hook.el and others.
        obarray.set_symbol_value("file-coding-system-alist", Value::Nil);
        obarray.set_symbol_value("features", Value::Nil);
        obarray.set_symbol_value("debug-on-error", Value::Nil);
        obarray.set_symbol_value("lexical-binding", Value::Nil);
        obarray.set_symbol_value("load-prefer-newer", Value::Nil);
        obarray.set_symbol_value("load-file-name", Value::Nil);
        obarray.set_symbol_value("noninteractive", Value::True);
        obarray.set_symbol_value("inhibit-quit", Value::Nil);
        obarray.set_symbol_value("print-length", Value::Nil);
        obarray.set_symbol_value("print-level", Value::Nil);
        obarray.set_symbol_value("standard-output", Value::True);
        obarray.set_symbol_value("buffer-read-only", Value::Nil);
        obarray.set_symbol_value("kill-ring", Value::Nil);
        obarray.set_symbol_value("kill-ring-yank-pointer", Value::Nil);
        obarray.set_symbol_value("last-command", Value::Nil);
        obarray.set_symbol_value("current-fill-column--has-warned", Value::Nil);
        obarray.set_symbol_value("current-input-method", Value::Nil);
        obarray.set_symbol_value("current-input-method-title", Value::Nil);
        obarray.set_symbol_value("current-iso639-language", Value::Nil);
        obarray.set_symbol_value("current-key-remap-sequence", Value::Nil);
        obarray.set_symbol_value("current-language-environment", Value::string("UTF-8"));
        obarray.set_symbol_value(
            "current-load-list",
            Value::list(vec![
                Value::symbol("comp--no-native-compile"),
                Value::cons(
                    Value::symbol("defun"),
                    Value::symbol("load--fixup-all-elns"),
                ),
                Value::symbol("load--eln-dest-dir"),
                Value::symbol("load--bin-dest-dir"),
            ]),
        );
        obarray.set_symbol_value("current-locale-environment", Value::string("C.UTF-8"));
        obarray.set_symbol_value("current-minibuffer-command", Value::Nil);
        obarray.set_symbol_value("current-time-list", Value::True);
        obarray.set_symbol_value("current-transient-input-method", Value::Nil);
        obarray.set_symbol_value("real-last-command", Value::Nil);
        obarray.set_symbol_value("last-repeatable-command", Value::Nil);
        obarray.set_symbol_value("this-original-command", Value::Nil);
        obarray.set_symbol_value("prefix-arg", Value::Nil);
        obarray.set_symbol_value("defining-kbd-macro", Value::Nil);
        obarray.set_symbol_value("executing-kbd-macro", Value::Nil);
        obarray.set_symbol_value("executing-kbd-macro-index", Value::Int(0));
        obarray.set_symbol_value("command-history", Value::Nil);
        obarray.set_symbol_value("extended-command-history", Value::Nil);
        obarray.set_symbol_value("completion-ignore-case", Value::Nil);
        obarray.set_symbol_value("read-buffer-completion-ignore-case", Value::Nil);
        obarray.set_symbol_value("read-file-name-completion-ignore-case", Value::Nil);
        obarray.set_symbol_value("completion-regexp-list", Value::Nil);
        obarray.set_symbol_value("completion--all-sorted-completions-location", Value::Nil);
        obarray.set_symbol_value("completion--capf-misbehave-funs", Value::Nil);
        obarray.set_symbol_value("completion--capf-safe-funs", Value::Nil);
        obarray.set_symbol_value(
            "completion--embedded-envvar-re",
            Value::string(
                "\\(?:^\\|[^$]\\(?:\\$\\$\\)*\\)\\$\\([[:alnum:]_]*\\|{\\([^}]*\\)\\)\\'",
            ),
        );
        obarray.set_symbol_value("completion--flex-score-last-md", Value::Nil);
        obarray.set_symbol_value("completion-all-sorted-completions", Value::Nil);
        obarray.set_symbol_value(
            "completion--cycling-threshold-type",
            Value::list(vec![Value::symbol("choice")]),
        );
        obarray.set_symbol_value(
            "completion--styles-type",
            Value::list(vec![Value::symbol("repeat")]),
        );
        obarray.set_symbol_value(
            "completion-at-point-functions",
            Value::list(vec![Value::symbol("tags-completion-at-point-function")]),
        );
        obarray.set_symbol_value(
            "completion-setup-hook",
            Value::list(vec![Value::symbol("completion-setup-function")]),
        );
        obarray.set_symbol_value(
            "completion-in-region-mode-map",
            completion_in_region_mode_map,
        );
        obarray.set_symbol_value(
            "completion-list-mode-map",
            completion_list_mode_map,
        );
        obarray.set_symbol_value(
            "completion-list-mode-syntax-table",
            standard_syntax_table,
        );
        obarray.set_symbol_value(
            "completion-list-mode-abbrev-table",
            Value::symbol("completion-list-mode-abbrev-table"),
        );
        obarray.set_symbol_value("completion-list-mode-hook", Value::Nil);
        obarray.set_symbol_value(
            "completion-ignored-extensions",
            Value::list(vec![
                Value::string(".o"),
                Value::string("~"),
                Value::string(".elc"),
            ]),
        );
        obarray.set_symbol_value(
            "completion-styles",
            Value::list(vec![
                Value::symbol("basic"),
                Value::symbol("partial-completion"),
                Value::symbol("emacs22"),
            ]),
        );
        obarray.set_symbol_value(
            "completion-category-defaults",
            Value::list(vec![
                Value::list(vec![
                    Value::symbol("buffer"),
                    Value::list(vec![
                        Value::symbol("styles"),
                        Value::symbol("basic"),
                        Value::symbol("substring"),
                    ]),
                ]),
                Value::list(vec![
                    Value::symbol("unicode-name"),
                    Value::list(vec![
                        Value::symbol("styles"),
                        Value::symbol("basic"),
                        Value::symbol("substring"),
                    ]),
                ]),
                Value::list(vec![
                    Value::symbol("project-file"),
                    Value::list(vec![Value::symbol("styles"), Value::symbol("substring")]),
                ]),
                Value::list(vec![
                    Value::symbol("xref-location"),
                    Value::list(vec![Value::symbol("styles"), Value::symbol("substring")]),
                ]),
                Value::list(vec![
                    Value::symbol("info-menu"),
                    Value::list(vec![
                        Value::symbol("styles"),
                        Value::symbol("basic"),
                        Value::symbol("substring"),
                    ]),
                ]),
                Value::list(vec![
                    Value::symbol("symbol-help"),
                    Value::list(vec![
                        Value::symbol("styles"),
                        Value::symbol("basic"),
                        Value::symbol("shorthand"),
                        Value::symbol("substring"),
                    ]),
                ]),
                Value::list(vec![
                    Value::symbol("calendar-month"),
                    Value::cons(
                        Value::symbol("display-sort-function"),
                        Value::symbol("identity"),
                    ),
                ]),
            ]),
        );
        obarray.set_symbol_value(
            "completion-styles-alist",
            Value::list(vec![
                Value::list(vec![
                    Value::symbol("basic"),
                    Value::symbol("completion-basic-try-completion"),
                    Value::symbol("completion-basic-all-completions"),
                    Value::string(
                        "Completion of the prefix before point and the suffix after point.",
                    ),
                ]),
                Value::list(vec![
                    Value::symbol("partial-completion"),
                    Value::symbol("completion-pcm-try-completion"),
                    Value::symbol("completion-pcm-all-completions"),
                    Value::string("Completion of multiple words, each one taken as a prefix."),
                ]),
                Value::list(vec![
                    Value::symbol("emacs22"),
                    Value::symbol("completion-emacs22-try-completion"),
                    Value::symbol("completion-emacs22-all-completions"),
                    Value::string("Prefix completion that only operates on the text before point."),
                ]),
            ]),
        );
        obarray.set_symbol_value("completion-category-overrides", Value::Nil);
        obarray.set_symbol_value("completion-cycle-threshold", Value::Nil);
        obarray.set_symbol_value("completions-detailed", Value::Nil);
        obarray.set_symbol_value("completions-format", Value::symbol("horizontal"));
        obarray.set_symbol_value("completions-group", Value::Nil);
        obarray.set_symbol_value("completions-group-format", Value::string("     %s  "));
        obarray.set_symbol_value("completions-group-sort", Value::Nil);
        obarray.set_symbol_value(
            "completions-header-format",
            Value::string("%s possible completions:\n"),
        );
        obarray.set_symbol_value(
            "completions-highlight-face",
            Value::symbol("completions-highlight"),
        );
        obarray.set_symbol_value("completions-max-height", Value::Nil);
        obarray.set_symbol_value("completions-sort", Value::symbol("alphabetical"));
        obarray.set_symbol_value("completion-auto-help", Value::True);
        obarray.set_symbol_value("completion-auto-deselect", Value::True);
        obarray.set_symbol_value("completion-auto-select", Value::Nil);
        obarray.set_symbol_value("completion-auto-wrap", Value::True);
        obarray.set_symbol_value("completion-base-position", Value::Nil);
        obarray.set_symbol_value("completion-cycling", Value::Nil);
        obarray.set_symbol_value("completion-extra-properties", Value::Nil);
        obarray.set_symbol_value("completion-fail-discreetly", Value::Nil);
        obarray.set_symbol_value("completion-flex-nospace", Value::Nil);
        obarray.set_symbol_value("completion-in-region--data", Value::Nil);
        obarray.set_symbol_value(
            "completion-in-region-function",
            Value::symbol("completion--in-region"),
        );
        obarray.set_symbol_value("completion-in-region-functions", Value::Nil);
        obarray.set_symbol_value("completion-in-region-mode", Value::Nil);
        obarray.set_symbol_value("completion-in-region-mode--predicate", Value::Nil);
        obarray.set_symbol_value("completion-in-region-mode-hook", Value::Nil);
        obarray.set_symbol_value("completion-in-region-mode-predicate", Value::Nil);
        obarray.set_symbol_value("completion-show-help", Value::True);
        obarray.set_symbol_value("completion-show-inline-help", Value::True);
        obarray.set_symbol_value("completion-lazy-hilit", Value::Nil);
        obarray.set_symbol_value("completion-lazy-hilit-fn", Value::Nil);
        obarray.set_symbol_value(
            "completion-list-insert-choice-function",
            Value::symbol("completion--replace"),
        );
        obarray.set_symbol_value("completion-no-auto-exit", Value::Nil);
        obarray.set_symbol_value(
            "completion-pcm--delim-wild-regex",
            Value::string("[-_./:| *]"),
        );
        obarray.set_symbol_value("completion-pcm--regexp", Value::Nil);
        obarray.set_symbol_value(
            "completion-pcm-complete-word-inserts-delimiters",
            Value::Nil,
        );
        obarray.set_symbol_value("completion-pcm-word-delimiters", Value::string("-_./:| "));
        obarray.set_symbol_value("completion-reference-buffer", Value::Nil);
        obarray.set_symbol_value("completion-tab-width", Value::Nil);
        obarray.set_symbol_value("enable-recursive-minibuffers", Value::Nil);
        obarray.set_symbol_value("history-length", Value::Int(100));
        obarray.set_symbol_value("history-delete-duplicates", Value::Nil);
        obarray.set_symbol_value("history-add-new-input", Value::True);
        obarray.set_symbol_value("read-buffer-function", Value::Nil);
        obarray.set_symbol_value(
            "read-file-name-function",
            Value::symbol("read-file-name-default"),
        );
        obarray.set_symbol_value("read-expression-history", Value::Nil);
        obarray.set_symbol_value("read-number-history", Value::Nil);
        obarray.set_symbol_value("read-char-history", Value::Nil);
        obarray.set_symbol_value("read-answer-short", Value::symbol("auto"));
        obarray.set_symbol_value("read-char-by-name-sort", Value::Nil);
        obarray.set_symbol_value("read-char-choice-use-read-key", Value::Nil);
        obarray.set_symbol_value("read-circle", Value::True);
        obarray.set_symbol_value("read-envvar-name-history", Value::Nil);
        obarray.set_symbol_value("read-face-name-sample-text", Value::string("SAMPLE"));
        obarray.set_symbol_value("read-key-delay", Value::Float(0.01, next_float_id()));
        obarray.set_symbol_value(
            "read-answer-map--memoize",
            Value::hash_table(HashTableTest::Equal),
        );
        obarray.set_symbol_value(
            "read-char-from-minibuffer-map",
            read_char_from_minibuffer_map,
        );
        obarray.set_symbol_value(
            "read-char-from-minibuffer-map-hash",
            Value::hash_table(HashTableTest::Equal),
        );
        obarray.set_symbol_value("read-expression-map", read_expression_map);
        obarray.set_symbol_value(
            "read--expression-map",
            read_expression_internal_map,
        );
        obarray.set_symbol_value(
            "read-extended-command-mode-map",
            read_extended_command_mode_map,
        );
        obarray.set_symbol_value("read-key-empty-map", read_key_empty_map);
        obarray.set_symbol_value("read-key-full-map", read_key_full_map);
        obarray.set_symbol_value("read-regexp-map", read_regexp_map);
        obarray.set_symbol_value("read-extended-command-mode", Value::Nil);
        obarray.set_symbol_value("read-extended-command-mode-hook", Value::Nil);
        obarray.set_symbol_value("read-extended-command-predicate", Value::Nil);
        obarray.set_symbol_value("read-hide-char", Value::Nil);
        obarray.set_symbol_value("read-mail-command", Value::symbol("rmail"));
        obarray.set_symbol_value("read-minibuffer-restore-windows", Value::True);
        obarray.set_symbol_value("read-only-mode-hook", Value::Nil);
        obarray.set_symbol_value("read-process-output-max", Value::Int(65536));
        obarray.set_symbol_value("read-quoted-char-radix", Value::Int(8));
        obarray.set_symbol_value("read-regexp--case-fold", Value::Nil);
        obarray.set_symbol_value("read-regexp-defaults-function", Value::Nil);
        obarray.set_symbol_value("read-symbol-shorthands", Value::Nil);
        obarray.set_symbol_value(
            "minibuffer-frame-alist",
            Value::list(vec![
                Value::cons(Value::symbol("width"), Value::Int(80)),
                Value::cons(Value::symbol("height"), Value::Int(2)),
            ]),
        );
        obarray.set_symbol_value(
            "minibuffer-inactive-mode-abbrev-table",
            Value::symbol("minibuffer-inactive-mode-abbrev-table"),
        );
        obarray.set_symbol_value("minibuffer-inactive-mode-hook", Value::Nil);
        obarray.set_symbol_value(
            "minibuffer-inactive-mode-map",
            minibuffer_inactive_mode_map,
        );
        obarray.set_symbol_value(
            "minibuffer-inactive-mode-syntax-table",
            standard_syntax_table,
        );
        obarray.set_symbol_value(
            "minibuffer-mode-abbrev-table",
            Value::symbol("minibuffer-mode-abbrev-table"),
        );
        obarray.set_symbol_value("minibuffer-mode-hook", Value::Nil);
        obarray.set_symbol_value("minibuffer-mode-map", minibuffer_mode_map);
        obarray.set_symbol_value("minibuffer-local-map", minibuffer_local_map);
        obarray.set_symbol_value(
            "minibuffer-local-completion-map",
            minibuffer_local_completion_map,
        );
        obarray.set_symbol_value(
            "minibuffer-local-filename-completion-map",
            minibuffer_local_filename_completion_map,
        );
        obarray.set_symbol_value(
            "minibuffer-local-filename-syntax",
            standard_syntax_table,
        );
        obarray.set_symbol_value(
            "minibuffer-local-isearch-map",
            minibuffer_local_isearch_map,
        );
        obarray.set_symbol_value(
            "minibuffer-local-must-match-map",
            minibuffer_local_must_match_map,
        );
        obarray.set_symbol_value(
            "minibuffer-local-ns-map",
            minibuffer_local_ns_map,
        );
        obarray.set_symbol_value(
            "minibuffer-local-shell-command-map",
            minibuffer_local_shell_command_map,
        );
        obarray.set_symbol_value("minibuffer-history", Value::Nil);
        obarray.set_symbol_value(
            "minibuffer-history-variable",
            Value::symbol("minibuffer-history"),
        );
        obarray.set_symbol_value("minibuffer-history-position", Value::Nil);
        obarray.set_symbol_value("minibuffer-history-isearch-message-overlay", Value::Nil);
        obarray.set_symbol_value("minibuffer-history-search-history", Value::Nil);
        obarray.set_symbol_value("minibuffer-history-sexp-flag", Value::Nil);
        obarray.set_symbol_value("minibuffer-default", Value::Nil);
        obarray.set_symbol_value("minibuffer-default-add-done", Value::Nil);
        obarray.set_symbol_value(
            "minibuffer-default-add-function",
            Value::symbol("minibuffer-default-add-completions"),
        );
        obarray.set_symbol_value("minibuffer--original-buffer", Value::Nil);
        obarray.set_symbol_value("minibuffer--regexp-primed", Value::Nil);
        obarray.set_symbol_value(
            "minibuffer--regexp-prompt-regexp",
            Value::string(
                "\\(?:Posix search\\|RE search\\|Search for regexp\\|Query replace regexp\\)",
            ),
        );
        obarray.set_symbol_value("minibuffer--require-match", Value::Nil);
        obarray.set_symbol_value("minibuffer-auto-raise", Value::Nil);
        obarray.set_symbol_value("minibuffer-follows-selected-frame", Value::True);
        obarray.set_symbol_value(
            "minibuffer-exit-hook",
            Value::list(vec![
                Value::symbol("minibuffer--regexp-exit"),
                Value::symbol("minibuffer-exit-on-screen-keyboard"),
                Value::symbol("minibuffer-restore-windows"),
            ]),
        );
        obarray.set_symbol_value("minibuffer-completion-table", Value::Nil);
        obarray.set_symbol_value("minibuffer-completion-predicate", Value::Nil);
        obarray.set_symbol_value("minibuffer-completion-confirm", Value::Nil);
        obarray.set_symbol_value("minibuffer-completion-auto-choose", Value::True);
        obarray.set_symbol_value("minibuffer-completion-base", Value::Nil);
        obarray.set_symbol_value("minibuffer-help-form", Value::Nil);
        obarray.set_symbol_value("minibuffer-completing-file-name", Value::Nil);
        obarray.set_symbol_value("minibuffer-regexp-mode", Value::True);
        obarray.set_symbol_value("minibuffer-regexp-mode-hook", Value::Nil);
        obarray.set_symbol_value(
            "minibuffer-regexp-prompts",
            Value::list(vec![
                Value::string("Posix search"),
                Value::string("RE search"),
                Value::string("Search for regexp"),
                Value::string("Query replace regexp"),
            ]),
        );
        obarray.set_symbol_value("minibuffer-message-clear-timeout", Value::Nil);
        obarray.set_symbol_value("minibuffer-message-overlay", Value::Nil);
        obarray.set_symbol_value("minibuffer-message-properties", Value::Nil);
        obarray.set_symbol_value("minibuffer-message-timeout", Value::Int(2));
        obarray.set_symbol_value("minibuffer-message-timer", Value::Nil);
        obarray.set_symbol_value("minibuffer-lazy-count-format", Value::string("%s "));
        obarray.set_symbol_value("minibuffer-text-before-history", Value::Nil);
        obarray.set_symbol_value(
            "minibuffer-prompt-properties",
            Value::list(vec![
                Value::symbol("read-only"),
                Value::True,
                Value::symbol("face"),
                Value::symbol("minibuffer-prompt"),
            ]),
        );
        obarray.set_symbol_value("minibuffer-allow-text-properties", Value::Nil);
        obarray.set_symbol_value("minibuffer-scroll-window", Value::Nil);
        obarray.set_symbol_value("minibuffer-visible-completions", Value::Nil);
        obarray.set_symbol_value("minibuffer-visible-completions--always-bind", Value::Nil);
        obarray.set_symbol_value(
            "minibuffer-visible-completions-map",
            minibuffer_visible_completions_map,
        );
        obarray.set_symbol_value("minibuffer-depth-indicate-mode", Value::Nil);
        obarray.set_symbol_value(
            "minibuffer-default-prompt-format",
            Value::string(" (default %s)"),
        );
        obarray.set_symbol_value("minibuffer-beginning-of-buffer-movement", Value::Nil);
        obarray.set_symbol_value("minibuffer-electric-default-mode", Value::Nil);
        obarray.set_symbol_value("minibuffer-temporary-goal-position", Value::Nil);
        obarray.set_symbol_value(
            "minibuffer-confirm-exit-commands",
            Value::list(vec![
                Value::symbol("completion-at-point"),
                Value::symbol("minibuffer-complete"),
                Value::symbol("minibuffer-complete-word"),
            ]),
        );
        obarray.set_symbol_value("minibuffer-history-case-insensitive-variables", Value::Nil);
        obarray.set_symbol_value("minibuffer-on-screen-keyboard-displayed", Value::Nil);
        obarray.set_symbol_value("minibuffer-on-screen-keyboard-timer", Value::Nil);
        obarray.set_symbol_value(
            "minibuffer-setup-hook",
            Value::list(vec![
                Value::symbol("rfn-eshadow-setup-minibuffer"),
                Value::symbol("minibuffer--regexp-setup"),
                Value::symbol("minibuffer-setup-on-screen-keyboard"),
                Value::symbol("minibuffer-error-initialize"),
                Value::symbol("minibuffer-history-isearch-setup"),
                Value::symbol("minibuffer-history-initialize"),
            ]),
        );
        obarray.set_symbol_value("regexp-search-ring", Value::Nil);
        obarray.set_symbol_value("regexp-search-ring-max", Value::Int(16));
        obarray.set_symbol_value("regexp-search-ring-yank-pointer", Value::Nil);
        obarray.set_symbol_value("search-ring", Value::Nil);
        obarray.set_symbol_value("search-ring-max", Value::Int(16));
        obarray.set_symbol_value("search-ring-update", Value::Nil);
        obarray.set_symbol_value("search-ring-yank-pointer", Value::Nil);
        obarray.set_symbol_value("last-abbrev", Value::Nil);
        obarray.set_symbol_value("last-abbrev-location", Value::Int(0));
        obarray.set_symbol_value("last-abbrev-text", Value::Nil);
        obarray.set_symbol_value("last-command-event", Value::Nil);
        // last-event-frame is set by keyboard::pure::register_bootstrap_vars
        obarray.set_symbol_value("last-event-device", Value::Nil);
        obarray.set_symbol_value("last-input-event", Value::Nil);
        obarray.set_symbol_value("last-nonmenu-event", Value::Nil);
        obarray.set_symbol_value("last-prefix-arg", Value::Nil);
        obarray.set_symbol_value("last-kbd-macro", Value::Nil);
        obarray.set_symbol_value("last-code-conversion-error", Value::Nil);
        obarray.set_symbol_value("last-coding-system-specified", Value::Nil);
        obarray.set_symbol_value("last-coding-system-used", Value::symbol("undecided-unix"));
        obarray.set_symbol_value("last-next-selection-coding-system", Value::Nil);
        obarray.set_symbol_value("command-debug-status", Value::Nil);
        obarray.set_symbol_value(
            "command-error-function",
            Value::symbol("help-command-error-confusable-suggestions"),
        );
        obarray.set_symbol_value("key-substitution-in-progress", Value::Nil);
        obarray.set_symbol_value("this-command", Value::Nil);
        obarray.set_symbol_value("real-this-command", Value::Nil);
        obarray.set_symbol_value("this-command-keys-shift-translated", Value::Nil);
        obarray.set_symbol_value("current-prefix-arg", Value::Nil);
        obarray.set_symbol_value("track-mouse", Value::Nil);
        obarray.set_symbol_value("throw-on-input", Value::Nil);
        obarray.set_symbol_value(
            "while-no-input-ignore-events",
            Value::list(vec![
                Value::symbol("thread-event"),
                Value::symbol("file-notify"),
                Value::symbol("dbus-event"),
                Value::symbol("select-window"),
                Value::symbol("help-echo"),
                Value::symbol("move-frame"),
                Value::symbol("iconify-frame"),
                Value::symbol("make-frame-visible"),
                Value::symbol("focus-in"),
                Value::symbol("focus-out"),
                Value::symbol("config-changed-event"),
                Value::symbol("selection-request"),
            ]),
        );
        obarray.set_symbol_value("deactivate-mark", Value::True);
        obarray.set_symbol_value("mark-active", Value::Nil);
        obarray.set_symbol_value("mark-even-if-inactive", Value::True);
        obarray.set_symbol_value("mark-ring", Value::Nil);
        obarray.set_symbol_value("mark-ring-max", Value::Int(16));
        // saved-region-selection is set by keyboard::pure::register_bootstrap_vars
        obarray.set_symbol_value("transient-mark-mode", Value::Nil);
        obarray.set_symbol_value("transient-mark-mode-hook", Value::Nil);
        obarray.set_symbol_value("overriding-local-map", Value::Nil);
        obarray.set_symbol_value("overriding-local-map-menu-flag", Value::Nil);
        obarray.set_symbol_value("overriding-plist-environment", Value::Nil);
        obarray.set_symbol_value("overriding-terminal-local-map", Value::Nil);
        obarray.set_symbol_value("overriding-text-conversion-style", Value::symbol("lambda"));

        // ---- C-level bootstrap variables required by loadup.el files ----

        // Standard keymaps (C creates these in keyboard.c:init_kboard)
        obarray.set_symbol_value("global-map", global_map);
        obarray.set_symbol_value("esc-map", esc_map);
        obarray.set_symbol_value("ctl-x-map", ctl_x_map);
        obarray.set_symbol_value("special-event-map", special_event_map);
        obarray.set_symbol_value("help-map", help_map);
        obarray.set_symbol_value("mode-line-window-dedicated-keymap", mode_line_window_dedicated_keymap);
        obarray.set_symbol_value("indent-rigidly-map", indent_rigidly_map);
        obarray.set_symbol_value("text-mode-map", text_mode_map);
        obarray.set_symbol_value("image-slice-map", image_slice_map);
        obarray.set_symbol_value("tool-bar-map", tool_bar_map);
        obarray.set_symbol_value("key-translation-map", key_translation_map);
        obarray.set_symbol_value("function-key-map", function_key_map);
        obarray.set_symbol_value("input-decode-map", input_decode_map);
        obarray.set_symbol_value("local-function-key-map", local_function_key_map);

        // Core eval variables (stay in eval.rs)
        obarray.set_symbol_value("purify-flag", Value::Nil);
        obarray.set_symbol_value("max-lisp-eval-depth", Value::Int(1600));
        obarray.set_symbol_value("max-specpdl-size", Value::Int(1800));
        obarray.set_symbol_value("inhibit-load-charset-map", Value::Nil);

        // Terminal/display variables (C-level DEFVAR in official Emacs)
        obarray.set_symbol_value("tty-defined-color-alist", Value::Nil);
        obarray.set_symbol_value("standard-display-table", Value::Nil);
        obarray.set_symbol_value("image-load-path", Value::list(vec![
            Value::string("/usr/share/emacs/30.1/etc/images/"),
            Value::symbol("data-directory"),
        ]));
        obarray.set_symbol_value("image-scaling-factor", Value::Float(1.0, next_float_id()));

        // GC / memory management (C DEFVAR in official Emacs)
        obarray.set_symbol_value("gc-cons-threshold", Value::Int(800_000));
        obarray.set_symbol_value("gc-cons-percentage", Value::Float(0.1, next_float_id()));
        obarray.set_symbol_value("garbage-collection-messages", Value::Nil);

        // User init / startup (C DEFVAR in official Emacs)
        obarray.set_symbol_value("user-init-file", Value::Nil);
        obarray.set_symbol_value("user-emacs-directory", Value::string("~/.emacs.d/"));

        // Frame parameters (C DEFVAR in official Emacs)
        obarray.set_symbol_value("frame--special-parameters", Value::Nil);

        // Initialize distributed bootstrap variables
        super::load::register_bootstrap_vars(&mut obarray);
        super::fileio::register_bootstrap_vars(&mut obarray);
        super::window_cmds::register_bootstrap_vars(&mut obarray);
        super::keyboard::pure::register_bootstrap_vars(&mut obarray);
        super::composite::register_bootstrap_vars(&mut obarray);
        super::coding::register_bootstrap_vars(&mut obarray);
        super::xdisp::register_bootstrap_vars(&mut obarray);
        super::frame_vars::register_bootstrap_vars(&mut obarray);
        super::buffer_vars::register_bootstrap_vars(&mut obarray);

        // ---- end C-level bootstrap variables ----

        obarray.set_symbol_value("unread-input-method-events", Value::Nil);
        obarray.set_symbol_value("unread-post-input-method-events", Value::Nil);
        obarray.set_symbol_value("input-method-alist", Value::Nil);
        obarray.set_symbol_value("input-method-activate-hook", Value::Nil);
        obarray.set_symbol_value("input-method-after-insert-chunk-hook", Value::Nil);
        obarray.set_symbol_value("input-method-deactivate-hook", Value::Nil);
        obarray.set_symbol_value("input-method-exit-on-first-char", Value::Nil);
        obarray.set_symbol_value("input-method-exit-on-invalid-key", Value::Nil);
        obarray.set_symbol_value("input-method-function", Value::symbol("list"));
        obarray.set_symbol_value("input-method-highlight-flag", Value::True);
        obarray.set_symbol_value("input-method-history", Value::Nil);
        // input-method-previous-message is set by keyboard::pure::register_bootstrap_vars
        obarray.set_symbol_value("input-method-use-echo-area", Value::Nil);
        obarray.set_symbol_value("input-method-verbose-flag", Value::symbol("default"));
        obarray.set_symbol_value("unread-command-events", Value::Nil);
        // GNU Emacs seeds core startup vars with integer
        // `variable-documentation` offsets in the DOC table.
        for &(name, _) in STARTUP_VARIABLE_DOC_STUBS {
            obarray.put_property(name, "variable-documentation", Value::Int(0));
        }
        // Some startup docs are string-valued in GNU Emacs (not integer offsets).
        for &(name, doc) in STARTUP_VARIABLE_DOC_STRING_PROPERTIES {
            obarray.put_property(name, "variable-documentation", Value::string(doc));
        }

        // GNU Emacs exposes `x-display-color-p` as an alias to
        // `display-color-p` in startup state.
        obarray.set_symbol_function("x-display-color-p", Value::symbol("display-color-p"));
        obarray.set_symbol_function("x-color-defined-p", Value::symbol("color-defined-p"));
        obarray.set_symbol_function("x-color-values", Value::symbol("color-values"));
        obarray.set_symbol_function("x-defined-colors", Value::symbol("defined-colors"));
        obarray.set_symbol_function("x-get-selection", Value::symbol("gui-get-selection"));
        obarray.set_symbol_function(
            "x-get-selection-value",
            Value::symbol("gui-get-primary-selection"),
        );
        obarray.set_symbol_function("x-select-text", Value::symbol("gui-select-text"));
        obarray.set_symbol_function("x-selection-value", Value::symbol("gui-selection-value"));
        obarray.set_symbol_function("x-set-selection", Value::symbol("gui-set-selection"));
        // Window size aliases are also preseeded in startup state.
        obarray.set_symbol_function("window-height", Value::symbol("window-total-height"));
        obarray.set_symbol_function("window-width", Value::symbol("window-body-width"));
        obarray.set_symbol_function(
            "window-inside-pixel-edges",
            Value::symbol("window-body-pixel-edges"),
        );
        obarray.set_symbol_function("window-inside-edges", Value::symbol("window-body-edges"));
        // Additional startup aliases exposed as symbol indirections in GNU Emacs.
        obarray.set_symbol_function("count-matches", Value::symbol("how-many"));
        obarray.set_symbol_function("replace-rectangle", Value::symbol("string-rectangle"));
        obarray.set_symbol_function("wholenump", Value::symbol("natnump"));
        obarray.set_symbol_function(
            "subr-native-elisp-p",
            Value::symbol("native-comp-function-p"),
        );
        obarray.set_symbol_function(
            "kmacro-name-last-macro",
            Value::Subr(intern("kmacro-name-last-macro")),
        );
        obarray.set_symbol_function(
            "name-last-kbd-macro",
            Value::symbol("kmacro-name-last-macro"),
        );
        // GNU Emacs exposes this helper as a Lisp wrapper, not a primitive.
        obarray.set_symbol_function(
            "subr-primitive-p",
            Value::make_bytecode(Compiler::new(false).compile_lambda(
                &LambdaParams::simple(vec![intern("object")]),
                &[Expr::List(vec![
                    Expr::Symbol(intern("subrp")),
                    Expr::Symbol(intern("object")),
                ])],
            )),
        );
        // Bookmark command wrappers are startup autoloads in GNU Emacs.
        let mut seed_autoload = |name: &str, file: &str, doc: &str| {
            obarray.set_symbol_function(
                name,
                Value::list(vec![
                    Value::symbol("autoload"),
                    Value::string(file),
                    Value::string(doc),
                    Value::True,
                    Value::Nil,
                ]),
            );
        };
        seed_autoload(
            "bookmark-delete",
            "bookmark",
            "Delete BOOKMARK-NAME from the bookmark list.",
        );
        seed_autoload(
            "bookmark-jump",
            "bookmark",
            "Jump to bookmark BOOKMARK (a point in some file).",
        );
        seed_autoload(
            "bookmark-load",
            "bookmark",
            "Load bookmarks from FILE (which must be in bookmark format).",
        );
        seed_autoload(
            "bookmark-rename",
            "bookmark",
            "Change the name of OLD-NAME bookmark to NEW-NAME name.",
        );
        seed_autoload(
            "bookmark-save",
            "bookmark",
            "Save currently defined bookmarks in FILE.",
        );
        seed_autoload(
            "bookmark-set",
            "bookmark",
            "Set a bookmark named NAME at the current location.",
        );
        seed_autoload(
            "format-seconds",
            "time-date",
            "Use format control STRING to format the number SECONDS.",
        );
        seed_autoload(
            "format-spec",
            "format-spec",
            "Return a string based on FORMAT and SPECIFICATION.",
        );
        seed_autoload(
            "string-clean-whitespace",
            "subr-x",
            "Clean up whitespace in STRING.",
        );
        seed_autoload(
            "string-glyph-split",
            "subr-x",
            "Split STRING into a list of strings representing separate glyphs.",
        );
        seed_autoload(
            "upcase-char",
            "misc",
            "Uppercasify ARG chars starting from point.  Point doesn't move.",
        );
        seed_autoload(
            "bounds-of-thing-at-point",
            "thingatpt",
            "Determine the start and end buffer locations for the THING at point.",
        );
        seed_autoload("thing-at-point", "thingatpt", "Return the THING at point.");
        seed_autoload(
            "symbol-at-point",
            "thingatpt",
            "Return the symbol at point, or nil if none is found.",
        );
        seed_autoload(
            "safe-date-to-time",
            "time-date",
            "Parse a string DATE that represents a date-time and return a time value.",
        );
        seed_autoload(
            "read-passwd",
            "auth-source",
            "Read a password, prompting with PROMPT, and return it.",
        );
        seed_autoload("clear-rectangle", "rect", "Blank out the region-rectangle.");
        seed_autoload(
            "delete-extract-rectangle",
            "rect",
            "Delete the contents of the rectangle with corners at START and END.",
        );
        seed_autoload(
            "delete-rectangle",
            "rect",
            "Delete (don't save) text in the region-rectangle.",
        );
        seed_autoload(
            "describe-function",
            "help-fns",
            "Display the full documentation of FUNCTION (a symbol).",
        );
        seed_autoload(
            "describe-variable",
            "help-fns",
            "Display the full documentation of VARIABLE (a symbol).",
        );
        seed_autoload(
            "extract-rectangle",
            "rect",
            "Return the contents of the rectangle with corners at START and END.",
        );
        seed_autoload(
            "insert-kbd-macro",
            "macros",
            "Insert in buffer the definition of kbd macro MACRONAME, as Lisp code.",
        );
        seed_autoload(
            "insert-rectangle",
            "rect",
            "Insert text of RECTANGLE with upper left corner at point.",
        );
        seed_autoload(
            "kbd-macro-query",
            "macros",
            "Query user during kbd macro execution.",
        );
        seed_autoload(
            "kill-rectangle",
            "rect",
            "Delete the region-rectangle and save it as the last killed one.",
        );
        seed_autoload(
            "open-rectangle",
            "rect",
            "Blank out the region-rectangle, shifting text right.",
        );
        seed_autoload(
            "string-pixel-width",
            "subr-x",
            "Return the width of STRING in pixels.",
        );
        seed_autoload(
            "string-rectangle",
            "rect",
            "Replace rectangle contents with STRING on each line.",
        );
        seed_autoload(
            "yank-rectangle",
            "rect",
            "Yank the last killed rectangle with upper left corner at point.",
        );
        drop(seed_autoload);
        let mut seed_autoload_noninteractive = |name: &str, file: &str, doc: &str| {
            obarray.set_symbol_function(
                name,
                Value::list(vec![
                    Value::symbol("autoload"),
                    Value::string(file),
                    Value::string(doc),
                    Value::Nil,
                    Value::Nil,
                ]),
            );
        };
        // Some helper autoloads are non-interactive in GNU Emacs startup
        // function-cells; override their startup metadata accordingly.
        seed_autoload_noninteractive(
            "bounds-of-thing-at-point",
            "thingatpt",
            "Determine the start and end buffer locations for the THING at point.",
        );
        seed_autoload_noninteractive("thing-at-point", "thingatpt", "Return the THING at point.");
        seed_autoload_noninteractive(
            "symbol-at-point",
            "thingatpt",
            "Return the symbol at point, or nil if none is found.",
        );
        seed_autoload_noninteractive(
            "format-seconds",
            "time-date",
            "Use format control STRING to format the number SECONDS.",
        );
        seed_autoload_noninteractive(
            "format-spec",
            "format-spec",
            "Return a string based on FORMAT and SPECIFICATION.",
        );
        seed_autoload_noninteractive(
            "read-passwd",
            "auth-source",
            "Read a password, prompting with PROMPT, and return it.",
        );
        seed_autoload_noninteractive(
            "safe-date-to-time",
            "time-date",
            "Parse a string DATE that represents a date-time and return a time value.",
        );
        seed_autoload_noninteractive(
            "delete-extract-rectangle",
            "rect",
            "Delete the contents of the rectangle with corners at START and END.",
        );
        seed_autoload_noninteractive(
            "extract-rectangle",
            "rect",
            "Return the contents of the rectangle with corners at START and END.",
        );
        seed_autoload_noninteractive(
            "insert-rectangle",
            "rect",
            "Insert text of RECTANGLE with upper left corner at point.",
        );
        seed_autoload_noninteractive(
            "string-clean-whitespace",
            "subr-x",
            "Clean up whitespace in STRING.",
        );
        seed_autoload_noninteractive(
            "string-glyph-split",
            "subr-x",
            "Split STRING into a list of strings representing separate glyphs.",
        );
        seed_autoload_noninteractive(
            "string-pixel-width",
            "subr-x",
            "Return the width of STRING in pixels.",
        );
        // Keep these as non-interactive autoload wrappers to match GNU Emacs
        // `symbol-function` shape while preserving runtime callability through
        // builtin dispatch.
        drop(seed_autoload_noninteractive);
        obarray.set_symbol_function(
            "string-chop-newline",
            Value::list(vec![
                Value::symbol("autoload"),
                Value::string("subr-x"),
                Value::string("Remove the final newline (if any) from STRING."),
                Value::Nil,
                Value::Nil,
            ]),
        );
        obarray.set_symbol_function(
            "string-pad",
            Value::list(vec![
                Value::symbol("autoload"),
                Value::string("subr-x"),
                Value::string("Pad STRING to LENGTH using PADDING."),
                Value::Nil,
                Value::Nil,
            ]),
        );
        obarray.set_symbol_function(
            "string-fill",
            Value::list(vec![
                Value::symbol("autoload"),
                Value::string("subr-x"),
                Value::string(
                    "Try to word-wrap STRING so that it displays with lines no wider than WIDTH.",
                ),
                Value::Nil,
                Value::Nil,
            ]),
        );
        obarray.set_symbol_function(
            "string-limit",
            Value::list(vec![
                Value::symbol("autoload"),
                Value::string("subr-x"),
                Value::string(
                    "Return a substring of STRING that is (up to) LENGTH characters long.",
                ),
                Value::Nil,
                Value::Nil,
            ]),
        );
        // Some startup helpers are Lisp functions that delegate to primitives.
        // Seed lightweight bytecode wrappers so `symbol-function` shape matches GNU Emacs.
        let seed_function_wrapper = |obarray: &mut Obarray, name: &str| {
            let wrapper = format!("neovm--startup-subr-wrapper-{name}");
            obarray.set_symbol_function(&wrapper, Value::Subr(intern(name)));

            let params = LambdaParams {
                required: vec![],
                optional: vec![],
                rest: Some(intern("args")),
            };
            let body = vec![Expr::List(vec![
                Expr::Symbol(intern("apply")),
                Expr::List(vec![
                    Expr::Symbol(intern("quote")),
                    Expr::Symbol(intern(&wrapper)),
                ]),
                Expr::Symbol(intern("args")),
            ])];
            let bc = Compiler::new(false).compile_lambda(&params, &body);
            obarray.set_symbol_function(name, Value::make_bytecode(bc));
        };
        let seed_fixed_arity_wrapper =
            |obarray: &mut Obarray, name: &str, required: &[&str], optional: &[&str]| {
                let wrapper = format!("neovm--startup-subr-wrapper-{name}");
                obarray.set_symbol_function(&wrapper, Value::Subr(intern(name)));

                let params = LambdaParams {
                    required: required.iter().map(|s| intern(s)).collect(),
                    optional: optional.iter().map(|s| intern(s)).collect(),
                    rest: None,
                };

                let mut call = Vec::with_capacity(1 + required.len() + optional.len());
                call.push(Expr::Symbol(intern(&wrapper)));
                call.extend(required.iter().map(|s| Expr::Symbol(intern(s))));
                call.extend(optional.iter().map(|s| Expr::Symbol(intern(s))));

                let bc = Compiler::new(false).compile_lambda(&params, &[Expr::List(call)]);
                obarray.set_symbol_function(name, Value::make_bytecode(bc));
            };
        for name in [
            "autoloadp",
            "seq-count",
            "seq-concatenate",
            "seq-contains-p",
            "seq-drop",
            "seq-do",
            "seq-empty-p",
            "seq-every-p",
            "seq-into",
            "seq-length",
            "seq-mapn",
            "seq-max",
            "seq-min",
            "seq-position",
            "seq-reduce",
            "seq-reverse",
            "seq-some",
            "seq-sort",
            "seq-subseq",
            "seq-take",
            "seq-uniq",
            "looking-at-p",
            "string-match-p",
            "string-blank-p",
            "string-empty-p",
            "string-equal-ignore-case",
            "string-to-vector",
        ] {
            seed_function_wrapper(&mut obarray, name);
        }

        seed_fixed_arity_wrapper(&mut obarray, "string-join", &["strings"], &["separator"]);
        seed_fixed_arity_wrapper(&mut obarray, "string-to-list", &["string"], &[]);

        // Keep word-at-point unavailable at startup; symbol-at-point lazily
        // materializes it to mirror GNU Emacs thing-at-point bootstrap.
        obarray.fmakunbound("word-at-point");

        // Stub macros needed during bootstrap — these are normally defined in
        // gv.el which cannot load yet (NeoVM's pcase special form can't handle
        // gv.el's pcase patterns).  The stubs make (gv-define-expander NAME ...)
        // and (gv-define-setter NAME ...) expand to nil so cl-lib.el can load.
        // gv-define-simple-setter and gv-define-setter are also handled as
        // evaluator special forms (sf_gv_define_simple_setter / sf_gv_define_setter).
        let noop_macro = Value::make_macro(LambdaData {
            params: LambdaParams {
                required: Vec::new(),
                optional: Vec::new(),
                rest: Some(intern("_args")),
            },
            body: vec![].into(),   // empty body → nil
            env: None,
            docstring: None,
            doc_form: None,
        });
        for stub_name in &[
            "gv-define-expander",
            "gv-define-setter",
            "gv-define-simple-setter",
        ] {
            obarray.set_symbol_function(stub_name, noop_macro);
        }

        // cl-defgeneric and cl-defmethod stubs — these macros are normally
        // defined by cl-generic.el, which fails during bootstrap (needs cl
        // type system).  Stub them as no-ops so files like startup.el and
        // frame.el that use them can still load.
        for stub_name in &[
            "cl-defgeneric",
            "cl-defmethod",
        ] {
            obarray.set_symbol_function(stub_name, noop_macro);
        }

        // cl-check-type and cl-typep stubs — cl-preloaded.el uses
        // (cl-check-type ...) which macroexpands to (cl-typep val type).
        // cl-macs.el defines these via define-inline (stores inline body
        // in the variable cell), but cl-macs is only eval-when-compile'd.
        // Stub cl-check-type as a no-op macro and cl-typep as a function
        // returning t — skips type validation during bootstrap.
        obarray.set_symbol_function("cl-check-type", noop_macro);
        obarray.set_symbol_function(
            "cl-typep",
            Value::make_lambda(LambdaData {
                params: LambdaParams {
                    required: vec![intern("_val"), intern("_type")],
                    optional: Vec::new(),
                    rest: None,
                },
                body: vec![Expr::Symbol(intern("t"))].into(),
                env: None,
                docstring: None,
                doc_form: None,
            }),
        );
        obarray.set_symbol_value("cl-typep", Value::True);

        // Mark standard variables as special (dynamically bound)
        for name in &[
            "debug-on-error",
            "lexical-binding",
            "load-prefer-newer",
            "load-path",
            "load-history",
            "features",
            "default-directory",
            "load-file-name",
            "noninteractive",
            "inhibit-quit",
            "print-length",
            "print-level",
            "standard-output",
            "buffer-read-only",
            "unread-command-events",
        ] {
            obarray.make_special(name);
        }

        // Initialize the standard error hierarchy (error, user-error, etc.)
        super::errors::init_standard_errors(&mut obarray);

        // Initialize indentation variables (tab-width, indent-tabs-mode, etc.)
        super::indent::init_indent_vars(&mut obarray);

        let mut custom = CustomManager::new();
        custom.make_variable_buffer_local("buffer-read-only");

        let mut ev = Self {
            interner,
            heap,
            obarray,
            dynamic: Vec::new(),
            lexenv: Value::Nil,
            features: Vec::new(),
            require_stack: Vec::new(),
            loads_in_progress: Vec::new(),
            buffers: BufferManager::new(),
            match_data: None,
            processes: ProcessManager::new(),
            timers: TimerManager::new(),
            watchers: VariableWatcherList::new(),
            current_local_map: Value::Nil,
            registers: RegisterManager::new(),
            bookmarks: BookmarkManager::new(),
            abbrevs: AbbrevManager::new(),
            autoloads: AutoloadManager::new(),
            custom,
            kill_ring: KillRing::new(),
            rectangle: RectangleState::new(),
            interactive: InteractiveRegistry::new(),
            recent_input_events: Vec::new(),
            read_command_keys: Vec::new(),
            input_mode_interrupt: true,
            frames: FrameManager::new(),
            modes: ModeRegistry::new(),
            threads: ThreadManager::new(),
            category_manager: CategoryManager::new(),
            kmacro: KmacroManager::new(),
            coding_systems: CodingSystemManager::new(),
            face_table: FaceTable::new(),
            depth: 0,
            max_depth: 1600, // Matches GNU Emacs default (max-lisp-eval-depth)
            gc_pending: false,
            gc_count: 0,
            gc_stress: false,
            temp_roots: Vec::new(),
            catch_tags: Vec::new(),
            saved_lexenvs: Vec::new(),
            named_call_cache: None,
            pcase_macroexpand_temp_counter: 0,
            literal_cache: HashMap::new(),
            macro_expansion_cache: HashMap::new(),
            macro_cache_hits: 0,
            macro_cache_misses: 0,
            macro_expand_total_us: 0,
            macro_cache_disabled: false,
        };
        // The heap and interner are boxed so their addresses are stable across moves.
        // Re-point anyway to be explicit about thread-local state.
        set_current_interner(&mut ev.interner);
        set_current_heap(&mut ev.heap);
        ev
    }

    // -----------------------------------------------------------------------
    // Garbage collection
    // -----------------------------------------------------------------------

    /// Enumerate every live `Value` reference in the evaluator and all
    /// sub-managers.  This is the root set for mark-and-sweep collection.
    fn collect_roots(&self) -> Vec<Value> {
        let mut roots = Vec::new();

        // Direct Evaluator fields
        roots.extend(self.temp_roots.iter().cloned());
        roots.extend(self.catch_tags.iter().cloned());
        roots.extend(self.recent_input_events.iter().cloned());
        roots.extend(self.read_command_keys.iter().cloned());
        for scope in &self.dynamic {
            roots.extend(scope.values().cloned());
        }
        roots.push(self.lexenv);
        // Scan saved lexenvs (from apply_lambda's lexenv replacement)
        for saved_env in &self.saved_lexenvs {
            roots.push(*saved_env);
        }

        // Literal cache — cached quote_to_value results for pcase eq-memoization
        roots.extend(self.literal_cache.values().copied());

        // Macro expansion cache — root any OpaqueValue nodes in cached Expr trees
        for (expr, _fingerprint) in self.macro_expansion_cache.values() {
            expr.collect_opaque_values(&mut roots);
        }

        // Named call cache — holds a Value when target is Obarray(val)
        if let Some(cache) = &self.named_call_cache {
            if let NamedCallTarget::Obarray(val) = &cache.target {
                roots.push(*val);
            }
        }

        // Thread-local statics holding Values
        collect_thread_local_gc_roots(&mut roots);

        // current_local_map is a cons-list keymap Value, trace it as a root
        if !self.current_local_map.is_nil() {
            roots.push(self.current_local_map);
        }

        // Sub-managers
        self.obarray.trace_roots(&mut roots);
        self.processes.trace_roots(&mut roots);
        self.timers.trace_roots(&mut roots);
        self.watchers.trace_roots(&mut roots);
        self.registers.trace_roots(&mut roots);
        self.custom.trace_roots(&mut roots);
        self.autoloads.trace_roots(&mut roots);
        self.buffers.trace_roots(&mut roots);
        self.threads.trace_roots(&mut roots);
        self.kmacro.trace_roots(&mut roots);
        self.modes.trace_roots(&mut roots);
        self.frames.trace_roots(&mut roots);

        roots
    }

    /// Get the current GC threshold.
    pub fn gc_threshold(&self) -> usize {
        self.heap.gc_threshold()
    }

    /// Set the GC threshold. Use usize::MAX to effectively disable GC.
    pub fn set_gc_threshold(&mut self, threshold: usize) {
        self.heap.set_gc_threshold(threshold);
    }

    /// Set the maximum eval recursion depth.
    pub fn set_max_depth(&mut self, depth: usize) {
        self.max_depth = depth;
    }

    /// Set the thread-local interner and heap pointers for the current thread.
    ///
    /// Must be called when using an Evaluator from a thread other than the one
    /// that created it (e.g., in worker thread pools).
    pub fn setup_thread_locals(&mut self) {
        set_current_interner(&mut self.interner);
        set_current_heap(&mut self.heap);
    }

    /// Perform a full mark-and-sweep garbage collection.
    #[tracing::instrument(level = "debug", skip(self))]
    pub fn gc_collect(&mut self) {
        let roots = self.collect_roots();
        self.heap.collect(roots.into_iter());
        self.gc_pending = false;
        self.gc_count += 1;
    }

    /// Number of gray objects to process per incremental marking step.
    const MARK_WORK_LIMIT: usize = 1024;

    /// Incremental GC safe point.
    ///
    /// In gc_stress mode, always does a full collection for maximum bug
    /// detection.  Otherwise, drives an incremental mark-sweep state machine:
    ///
    ///   Idle → (threshold?) → begin_marking → Marking
    ///   Marking → mark_some(LIMIT) → (done?) → sweep → Idle
    pub fn gc_safe_point(&mut self) {
        // Stress mode: full collection at every safe point.
        if self.gc_stress {
            if self.gc_pending || self.heap.should_collect() || self.gc_stress {
                self.gc_collect();
            }
            return;
        }

        if self.heap.is_marking() {
            // Continue incremental marking
            let done = self.heap.mark_some(Self::MARK_WORK_LIMIT);
            if done {
                // Re-scan roots before sweeping: mutations to obarray,
                // dynamic stack, or temp_roots during the marking phase
                // may have introduced new live references.
                let roots = self.collect_roots();
                self.heap.rescan_roots(roots.into_iter());
                self.heap.finish_collection();
                self.gc_count += 1;
            }
        } else if self.gc_pending || self.heap.should_collect() {
            // Start a new incremental collection cycle
            let roots = self.collect_roots();
            self.heap.begin_marking(roots.into_iter());
            self.gc_pending = false;
            // Do first batch of marking work immediately
            let done = self.heap.mark_some(Self::MARK_WORK_LIMIT);
            if done {
                self.heap.finish_collection();
                self.gc_count += 1;
            }
        }
    }

    /// Save the current length of temp_roots for later restoration.
    pub(crate) fn save_temp_roots(&self) -> usize {
        self.temp_roots.len()
    }

    /// Add a value to temp_roots so it survives GC.
    pub(crate) fn push_temp_root(&mut self, val: Value) {
        self.temp_roots.push(val);
    }

    /// Restore temp_roots to a previously saved length.
    pub(crate) fn restore_temp_roots(&mut self, saved_len: usize) {
        self.temp_roots.truncate(saved_len);
    }

    /// Whether lexical-binding is currently enabled.
    pub fn lexical_binding(&self) -> bool {
        self.obarray
            .symbol_value("lexical-binding")
            .is_some_and(|v| v.is_truthy())
    }

    pub(crate) fn record_input_event(&mut self, event: Value) {
        self.assign("last-input-event", event);
        self.recent_input_events.push(event);
        if self.recent_input_events.len() > RECENT_INPUT_EVENT_LIMIT {
            self.recent_input_events.remove(0);
        }
    }

    pub(crate) fn record_nonmenu_input_event(&mut self, event: Value) {
        self.assign("last-nonmenu-event", event);
    }

    pub(crate) fn next_pcase_macroexpand_temp_symbol(&mut self) -> Value {
        let n = self.pcase_macroexpand_temp_counter;
        self.pcase_macroexpand_temp_counter = self.pcase_macroexpand_temp_counter.saturating_add(1);
        Value::symbol(format!("x{n}"))
    }

    pub(crate) fn recent_input_events(&self) -> &[Value] {
        &self.recent_input_events
    }

    pub(crate) fn clear_recent_input_events(&mut self) {
        self.recent_input_events.clear();
    }

    pub(crate) fn set_read_command_keys(&mut self, keys: Vec<Value>) {
        self.read_command_keys = keys;
    }

    pub(crate) fn clear_read_command_keys(&mut self) {
        self.read_command_keys.clear();
    }

    pub(crate) fn read_command_keys(&self) -> &[Value] {
        &self.read_command_keys
    }

    pub(crate) fn current_input_mode_tuple(&self) -> (bool, bool, bool, i64) {
        // Batch oracle compatibility: flow-control and meta are fixed to
        // nil/t respectively, and quit char is fixed to C-g (7).
        (self.input_mode_interrupt, false, true, 7)
    }

    pub(crate) fn set_input_mode_interrupt(&mut self, interrupt: bool) {
        self.input_mode_interrupt = interrupt;
    }

    pub(crate) fn pop_unread_command_event(&mut self) -> Option<Value> {
        let current = match self.eval_symbol("unread-command-events") {
            Ok(value) => value,
            Err(_) => Value::Nil,
        };
        match current {
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                let head = pair.car;
                let tail = pair.cdr;
                drop(pair);
                self.assign("unread-command-events", tail);
                self.record_input_event(head);
                Some(head)
            }
            _ => None,
        }
    }

    pub(crate) fn peek_unread_command_event(&self) -> Option<Value> {
        let current = match self.eval_symbol("unread-command-events") {
            Ok(value) => value,
            Err(_) => Value::Nil,
        };
        match current {
            Value::Cons(cell) => {
                let pair = read_cons(cell);
                Some(pair.car)
            }
            _ => None,
        }
    }

    /// Enable or disable lexical binding.
    pub fn set_lexical_binding(&mut self, enabled: bool) {
        self.obarray
            .set_symbol_value("lexical-binding", Value::bool(enabled));
    }

    /// Load a file, converting EvalError back to Flow for use in special forms.
    pub(crate) fn load_file_internal(&mut self, path: &std::path::Path) -> EvalResult {
        super::load::load_file(self, path).map_err(|e| match e {
            EvalError::Signal { symbol, data } => signal(resolve_sym(symbol), data),
            EvalError::UncaughtThrow { tag, value } => signal("no-catch", vec![tag, value]),
        })
    }

    /// Keep the Lisp-visible `features` variable in sync with the evaluator's
    /// internal feature set.
    fn sync_features_variable(&mut self) {
        let values: Vec<Value> = self
            .features
            .iter()
            .map(|id| Value::Symbol(*id))
            .collect();
        self.obarray
            .set_symbol_value("features", Value::list(values));
    }

    fn refresh_features_from_variable(&mut self) {
        let current = self
            .obarray
            .symbol_value("features")
            .cloned()
            .unwrap_or(Value::Nil);
        let mut parsed = Vec::new();
        if let Some(items) = list_to_vec(&current) {
            for item in items {
                if let Value::Symbol(id) = item {
                    parsed.push(id);
                }
            }
        }
        self.features = parsed;
    }

    fn has_feature(&mut self, name: &str) -> bool {
        self.refresh_features_from_variable();
        let id = intern(name);
        self.features.iter().any(|f| *f == id)
    }

    pub(crate) fn add_feature(&mut self, name: &str) {
        self.refresh_features_from_variable();
        let id = intern(name);
        if self.features.iter().any(|f| *f == id) {
            return;
        }
        // Emacs pushes newly-provided features at the front.
        self.features.insert(0, id);
        self.sync_features_variable();
    }

    pub(crate) fn feature_present(&mut self, name: &str) -> bool {
        self.has_feature(name)
    }

    /// Remove a feature (used to undo temporary provides during bootstrap).
    pub(crate) fn remove_feature(&mut self, name: &str) {
        self.refresh_features_from_variable();
        let id = intern(name);
        self.features.retain(|f| *f != id);
        self.sync_features_variable();
    }

    /// Access the obarray (for builtins that need it).
    pub fn obarray(&self) -> &Obarray {
        &self.obarray
    }

    /// Access the obarray mutably.
    pub fn obarray_mut(&mut self) -> &mut Obarray {
        &mut self.obarray
    }

    /// Public read access to the buffer manager.
    pub fn buffer_manager(&self) -> &BufferManager {
        &self.buffers
    }

    /// Public mutable access to the buffer manager.
    pub fn buffer_manager_mut(&mut self) -> &mut BufferManager {
        &mut self.buffers
    }

    /// Public read access to the frame manager.
    pub fn frame_manager(&self) -> &FrameManager {
        &self.frames
    }

    /// Public mutable access to the frame manager.
    pub fn frame_manager_mut(&mut self) -> &mut FrameManager {
        &mut self.frames
    }

    /// Public read access to the kill ring.
    pub fn kill_ring(&self) -> &KillRing {
        &self.kill_ring
    }

    /// Public mutable access to the kill ring.
    pub fn kill_ring_mut(&mut self) -> &mut KillRing {
        &mut self.kill_ring
    }

    /// Public read access to the face table.
    pub fn face_table(&self) -> &FaceTable {
        &self.face_table
    }

    /// Public mutable access to the face table.
    pub fn face_table_mut(&mut self) -> &mut FaceTable {
        &mut self.face_table
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    pub fn eval_expr(&mut self, expr: &Expr) -> Result<Value, EvalError> {
        set_current_heap(&mut self.heap);
        self.eval(expr).map_err(map_flow)
    }

    /// Evaluate a Value as code (like Elisp's `eval`).
    /// Converts Value to Expr, roots any embedded OpaqueValues (closures,
    /// bytecode, etc.) so they survive GC, then evaluates.
    pub(crate) fn eval_value(&mut self, value: &Value) -> EvalResult {
        let expr = value_to_expr(value);
        let saved = self.save_temp_roots();
        let mut opaques = Vec::new();
        collect_opaque_values(&expr, &mut opaques);
        for v in &opaques {
            self.push_temp_root(*v);
        }
        let result = self.eval(&expr);
        self.restore_temp_roots(saved);
        result
    }

    pub fn eval_forms(&mut self, forms: &[Expr]) -> Vec<Result<Value, EvalError>> {
        set_current_heap(&mut self.heap);
        let saved_len = self.temp_roots.len();
        let mut results = Vec::with_capacity(forms.len());
        for form in forms {
            let result = self.eval_expr(form);
            // Root successful values so they survive GC triggered by later forms.
            if let Ok(ref val) = result {
                self.temp_roots.push(*val);
            }
            results.push(result);
        }
        self.temp_roots.truncate(saved_len);
        results
    }

    /// Set a global variable.
    pub fn set_variable(&mut self, name: &str, value: Value) {
        self.obarray.set_symbol_value(name, value);
    }

    /// Set a function binding.
    pub fn set_function(&mut self, name: &str, value: Value) {
        self.obarray.set_symbol_function(name, value);
    }

    // -----------------------------------------------------------------------
    // Core eval
    // -----------------------------------------------------------------------

    pub(crate) fn eval(&mut self, expr: &Expr) -> EvalResult {
        self.depth += 1;
        // Sync max_depth from max-lisp-eval-depth variable only when we're
        // near the limit (avoids obarray lookup on every eval call).
        if self.depth > self.max_depth {
            if let Some(Value::Int(n)) = self.obarray.symbol_value("max-lisp-eval-depth") {
                let new_max = (*n).max(100) as usize;
                if new_max != self.max_depth {
                    self.max_depth = new_max;
                }
            }
        }
        if self.depth > self.max_depth {
            self.depth -= 1;
            return Err(signal(
                "excessive-lisp-nesting",
                vec![Value::Int(self.max_depth as i64)],
            ));
        }
        // Use stacker to dynamically grow the call stack when nearing
        // exhaustion.  The red-zone (256 KB) must be larger than the
        // combined stack frames between successive eval() calls (through
        // eval_list → apply → apply_lambda → bytecode VM).  When the
        // remaining stack falls below the red-zone a new 2 MB segment is
        // allocated on the heap.
        let result = stacker::maybe_grow(256 * 1024, 2 * 1024 * 1024, || {
            self.eval_inner(expr)
        });
        self.depth -= 1;
        result
    }

    fn eval_inner(&mut self, expr: &Expr) -> EvalResult {
        match expr {
            Expr::Int(v) => Ok(Value::Int(*v)),
            Expr::Float(v) => Ok(Value::Float(*v, next_float_id())),
            Expr::Str(s) => Ok(Value::string(s.clone())),
            Expr::Char(c) => Ok(Value::Char(*c)),
            Expr::Keyword(id) => Ok(Value::Keyword(*id)),
            Expr::Bool(true) => Ok(Value::True),
            Expr::Bool(false) => Ok(Value::Nil),
            Expr::Vector(items) => {
                // Emacs vector literals are self-evaluating constants; elements
                // are not evaluated in the current lexical/dynamic environment.
                let vals = items.iter().map(quote_to_value).collect();
                Ok(Value::vector(vals))
            }
            Expr::Symbol(id) => self.eval_symbol_by_id(*id),
            Expr::List(items) => self.eval_list(items),
            Expr::DottedList(items, last) => {
                // Evaluate as a list call, ignoring dotted cdr
                // (This is for `(func a b . rest)` style, which in practice
                //  means the dotted pair is rarely used in function calls)
                let _ = last;
                self.eval_list(items)
            }
            Expr::OpaqueValue(v) => Ok(*v),
        }
    }

    /// Look up a symbol by its SymId. Uses the SymId directly for lexenv
    /// lookup (preserving uninterned symbol identity, like Emacs's EQ-based
    /// Fassq on Vinternal_interpreter_environment).
    fn eval_symbol_by_id(&self, sym_id: SymId) -> EvalResult {
        let symbol = resolve_sym(sym_id);
        if symbol == "nil" {
            return Ok(Value::Nil);
        }
        if symbol == "t" {
            return Ok(Value::True);
        }
        // Keywords evaluate to themselves
        if symbol.starts_with(':') {
            return Ok(Value::Keyword(intern(symbol)));
        }

        let resolved = super::builtins::resolve_variable_alias_name(self, symbol)?;

        // If lexical binding is on and symbol is NOT special, check lexenv first.
        // Use the original sym_id for lookup — this preserves uninterned symbol
        // identity (an uninterned #:body won't match the interned `body`).
        if self.lexical_binding() && !self.obarray.is_special(symbol) {
            if let Some(value) = lexenv_lookup(self.lexenv, sym_id) {
                return Ok(value);
            }
            if resolved != symbol && !self.obarray.is_special(&resolved) {
                let resolved_id = intern(&resolved);
                if let Some(value) = lexenv_lookup(self.lexenv, resolved_id) {
                    return Ok(value);
                }
            }
        }

        // Dynamic scope lookup (inner to outer)
        for frame in self.dynamic.iter().rev() {
            if let Some(value) = frame.get(&sym_id) {
                return Ok(*value);
            }
            if resolved != symbol {
                let resolved_id = intern(&resolved);
                if let Some(value) = frame.get(&resolved_id) {
                    return Ok(*value);
                }
            }
        }

        if resolved == "nil" {
            return Ok(Value::Nil);
        }
        if resolved == "t" {
            return Ok(Value::True);
        }
        if resolved.starts_with(':') {
            return Ok(Value::Keyword(intern(&resolved)));
        }

        // Buffer-local binding on current buffer.
        if let Some(buf) = self.buffers.current_buffer() {
            if let Some(value) = buf.get_buffer_local(&resolved) {
                return Ok(*value);
            }
        }

        // Obarray value cell
        if let Some(value) = self.obarray.symbol_value(&resolved) {
            return Ok(*value);
        }

        Err(signal("void-variable", vec![Value::symbol(symbol)]))
    }

    fn eval_symbol(&self, symbol: &str) -> EvalResult {
        self.eval_symbol_by_id(intern(symbol))
    }

    /// Evaluate a slice of expressions into a Vec, rooting intermediate results
    /// in `temp_roots` so they survive any GC triggered by later evaluations.
    ///
    /// Returns `(args, saved_len)`.  The evaluated args remain rooted in
    /// `temp_roots` so that subsequent `apply` / `apply_named_callable`
    /// calls can't have their args freed by GC.  The caller **must** call
    /// `self.restore_temp_roots(saved_len)` once the args are no longer
    /// needed (typically after `apply` returns).
    fn eval_args(&mut self, exprs: &[Expr]) -> Result<(Vec<Value>, usize), Flow> {
        let saved_len = self.temp_roots.len();
        let mut args = Vec::with_capacity(exprs.len());
        for expr in exprs.iter() {
            match self.eval(expr) {
                Ok(val) => {
                    self.temp_roots.push(val);
                    args.push(val);
                }
                Err(Flow::Signal(sig))
                    if sig.symbol_name() == "wrong-type-argument"
                        && matches!(
                            expr,
                            Expr::List(items)
                                if matches!(
                                    items.first(),
                                    Some(Expr::Symbol(id))
                                        if resolve_sym(*id) == "lambda" || resolve_sym(*id) == "closure"
                                )
                        ) =>
                {
                    self.temp_roots.truncate(saved_len);
                    return Err(signal(
                        "invalid-function",
                        vec![quote_to_value(expr)],
                    ));
                }
                Err(e) => {
                    self.temp_roots.truncate(saved_len);
                    return Err(e);
                }
            }
        }
        // Do NOT truncate — caller restores after apply.
        Ok((args, saved_len))
    }

    fn eval_list(&mut self, items: &[Expr]) -> EvalResult {
        let Some((head, tail)) = items.split_first() else {
            return Ok(Value::Nil);
        };

        if let Expr::Symbol(id) = head {
            let name = resolve_sym(*id);

            // When an Elisp file installs a macro for a name that NeoVM
            // handles as a special form (e.g. pcase.el defines
            // `(defmacro pcase ...)`), the macro would shadow our Rust
            // special form.  Intercept these cases and route to the
            // special form handler instead.
            if super::subr_info::is_evaluator_sf_skip_macroexpand(name) {
                if let Some(func) = self.obarray.symbol_function(name) {
                    let is_macro = matches!(func, Value::Macro(_))
                        || (func.is_cons() && func.cons_car().is_symbol_named("macro"));
                    if is_macro {
                        if let Some(result) = self.try_special_form(name, tail) {
                            return result;
                        }
                    }
                }
            }

            // Check for macro expansion (from obarray function cell)
            if let Some(func) = self.obarray.symbol_function(name).cloned() {
                if func.is_nil() {
                    return Err(signal("void-function", vec![Value::symbol(name)]));
                }
                if let Value::Macro(_) = &func {
                    let expanded = self.expand_macro(func, tail)?;
                    // Root OpaqueValues (closures, bytecode, etc.) embedded
                    // in the expansion so they survive GC during eval.
                    let saved_opaque = self.save_temp_roots();
                    let mut opaques = Vec::new();
                    collect_opaque_values(&expanded, &mut opaques);
                    for v in &opaques {
                        self.push_temp_root(*v);
                    }
                    let result = self.eval(&expanded);
                    self.restore_temp_roots(saved_opaque);
                    return result;
                }
                // Handle cons-cell macros: (macro . fn) — used by byte-run.el's
                // (defalias 'defmacro (cons 'macro #'(lambda ...)))
                if let Value::Cons(cons_id) = func {
                    let car = func.cons_car();
                    if car.is_symbol_named("macro") {
                        let cache_key = (cons_id, tail.as_ptr() as usize);
                        let current_fp = tail_fingerprint(tail);
                        if !self.macro_cache_disabled {
                            if let Some((cached, stored_fp)) = self.macro_expansion_cache.get(&cache_key) {
                                if *stored_fp == current_fp {
                                    self.macro_cache_hits += 1;
                                    let expanded = cached.clone();
                                    let saved_opaque = self.save_temp_roots();
                                    let mut opaques = Vec::new();
                                    collect_opaque_values(&expanded, &mut opaques);
                                    for v in &opaques {
                                        self.push_temp_root(*v);
                                    }
                                    let result = self.eval(&expanded);
                                    self.restore_temp_roots(saved_opaque);
                                    return result;
                                }
                                // Fingerprint mismatch → ABA detected, fall through to re-expand
                            }
                        }

                        let expand_start = std::time::Instant::now();
                        let saved = self.save_temp_roots();
                        let macro_fn = func.cons_cdr();
                        self.push_temp_root(macro_fn);
                        // Root all arg values during macro expansion to survive GC.
                        let arg_values: Vec<Value> = tail.iter().map(quote_to_value).collect();
                        for v in &arg_values {
                            self.push_temp_root(*v);
                        }
                        let expanded_value = self.apply(macro_fn, arg_values)?;
                        // Root expansion result during value_to_expr traversal
                        // AND during eval of expanded_expr (OpaqueValues reference
                        // heap objects reachable only through expanded_value).
                        self.push_temp_root(expanded_value);
                        let expanded_expr = value_to_expr(&expanded_value);
                        self.restore_temp_roots(saved);

                        // Cache the expansion as Rc<Expr>.  The Rc keeps the
                        // expansion alive in the cache, ensuring inner Vec
                        // addresses remain stable for future cache key lookups.
                        let expand_elapsed = expand_start.elapsed();
                        self.macro_cache_misses += 1;
                        self.macro_expand_total_us += expand_elapsed.as_micros() as u64;

                        let expanded_rc = Rc::new(expanded_expr);
                        if !self.macro_cache_disabled {
                            self.macro_expansion_cache
                                .insert(cache_key, (expanded_rc.clone(), current_fp));
                        }

                        let saved_opaque = self.save_temp_roots();
                        let mut opaques = Vec::new();
                        collect_opaque_values(&expanded_rc, &mut opaques);
                        for v in &opaques {
                            self.push_temp_root(*v);
                        }
                        let result = self.eval(&expanded_rc);
                        self.restore_temp_roots(saved_opaque);
                        return result;
                    }
                }

                if let Value::Subr(bound_name) = &func {
                    if resolve_sym(*bound_name) == name && super::subr_info::is_special_form(name) {
                        if let Some(result) = self.try_special_form(name, tail) {
                            return result;
                        }
                    }
                }

                // Explicit function-cell bindings override special-form fallback.
                let (args, args_saved) = self.eval_args(tail)?;
                if super::autoload::is_autoload_value(&func) {
                    let writeback_args = args.clone();
                    let result =
                        self.apply_named_callable(name, args, Value::Subr(intern(name)), false);
                    self.restore_temp_roots(args_saved);
                    if let Ok(value) = &result {
                        self.maybe_writeback_mutating_first_arg(name, None, &writeback_args, value);
                    }
                    return result;
                }
                let function_is_callable = match &func {
                    Value::Lambda(_) | Value::ByteCode(_) | Value::Macro(_) => true,
                    Value::Subr(bound_name) => !super::subr_info::is_special_form(resolve_sym(*bound_name)),
                    _ => false,
                };
                let alias_target = match &func {
                    Value::Symbol(target) => Some(resolve_sym(*target).to_owned()),
                    Value::Subr(bound_name) => Some(resolve_sym(*bound_name).to_owned()),
                    _ => None,
                };
                let writeback_args = args.clone();
                let result = match self.apply(func, args) {
                    Err(Flow::Signal(sig))
                        if sig.symbol_name() == "invalid-function" && !function_is_callable =>
                    {
                        if matches!(func, Value::Symbol(_)) {
                            Err(Flow::Signal(sig))
                        } else {
                            Err(signal("invalid-function", vec![Value::symbol(name)]))
                        }
                    }
                    other => other,
                };
                self.restore_temp_roots(args_saved);
                if let Ok(value) = &result {
                    self.maybe_writeback_mutating_first_arg(
                        name,
                        alias_target.as_deref(),
                        &writeback_args,
                        value,
                    );
                }
                return if let Some(target) = alias_target {
                    result.map_err(|flow| {
                        rewrite_wrong_arity_alias_function_object(flow, name, &target)
                    })
                } else {
                    result
                };
            }

            // Special forms
            if name == "`" || !self.obarray.is_function_unbound(name) {
                if let Some(result) = self.try_special_form(name, tail) {
                    return result;
                }
            }

            // Regular function call — evaluate args then dispatch
            let (args, args_saved) = self.eval_args(tail)?;

            let writeback_args = args.clone();
            let result = self.apply_named_callable(name, args, Value::Subr(intern(name)), false);
            self.restore_temp_roots(args_saved);
            if let Ok(value) = &result {
                self.maybe_writeback_mutating_first_arg(name, None, &writeback_args, value);
            }
            return result;
        }

        // Head is a list (possibly a lambda expression)
        if let Expr::List(lambda_form) = head {
            if let Some(Expr::Symbol(id)) = lambda_form.first() {
                if resolve_sym(*id) == "lambda" {
                    let func = self.eval_lambda(&lambda_form[1..])?;
                    let (args, args_saved) = self.eval_args(tail)?;
                    let result = self.apply(func, args);
                    self.restore_temp_roots(args_saved);
                    return result;
                }
            }
        }

        // Head is an opaque callable value (Lambda, ByteCode, Subr, etc.)
        // embedded in code via value_to_expr (e.g., from eval/macro expansion).
        if let Expr::OpaqueValue(func) = head {
            let (args, args_saved) = self.eval_args(tail)?;
            let result = self.apply(*func, args);
            self.restore_temp_roots(args_saved);
            return result;
        }

        Err(signal("invalid-function", vec![quote_to_value(head)]))
    }

    fn maybe_writeback_mutating_first_arg(
        &mut self,
        called_name: &str,
        alias_target: Option<&str>,
        call_args: &[Value],
        result: &Value,
    ) {
        let mutates_fillarray =
            called_name == "fillarray" || alias_target.is_some_and(|name| name == "fillarray");
        let mutates_aset = called_name == "aset" || alias_target.is_some_and(|name| name == "aset");
        if !mutates_fillarray && !mutates_aset {
            return;
        }
        let Some(first_arg) = call_args.first() else {
            return;
        };
        if !first_arg.is_string() {
            return;
        }

        let replacement = if mutates_fillarray {
            if !result.is_string() || eq_value(first_arg, result) {
                return;
            }
            *result
        } else {
            if call_args.len() < 3 {
                return;
            }
            let Ok(updated) =
                super::builtins::aset_string_replacement(first_arg, &call_args[1], &call_args[2])
            else {
                return;
            };
            if eq_value(first_arg, &updated) {
                return;
            }
            updated
        };

        if first_arg.as_str() == replacement.as_str() {
            return;
        }

        let mut visited = HashSet::new();
        // Walk the lexenv cons alist and replace alias refs in binding values
        {
            let mut lexenv_val = self.lexenv;
            Self::replace_alias_refs_in_value(&mut lexenv_val, first_arg, &replacement, &mut visited);
            self.lexenv = lexenv_val;
        }
        for frame in &mut self.dynamic {
            for value in frame.values_mut() {
                Self::replace_alias_refs_in_value(value, first_arg, &replacement, &mut visited);
            }
        }
        if let Some(buf) = self.buffers.current_buffer_mut() {
            for value in buf.properties.values_mut() {
                Self::replace_alias_refs_in_value(value, first_arg, &replacement, &mut visited);
            }
        }

        let symbols: Vec<String> = self
            .obarray
            .all_symbols()
            .into_iter()
            .map(str::to_string)
            .collect();
        for name in symbols {
            if let Some(symbol) = self.obarray.get_mut(&name) {
                if let Some(value) = symbol.value.as_mut() {
                    Self::replace_alias_refs_in_value(value, first_arg, &replacement, &mut visited);
                }
            }
        }
    }

    fn replace_alias_refs_in_value(
        value: &mut Value,
        from: &Value,
        to: &Value,
        visited: &mut HashSet<usize>,
    ) {
        if eq_value(value, from) {
            *value = *to;
            return;
        }

        match value {
            Value::Cons(cell) => {
                let key = (cell.index as usize) ^ 0x1;
                if !visited.insert(key) {
                    return;
                }
                let pair = read_cons(*cell);
                let mut new_car = pair.car;
                let mut new_cdr = pair.cdr;
                Self::replace_alias_refs_in_value(&mut new_car, from, to, visited);
                Self::replace_alias_refs_in_value(&mut new_cdr, from, to, visited);
                with_heap_mut(|h| {
                    h.set_car(*cell, new_car);
                    h.set_cdr(*cell, new_cdr);
                });
            }
            Value::Vector(items) | Value::Record(items) => {
                let key = (items.index as usize) ^ 0x2;
                if !visited.insert(key) {
                    return;
                }
                let mut values = with_heap(|h| h.get_vector(*items).clone());
                for item in values.iter_mut() {
                    Self::replace_alias_refs_in_value(item, from, to, visited);
                }
                with_heap_mut(|h| *h.get_vector_mut(*items) = values);
            }
            Value::HashTable(table) => {
                let key = (table.index as usize) ^ 0x4;
                if !visited.insert(key) {
                    return;
                }
                let mut ht = with_heap(|h| h.get_hash_table(*table).clone());
                let old_ptr = match from {
                    Value::Str(id) => Some(id.index as usize),
                    _ => None,
                };
                let new_ptr = match to {
                    Value::Str(id) => Some(id.index as usize),
                    _ => None,
                };
                if matches!(ht.test, HashTableTest::Eq | HashTableTest::Eql) {
                    if let (Some(old_ptr), Some(new_ptr)) = (old_ptr, new_ptr) {
                        if let Some(existing) = ht.data.remove(&HashKey::Ptr(old_ptr)) {
                            ht.data.insert(HashKey::Ptr(new_ptr), existing);
                        }
                        if ht.key_snapshots.remove(&HashKey::Ptr(old_ptr)).is_some() {
                            ht.key_snapshots.insert(HashKey::Ptr(new_ptr), *to);
                        }
                    }
                }
                for item in ht.data.values_mut() {
                    Self::replace_alias_refs_in_value(item, from, to, visited);
                }
                with_heap_mut(|h| *h.get_hash_table_mut(*table) = ht);
            }
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Special forms
    // -----------------------------------------------------------------------

    fn try_special_form(&mut self, name: &str, tail: &[Expr]) -> Option<EvalResult> {
        Some(match name {
            "quote" => self.sf_quote(tail),
            "`" => self.sf_backquote(tail),
            "function" => self.sf_function(tail),
            "let" => self.sf_let(tail),
            "let*" => self.sf_let_star(tail),
            "setq" => self.sf_setq(tail),
            "setq-local" => self.sf_setq_local(tail),
            "if" => self.sf_if(tail),
            "and" => self.sf_and(tail),
            "or" => self.sf_or(tail),
            "cond" => self.sf_cond(tail),
            "while" => self.sf_while(tail),
            "progn" => self.sf_progn(tail),
            "prog1" => self.sf_prog1(tail),
            "lambda" => self.eval_lambda(tail),
            "defun" => self.sf_defun(tail),
            "defvar" => self.sf_defvar(tail),
            "defconst" => self.sf_defconst(tail),
            "defmacro" => self.sf_defmacro(tail),
            "funcall" => self.sf_funcall(tail),
            "catch" => self.sf_catch(tail),
            "throw" => self.sf_throw(tail),
            "unwind-protect" => self.sf_unwind_protect(tail),
            "condition-case" => self.sf_condition_case(tail),
            "byte-code-literal" => self.sf_byte_code_literal(tail),
            "byte-code" => self.sf_byte_code(tail),
            "interactive" => Ok(Value::Nil), // Stub: ignored for now
            "declare" => Ok(Value::Nil),     // Stub: ignored for now
            "when" => self.sf_when(tail),
            "unless" => self.sf_unless(tail),
            "bound-and-true-p" => self.sf_bound_and_true_p(tail),
            "defalias" => self.sf_defalias(tail),
            "provide" => self.sf_provide(tail),
            "require" => self.sf_require(tail),
            "save-excursion" => self.sf_save_excursion(tail),
            "save-window-excursion" => self.sf_save_window_excursion(tail),
            "save-selected-window" => self.sf_save_selected_window(tail),
            "save-mark-and-excursion" => self.sf_save_mark_and_excursion(tail),
            "save-restriction" => self.sf_save_restriction(tail),
            "save-match-data" => self.sf_save_match_data(tail),
            "with-local-quit" => self.sf_with_local_quit(tail),
            "with-temp-message" => self.sf_with_temp_message(tail),
            "with-demoted-errors" => self.sf_with_demoted_errors(tail),
            "with-current-buffer" => self.sf_with_current_buffer(tail),
            "ignore-errors" => self.sf_ignore_errors(tail),
            "dotimes" => self.sf_dotimes(tail),
            "dolist" => self.sf_dolist(tail),
            // Custom system special forms
            "defcustom" => super::custom::sf_defcustom(self, tail),
            "defgroup" => super::custom::sf_defgroup(self, tail),
            "setq-default" => super::custom::sf_setq_default(self, tail),
            "defvar-local" => super::custom::sf_defvar_local(self, tail),
            // Autoload special forms
            "autoload" => super::autoload::sf_autoload(self, tail),
            "eval-when-compile" => super::autoload::sf_eval_when_compile(self, tail),
            "eval-and-compile" => super::autoload::sf_eval_and_compile(self, tail),
            // Error hierarchy
            "define-error" => super::errors::sf_define_error(self, tail),
            // Reader/printer special forms
            "with-output-to-string" => super::reader::sf_with_output_to_string(self, tail),
            // Threading
            "with-mutex" => super::threads::sf_with_mutex(self, tail),
            // Misc special forms
            "with-temp-buffer" => super::misc::sf_with_temp_buffer(self, tail),
            "save-current-buffer" => super::misc::sf_save_current_buffer(self, tail),
            "track-mouse" => super::misc::sf_track_mouse(self, tail),
            "with-syntax-table" => super::misc::sf_with_syntax_table(self, tail),
            // Interactive / mode definition special forms
            "define-minor-mode" => super::interactive::sf_define_minor_mode(self, tail),
            "define-derived-mode" => super::interactive::sf_define_derived_mode(self, tail),
            "define-generic-mode" => super::interactive::sf_define_generic_mode(self, tail),
            _ => return None,
        })
    }

    fn sf_quote(&self, tail: &[Expr]) -> EvalResult {
        if tail.len() != 1 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("quote"), Value::Int(tail.len() as i64)],
            ));
        }
        Ok(quote_to_value(&tail[0]))
    }

    fn sf_backquote(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.len() != 1 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![
                    Value::cons(Value::Int(1), Value::Int(1)),
                    Value::Int(tail.len() as i64),
                ],
            ));
        }
        let template = quote_to_value(&tail[0]);
        self.eval_backquote_template(&template, 1)
    }

    fn backquote_marker_arg(template: &Value, marker: &str) -> Option<Value> {
        let items = list_to_vec(template)?;
        if items.len() == 2 && items[0].as_symbol_name() == Some(marker) {
            Some(items[1])
        } else {
            None
        }
    }

    fn eval_backquote_template(&mut self, template: &Value, depth: usize) -> EvalResult {
        if let Some(arg_expr) = Self::backquote_marker_arg(template, ",@") {
            if depth == 1 {
                return self.eval_value(&arg_expr);
            }
            let inner = self.eval_backquote_template(&arg_expr, depth.saturating_sub(1))?;
            return Ok(Value::list(vec![Value::symbol(",@"), inner]));
        }

        if let Some(arg_expr) = Self::backquote_marker_arg(template, ",") {
            if depth == 1 {
                return self.eval_value(&arg_expr);
            }
            let inner = self.eval_backquote_template(&arg_expr, depth.saturating_sub(1))?;
            return Ok(Value::list(vec![Value::symbol(","), inner]));
        }

        if let Some(inner) = Self::backquote_marker_arg(template, "`") {
            let expanded = self.eval_backquote_template(&inner, depth.saturating_add(1))?;
            return Ok(Value::list(vec![Value::symbol("`"), expanded]));
        }

        match template {
            Value::Cons(_) => self.eval_backquote_list_template(template, depth),
            Value::Vector(v) => self.eval_backquote_vector_template(*v, depth),
            _ => Ok(*template),
        }
    }

    fn eval_backquote_list_template(&mut self, template: &Value, depth: usize) -> EvalResult {
        let mut expanded_items = Vec::new();
        let mut cursor = *template;

        while let Value::Cons(cell) = cursor {
            let pair = read_cons(cell);
            let car = pair.car;
            cursor = pair.cdr;
            drop(pair);

            match self.eval_backquote_element(&car, depth)? {
                BackquoteElement::Item(value) => expanded_items.push(value),
                BackquoteElement::Splice(mut values) => expanded_items.append(&mut values),
            }
        }

        let mut tail = if cursor.is_nil() {
            Value::Nil
        } else {
            self.eval_backquote_template(&cursor, depth)?
        };

        for value in expanded_items.into_iter().rev() {
            tail = Value::cons(value, tail);
        }
        Ok(tail)
    }

    fn eval_backquote_vector_template(&mut self, vector_id: ObjId, depth: usize) -> EvalResult {
        let items = with_heap(|h| h.get_vector(vector_id).clone());
        let mut expanded_items = Vec::new();
        for item in items {
            match self.eval_backquote_element(&item, depth)? {
                BackquoteElement::Item(value) => expanded_items.push(value),
                BackquoteElement::Splice(mut values) => expanded_items.append(&mut values),
            }
        }
        Ok(Value::vector(expanded_items))
    }

    fn eval_backquote_element(
        &mut self,
        element: &Value,
        depth: usize,
    ) -> Result<BackquoteElement, Flow> {
        if let Some(arg_expr) = Self::backquote_marker_arg(element, ",@") {
            if depth == 1 {
                let evaluated = self.eval_value(&arg_expr)?;
                let values = list_to_vec(&evaluated).ok_or_else(|| {
                    signal(
                        "wrong-type-argument",
                        vec![Value::symbol("listp"), evaluated],
                    )
                })?;
                return Ok(BackquoteElement::Splice(values));
            }
            let inner = self.eval_backquote_template(&arg_expr, depth.saturating_sub(1))?;
            return Ok(BackquoteElement::Item(Value::list(vec![
                Value::symbol(",@"),
                inner,
            ])));
        }

        if let Some(arg_expr) = Self::backquote_marker_arg(element, ",") {
            if depth == 1 {
                return Ok(BackquoteElement::Item(self.eval_value(&arg_expr)?));
            }
            let inner = self.eval_backquote_template(&arg_expr, depth.saturating_sub(1))?;
            return Ok(BackquoteElement::Item(Value::list(vec![
                Value::symbol(","),
                inner,
            ])));
        }

        if let Some(inner) = Self::backquote_marker_arg(element, "`") {
            let expanded = self.eval_backquote_template(&inner, depth.saturating_add(1))?;
            return Ok(BackquoteElement::Item(Value::list(vec![
                Value::symbol("`"),
                expanded,
            ])));
        }

        Ok(BackquoteElement::Item(
            self.eval_backquote_template(element, depth)?,
        ))
    }

    fn sf_function(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.len() != 1 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("function"), Value::Int(tail.len() as i64)],
            ));
        }
        match &tail[0] {
            Expr::List(items) => {
                // #'(lambda ...) — create closure
                if let Some(Expr::Symbol(id)) = items.first() {
                    if resolve_sym(*id) == "lambda" {
                        return self.eval_lambda(&items[1..]);
                    }
                }
                Ok(quote_to_value(&tail[0]))
            }
            _ => Ok(quote_to_value(&tail[0])),
        }
    }

    fn sf_let(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("let"), Value::Int(tail.len() as i64)],
            ));
        }

        let mut lexical_bindings: Vec<(SymId, Value)> = Vec::new();
        let mut dynamic_bindings = OrderedSymMap::new();
        let mut watcher_bindings: Vec<(String, Value, Value)> = Vec::new();
        let use_lexical = self.lexical_binding();
        let mut constant_binding_error: Option<String> = None;

        // Root binding values during evaluation so GC triggered by later
        // initializers doesn't collect earlier ones.
        let saved_roots = self.temp_roots.len();
        match &tail[0] {
            Expr::List(entries) => {
                for binding in entries {
                    match binding {
                        Expr::Symbol(id) => {
                            let name = resolve_sym(*id);
                            if name == "nil" || name == "t" {
                                if constant_binding_error.is_none() {
                                    constant_binding_error = Some(name.to_owned());
                                }
                                continue;
                            }
                            let old_value = self.visible_variable_value_or_nil(name);
                            if use_lexical && !self.obarray.is_special(name) {
                                lexical_bindings.push((*id, Value::Nil));
                            } else {
                                dynamic_bindings.insert(*id, Value::Nil);
                            }
                            watcher_bindings.push((name.to_owned(), Value::Nil, old_value));
                        }
                        Expr::List(pair) if !pair.is_empty() => {
                            let Expr::Symbol(id) = &pair[0] else {
                                self.temp_roots.truncate(saved_roots);
                                return Err(signal(
                                    "wrong-type-argument",
                                    vec![Value::symbol("symbolp"), quote_to_value(&pair[0])],
                                ));
                            };
                            let name = resolve_sym(*id);
                            let value = if pair.len() > 1 {
                                match self.eval(&pair[1]) {
                                    Ok(v) => v,
                                    Err(e) => {
                                        self.temp_roots.truncate(saved_roots);
                                        return Err(e);
                                    }
                                }
                            } else {
                                Value::Nil
                            };
                            self.temp_roots.push(value);
                            if name == "nil" || name == "t" {
                                if constant_binding_error.is_none() {
                                    constant_binding_error = Some(name.to_owned());
                                }
                                continue;
                            }
                            let old_value = self.visible_variable_value_or_nil(name);
                            if use_lexical && !self.obarray.is_special(name) {
                                lexical_bindings.push((*id, value));
                            } else {
                                dynamic_bindings.insert(*id, value);
                            }
                            watcher_bindings.push((name.to_owned(), value, old_value));
                        }
                        _ => {
                            self.temp_roots.truncate(saved_roots);
                            return Err(signal("wrong-type-argument", vec![]));
                        }
                    }
                }
            }
            Expr::Symbol(id) if resolve_sym(*id) == "nil" => {} // (let nil ...)
            Expr::DottedList(_, last) => {
                self.temp_roots.truncate(saved_roots);
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), quote_to_value(last)],
                ))
            }
            other => {
                self.temp_roots.truncate(saved_roots);
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), quote_to_value(other)],
                ))
            }
        }
        // Binding values are about to be moved into dynamic/lexenv (rooted).
        self.temp_roots.truncate(saved_roots);
        if let Some(name) = constant_binding_error {
            return Err(signal("setting-constant", vec![Value::symbol(name)]));
        }

        let pushed_lex = !lexical_bindings.is_empty();
        let pushed_dyn = !dynamic_bindings.is_empty();
        // Save lexenv before prepending bindings.
        let saved_lexenv = if pushed_lex {
            let saved = self.lexenv;
            self.saved_lexenvs.push(saved);
            // Prepend each binding in source order — this matches GNU Emacs's
            // Flet which prepends (cons-es) each binding onto the alist,
            // naturally reversing the source order.
            for (sym_id, val) in &lexical_bindings {
                self.lexenv = lexenv_prepend(self.lexenv, *sym_id, *val);
            }
            true
        } else {
            false
        };
        if pushed_dyn {
            self.dynamic.push(dynamic_bindings);
        }

        for (name, value, _) in &watcher_bindings {
            if let Err(error) = self.run_variable_watchers(name, value, &Value::Nil, "let") {
                if pushed_dyn {
                    self.dynamic.pop();
                }
                if saved_lexenv {
                    self.lexenv = self.saved_lexenvs.pop().unwrap();
                }
                return Err(error);
            }
        }

        let result = self.sf_progn(&tail[1..]);
        if pushed_dyn {
            self.dynamic.pop();
        }
        if saved_lexenv {
            self.lexenv = self.saved_lexenvs.pop().unwrap();
        }

        let unlet_result = self.run_unlet_watchers(&watcher_bindings);
        match (result, unlet_result) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(value), Ok(())) => Ok(value),
        }
    }

    fn sf_let_star(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("let*"), Value::Int(tail.len() as i64)],
            ));
        }

        let entries = match &tail[0] {
            Expr::List(entries) => entries.clone(),
            Expr::Symbol(id) if resolve_sym(*id) == "nil" => Vec::new(),
            Expr::DottedList(_, last) => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), quote_to_value(last)],
                ))
            }
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), quote_to_value(other)],
                ))
            }
        };

        let use_lexical = self.lexical_binding();
        let saved_lex = use_lexical; // Save lexenv when lexical mode active
        let pushed_dyn = true; // Always push a dynamic frame too (for special vars or dynamic mode)
        let mut watcher_bindings: Vec<(String, Value, Value)> = Vec::new();

        self.dynamic.push(OrderedSymMap::new());
        let saved_lexenv = if use_lexical {
            let saved = self.lexenv;
            self.saved_lexenvs.push(saved);
            true
        } else {
            false
        };

        let init_result: Result<(), Flow> = (|| {
            for binding in &entries {
                match binding {
                    Expr::Symbol(id) => {
                        let name = resolve_sym(*id);
                        if name == "nil" || name == "t" {
                            return Err(signal("setting-constant", vec![Value::symbol(name)]));
                        }
                        let old_value = self.visible_variable_value_or_nil(name);
                        if use_lexical && !self.obarray.is_special(name) {
                            self.lexenv = lexenv_prepend(self.lexenv, *id, Value::Nil);
                        } else if let Some(frame) = self.dynamic.last_mut() {
                            frame.insert(*id, Value::Nil);
                        }
                        watcher_bindings.push((name.to_owned(), Value::Nil, old_value));
                        self.run_variable_watchers(name, &Value::Nil, &Value::Nil, "let")?;
                    }
                    Expr::List(pair) if !pair.is_empty() => {
                        let Expr::Symbol(id) = &pair[0] else {
                            return Err(signal(
                                "wrong-type-argument",
                                vec![Value::symbol("symbolp"), quote_to_value(&pair[0])],
                            ));
                        };
                        let name = resolve_sym(*id);
                        let value = if pair.len() > 1 {
                            self.eval(&pair[1])?
                        } else {
                            Value::Nil
                        };
                        if name == "nil" || name == "t" {
                            return Err(signal("setting-constant", vec![Value::symbol(name)]));
                        }
                        let old_value = self.visible_variable_value_or_nil(name);
                        if use_lexical && !self.obarray.is_special(name) {
                            self.lexenv = lexenv_prepend(self.lexenv, *id, value);
                        } else if let Some(frame) = self.dynamic.last_mut() {
                            frame.insert(*id, value);
                        }
                        watcher_bindings.push((name.to_owned(), value, old_value));
                        self.run_variable_watchers(name, &value, &Value::Nil, "let")?;
                    }
                    _ => return Err(signal("wrong-type-argument", vec![])),
                }
            }
            Ok(())
        })();
        if let Err(error) = init_result {
            if saved_lexenv {
                self.lexenv = self.saved_lexenvs.pop().unwrap();
            }
            self.dynamic.pop();

            let _ = self.run_unlet_watchers(&watcher_bindings);
            return Err(error);
        }

        let result = self.sf_progn(&tail[1..]);
        if pushed_dyn {
            self.dynamic.pop();
        }
        if saved_lexenv {
            self.lexenv = self.saved_lexenvs.pop().unwrap();
        }

        let unlet_result = self.run_unlet_watchers(&watcher_bindings);
        match (result, unlet_result) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(value), Ok(())) => Ok(value),
        }
    }

    fn sf_setq(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Ok(Value::Nil);
        }
        if !tail.len().is_multiple_of(2) {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("setq"), Value::Int(tail.len() as i64)],
            ));
        }

        let mut last = Value::Nil;
        let mut i = 0;
        while i < tail.len() {
            let (sym_id, name) = match &tail[i] {
                Expr::Symbol(id) => (*id, resolve_sym(*id)),
                Expr::Keyword(id) => (*id, resolve_sym(*id)),
                _ => {
                    return Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("symbolp"), quote_to_value(&tail[i])],
                    ))
                }
            };
            let value = self.eval(&tail[i + 1])?;
            let resolved = super::builtins::resolve_variable_alias_name(self, name)?;
            if self.obarray.is_constant(&resolved) {
                return Err(signal(
                    "setting-constant",
                    vec![Value::symbol(name)],
                ));
            }
            // If the variable has an alias, use the resolved (interned) name.
            // Otherwise, preserve the original SymId for uninterned symbol support.
            if resolved != name {
                self.assign_with_watchers(&resolved, value, "set")?;
            } else {
                self.assign_with_watchers_by_id(sym_id, value, "set")?;
            }
            last = value;
            i += 2;
        }
        Ok(last)
    }

    fn sf_setq_local(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Ok(Value::Nil);
        }
        if !tail.len().is_multiple_of(2) {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("setq-local"), Value::Int(tail.len() as i64)],
            ));
        }

        let mut last = Value::Nil;
        let mut i = 0;
        while i < tail.len() {
            let name = match &tail[i] {
                Expr::Symbol(id) => resolve_sym(*id),
                Expr::Keyword(id) => resolve_sym(*id),
                _ => {
                    return Err(signal(
                        "error",
                        vec![Value::string(format!(
                            "Attempting to set a non-symbol: {}",
                            super::expr::print_expr(&tail[i])
                        ))],
                    ))
                }
            };
            let resolved = super::builtins::resolve_variable_alias_name(self, name)?;

            if self.obarray.is_constant(&resolved) {
                return Err(signal(
                    "setting-constant",
                    vec![Value::symbol(name)],
                ));
            }

            let value = self.eval(&tail[i + 1])?;
            if self.buffers.current_buffer().is_some() {
                let where_arg = {
                    let buf = self
                        .buffers
                        .current_buffer_mut()
                        .expect("checked above for current buffer");
                    let where_arg = Value::Buffer(buf.id);
                    buf.set_buffer_local(&resolved, value);
                    where_arg
                };
                self.run_variable_watchers_with_where(
                    &resolved,
                    &value,
                    &Value::Nil,
                    "set",
                    &where_arg,
                )?;
            } else {
                self.assign_with_watchers(&resolved, value, "set")?;
            }
            last = value;
            i += 2;
        }
        Ok(last)
    }

    fn sf_if(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.len() < 2 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("if"), Value::Int(tail.len() as i64)],
            ));
        }
        let cond = self.eval(&tail[0])?;
        if cond.is_truthy() {
            self.eval(&tail[1])
        } else {
            self.sf_progn(&tail[2..])
        }
    }

    fn sf_and(&mut self, tail: &[Expr]) -> EvalResult {
        let mut last = Value::True;
        for expr in tail {
            last = self.eval(expr)?;
            if last.is_nil() {
                return Ok(Value::Nil);
            }
        }
        Ok(last)
    }

    fn sf_or(&mut self, tail: &[Expr]) -> EvalResult {
        for expr in tail {
            let val = self.eval(expr)?;
            if val.is_truthy() {
                return Ok(val);
            }
        }
        Ok(Value::Nil)
    }

    fn sf_cond(&mut self, tail: &[Expr]) -> EvalResult {
        for clause in tail {
            let Expr::List(items) = clause else {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("listp"), quote_to_value(clause)],
                ));
            };
            if items.is_empty() {
                continue;
            }
            let test = self.eval(&items[0])?;
            if test.is_truthy() {
                if items.len() == 1 {
                    return Ok(test);
                }
                return self.sf_progn(&items[1..]);
            }
        }
        Ok(Value::Nil)
    }

    fn sf_while(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("while"), Value::Int(tail.len() as i64)],
            ));
        }
        let mut iters: u64 = 0;
        loop {
            let cond = self.eval(&tail[0])?;
            if cond.is_nil() {
                return Ok(Value::Nil);
            }
            self.sf_progn(&tail[1..])?;
            iters += 1;
            if iters == 1_000_000 {
                let cond_str = super::expr::print_expr(&tail[0]);
                tracing::warn!(
                    "while loop exceeded 1M iterations, cond: {}",
                    &cond_str[..cond_str.len().min(300)]
                );
            }
            self.gc_safe_point();
        }
    }

    pub(crate) fn sf_progn(&mut self, forms: &[Expr]) -> EvalResult {
        let mut last = Value::Nil;
        for form in forms {
            last = self.eval(form)?;
        }
        Ok(last)
    }

    fn sf_prog1(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("prog1"), Value::Int(tail.len() as i64)],
            ));
        }
        let first = self.eval(&tail[0])?;
        for form in &tail[1..] {
            self.eval(form)?;
        }
        Ok(first)
    }

    fn sf_when(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![
                    Value::cons(Value::Int(1), Value::Int(1)),
                    Value::Int(tail.len() as i64),
                ],
            ));
        }
        let cond = self.eval(&tail[0])?;
        if cond.is_truthy() {
            self.sf_progn(&tail[1..])
        } else {
            Ok(Value::Nil)
        }
    }

    fn sf_unless(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![
                    Value::cons(Value::Int(1), Value::Int(1)),
                    Value::Int(tail.len() as i64),
                ],
            ));
        }
        let cond = self.eval(&tail[0])?;
        if cond.is_nil() {
            self.sf_progn(&tail[1..])
        } else {
            Ok(Value::Nil)
        }
    }

    fn sf_bound_and_true_p(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.len() != 1 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![
                    Value::cons(Value::Int(1), Value::Int(1)),
                    Value::Int(tail.len() as i64),
                ],
            ));
        }
        let Expr::Symbol(id) = &tail[0] else {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), quote_to_value(&tail[0])],
            ));
        };
        match self.eval_symbol_by_id(*id) {
            Ok(value) => {
                if value.is_truthy() {
                    Ok(value)
                } else {
                    Ok(Value::Nil)
                }
            }
            Err(Flow::Signal(sig)) if sig.symbol_name() == "void-variable" => Ok(Value::Nil),
            Err(other) => Err(other),
        }
    }

    #[tracing::instrument(level = "trace", skip(self, tail), fields(name))]
    fn sf_defun(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.len() < 2 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![
                    Value::cons(Value::Int(2), Value::Int(2)),
                    Value::Int(tail.len() as i64),
                ],
            ));
        }
        let Expr::Symbol(id) = &tail[0] else {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), quote_to_value(&tail[0])],
            ));
        };
        let name = resolve_sym(*id);
        tracing::Span::current().record("name", name);
        let lambda = self.eval_lambda(&tail[1..])?;
        self.obarray.set_symbol_function(name, lambda);

        // Process (declare ...) forms in the body.
        // byte-run.el's defun macro normally handles this but NeoVM's
        // sf_defun intercepts before the macro can run.  We must process
        // key declarations here, especially `compiler-macro` which is
        // essential for cl-defstruct accessor setf to work.
        //
        // Body layout: (defun NAME PARAMS [DOCSTRING] [(declare ...)] BODY...)
        // tail[0] = NAME, tail[1] = PARAMS, tail[2..] = [doc] [declare] body
        let body_start = 2; // after NAME and PARAMS
        let mut idx = body_start;
        // Skip optional docstring
        if let Some(Expr::Str(_)) = tail.get(idx) {
            idx += 1;
        }
        // Process (declare ...) forms
        while let Some(Expr::List(decl_form)) = tail.get(idx) {
            if decl_form.first().is_some_and(|e| matches!(e, Expr::Symbol(s) if resolve_sym(*s) == "declare")) {
                for spec in &decl_form[1..] {
                    self.process_defun_declaration(name, spec);
                }
                idx += 1;
            } else {
                break;
            }
        }

        Ok(Value::symbol(name))
    }

    /// Process a single declaration spec from a `(declare ...)` form.
    /// Handles key declarations that byte-run.el's defun macro would process.
    fn process_defun_declaration(&mut self, fn_name: &str, spec: &Expr) {
        let Expr::List(items) = spec else { return };
        let Some(Expr::Symbol(key_id)) = items.first() else { return };
        let key = resolve_sym(*key_id);
        match key {
            "compiler-macro" => {
                // (compiler-macro CM-FN) → (put 'fn-name 'compiler-macro #'CM-FN)
                if let Some(cm_expr) = items.get(1) {
                    let cm_val = quote_to_value(cm_expr);
                    self.obarray.put_property(fn_name, "compiler-macro", cm_val);
                }
            }
            "side-effect-free" => {
                if let Some(val_expr) = items.get(1) {
                    let val = quote_to_value(val_expr);
                    self.obarray.put_property(fn_name, "side-effect-free", val);
                }
            }
            "pure" => {
                if let Some(val_expr) = items.get(1) {
                    let val = quote_to_value(val_expr);
                    self.obarray.put_property(fn_name, "pure", val);
                }
            }
            "gv-expander" | "gv-setter" => {
                // (gv-expander BODY) → (put 'fn-name 'gv-expander (lambda ...))
                // (gv-setter BODY) → (put 'fn-name 'gv-setter BODY)
                if let Some(val_expr) = items.get(1) {
                    if let Ok(val) = self.eval(val_expr) {
                        self.obarray.put_property(fn_name, key, val);
                    }
                }
            }
            "doc-string" => {
                // (doc-string N) → (put 'fn-name 'doc-string-elt N)
                if let Some(val_expr) = items.get(1) {
                    let val = quote_to_value(val_expr);
                    self.obarray.put_property(fn_name, "doc-string-elt", val);
                }
            }
            _ => {
                // Unknown declarations: check defun-declarations-alist
                // For now, silently ignore.
            }
        }
    }

    fn sf_defvar(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("defvar"), Value::Int(tail.len() as i64)],
            ));
        }
        if tail.len() > 3 {
            return Err(signal("error", vec![Value::string("Too many arguments")]));
        }
        let Expr::Symbol(id) = &tail[0] else {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), quote_to_value(&tail[0])],
            ));
        };
        let name = resolve_sym(*id);
        // Only set value if INITVALUE is provided and symbol is not already bound.
        // (defvar x) without INITVALUE only marks as special, does NOT bind.
        if tail.len() > 1 && !self.obarray.boundp(name) {
            let value = self.eval(&tail[1])?;
            self.obarray.set_symbol_value(name, value);
        }
        self.obarray.make_special(name);
        Ok(Value::symbol(name))
    }

    fn sf_defconst(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.len() < 2 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("defconst"), Value::Int(tail.len() as i64)],
            ));
        }
        if tail.len() > 3 {
            return Err(signal("error", vec![Value::string("Too many arguments")]));
        }
        let Expr::Symbol(id) = &tail[0] else {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), quote_to_value(&tail[0])],
            ));
        };
        let name = resolve_sym(*id);
        let value = self.eval(&tail[1])?;
        self.obarray.set_symbol_value(name, value);
        let sym = self.obarray.get_or_intern(name);
        sym.constant = true;
        sym.special = true;
        Ok(Value::symbol(name))
    }

    #[tracing::instrument(level = "trace", skip(self, tail), fields(name))]
    fn sf_defmacro(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.len() < 2 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![
                    Value::cons(Value::Int(2), Value::Int(2)),
                    Value::Int(tail.len() as i64),
                ],
            ));
        }
        let Expr::Symbol(id) = &tail[0] else {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), quote_to_value(&tail[0])],
            ));
        };
        let name = resolve_sym(*id);
        tracing::Span::current().record("name", name);
        let params = self.parse_lambda_params(&tail[1])?;
        let (docstring, body_start) = match tail.get(2) {
            Some(Expr::Str(s)) => (Some(s.clone()), 3),
            _ => (None, 2),
        };
        let body: Rc<Vec<Expr>> = tail[body_start..].to_vec().into();
        let macro_val = Value::make_macro(LambdaData {
            params,
            body,
            env: None,
            docstring,
            doc_form: None,
        });
        self.obarray.set_symbol_function(name, macro_val);
        Ok(Value::symbol(name))
    }

    fn sf_funcall(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("funcall"), Value::Int(tail.len() as i64)],
            ));
        }
        let function = match self.eval(&tail[0]) {
            Ok(function) => function,
            Err(Flow::Signal(sig))
                if sig.symbol_name() == "wrong-type-argument"
                    && matches!(
                        &tail[0],
                        Expr::List(items)
                            if matches!(
                                items.first(),
                                Some(Expr::Symbol(id))
                                    if resolve_sym(*id) == "lambda" || resolve_sym(*id) == "closure"
                            )
                    ) =>
            {
                return Err(signal(
                    "invalid-function",
                    vec![quote_to_value(&tail[0])],
                ));
            }
            Err(err) => return Err(err),
        };
        // Root the function value during arg evaluation in case GC fires.
        self.temp_roots.push(function);
        let (args, args_saved) = self.eval_args(&tail[1..])?;
        // Note: function is still rooted (pushed before eval_args).
        // args_saved captures the state AFTER the function push.
        // Restore to before function push after apply returns.
        let result = self.apply(function, args);
        self.restore_temp_roots(args_saved);
        self.temp_roots.pop(); // pop function
        result
    }

    /// Validate a `Flow::Throw` against the active catch tags.
    /// If a matching catch exists, pass through.  If not, convert to
    /// `Flow::Signal("no-catch", ...)` — mirrors GNU Emacs `Fthrow`.
    fn validate_throw(&self, flow: Flow) -> Flow {
        match flow {
            Flow::Throw { ref tag, ref value } => {
                if self.catch_tags.iter().rev().any(|t| eq_value(t, tag)) {
                    flow
                } else {
                    signal("no-catch", vec![*tag, *value])
                }
            }
            other => other,
        }
    }

    fn sf_catch(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("catch"), Value::Int(tail.len() as i64)],
            ));
        }
        let tag = self.eval(&tail[0])?;
        // Root tag so GC during body can't collect it.
        self.temp_roots.push(tag);
        // Register this catch tag so `throw` can check for a matching catch.
        self.catch_tags.push(tag);
        let result = match self.sf_progn(&tail[1..]) {
            Ok(value) => Ok(value),
            Err(Flow::Throw {
                tag: thrown_tag,
                value,
            }) if eq_value(&tag, &thrown_tag) => Ok(value),
            Err(flow) => Err(flow),
        };
        self.catch_tags.pop();
        self.temp_roots.pop();
        result
    }

    fn sf_throw(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.len() != 2 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("throw"), Value::Int(tail.len() as i64)],
            ));
        }
        let tag = self.eval(&tail[0])?;
        // Root tag so GC during value eval can't collect it.
        self.temp_roots.push(tag);
        let value = self.eval(&tail[1])?;
        self.temp_roots.pop();
        // Mirror GNU Emacs Fthrow: check for a matching catch first.
        // If found → Flow::Throw (bypasses condition-case, caught by catch).
        // If not → signal no-catch immediately (condition-case can catch this).
        if self.catch_tags.iter().rev().any(|t| eq_value(t, &tag)) {
            Err(Flow::Throw { tag, value })
        } else {
            Err(signal("no-catch", vec![tag, value]))
        }
    }

    fn sf_unwind_protect(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![
                    Value::symbol("unwind-protect"),
                    Value::Int(tail.len() as i64),
                ],
            ));
        }
        let primary = self.eval(&tail[0]);
        // Root the primary result so GC during cleanup can't collect it.
        if let Ok(ref val) = primary {
            self.temp_roots.push(*val);
        }
        let cleanup = self.sf_progn(&tail[1..]);
        if primary.is_ok() {
            self.temp_roots.pop();
        }
        match cleanup {
            Ok(_) => primary,
            Err(flow) => Err(flow),
        }
    }

    fn sf_condition_case(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.len() < 3 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![
                    Value::symbol("condition-case"),
                    Value::Int(tail.len() as i64),
                ],
            ));
        }

        let var = match &tail[0] {
            Expr::Symbol(id) => resolve_sym(*id).to_owned(),
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("symbolp"), quote_to_value(other)],
                ))
            }
        };
        let body = &tail[1];
        let handlers = &tail[2..];

        // Emacs validates handler shape even when BODY exits normally.
        for handler in handlers {
            match handler {
                Expr::List(_) => {}
                Expr::Symbol(id) if resolve_sym(*id) == "nil" => {}
                _ => {
                    return Err(signal(
                        "error",
                        vec![Value::string(format!(
                            "Invalid condition handler: {}",
                            super::expr::print_expr(handler)
                        ))],
                    ))
                }
            }
        }

        match self.eval(body) {
            Ok(value) => Ok(value),
            Err(Flow::Signal(sig)) => {
                for handler in handlers {
                    if matches!(handler, Expr::Symbol(id) if resolve_sym(*id) == "nil") {
                        continue;
                    }
                    let Expr::List(handler_items) = handler else {
                        return Err(signal("wrong-type-argument", vec![]));
                    };
                    if handler_items.is_empty() {
                        continue;
                    }

                    if signal_matches(&handler_items[0], sig.symbol_name()) {
                        let mut frame = OrderedSymMap::new();
                        if var != "nil" {
                            frame.insert(intern(&var), make_signal_binding_value(&sig));
                        }
                        self.dynamic.push(frame);
                        let result = self.sf_progn(&handler_items[1..]);
                        self.dynamic.pop();
                        return result;
                    }
                }
                Err(Flow::Signal(sig))
            }
            // Flow::Throw bypasses condition-case entirely (GNU Emacs semantics).
            // The throw was already validated to have a matching catch when it was
            // created in sf_throw / builtin_throw.  If there's no matching catch,
            // sf_throw signals no-catch as a Flow::Signal, which is handled above.
            Err(flow @ Flow::Throw { .. }) => Err(flow),
        }
    }

    /// Convert an `Expr` to a `Value`, treating everything as literal data
    /// except `(byte-code-literal ...)` forms which are evaluated to produce
    /// `Value::ByteCode`. This is needed because `.elc` constant vectors
    /// contain literal values (lists, symbols, etc.) that must NOT be evaluated,
    /// but may also contain nested `#[...]` compiled functions (parsed as
    /// `(byte-code-literal VECTOR)`) that DO need evaluation.
    fn quote_to_value_with_bytecode(&mut self, expr: &Expr) -> EvalResult {
        match expr {
            Expr::List(elts)
                if matches!(
                    elts.first(),
                    Some(Expr::Symbol(s)) if *s == intern("byte-code-literal")
                ) =>
            {
                self.eval(expr)
            }
            Expr::Vector(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.quote_to_value_with_bytecode(item)?);
                }
                Ok(Value::vector(values))
            }
            _ => Ok(quote_to_value(expr)),
        }
    }

    fn sf_byte_code_literal(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.len() != 1 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![
                    Value::symbol("byte-code-literal"),
                    Value::Int(tail.len() as i64),
                ],
            ));
        }

        let Expr::Vector(items) = &tail[0] else {
            return Err(signal(
                "wrong-type-argument",
                vec![Value::symbol("vectorp"), quote_to_value(&tail[0])],
            ));
        };

        // Need at least 4 elements: [arglist bytecodes constants maxdepth ...]
        if items.len() < 4 {
            // Not a valid bytecode object; return as a plain vector.
            let values = items.iter().map(quote_to_value).collect::<Vec<_>>();
            return Ok(Value::vector(values));
        }

        // Convert each element to a Value. Constants are literal data,
        // except nested #[...] (byte-code-literal) forms that need evaluation.
        let mut values = Vec::with_capacity(items.len());
        for item in items {
            values.push(self.quote_to_value_with_bytecode(item)?);
        }

        // Delegate to the shared make-byte-code construction.
        crate::emacs_core::builtins::make_byte_code_from_parts(
            &values[0],
            &values[1],
            &values[2],
            &values[3],
            values.get(4),
            values.get(5),
        )
    }

    /// Top-level `(byte-code "bytecodes" [constants] maxdepth)` form used in `.elc` files.
    /// Creates a temporary zero-arg ByteCodeFunction and executes it via the VM.
    fn sf_byte_code(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.len() != 3 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![
                    Value::symbol("byte-code"),
                    Value::Int(tail.len() as i64),
                ],
            ));
        }

        // The bytecode string and maxdepth are simple literals — quote them.
        // The constants vector may contain nested byte-code-literal forms.
        let bytecode_str = quote_to_value(&tail[0]);
        let constants_vec = self.quote_to_value_with_bytecode(&tail[1])?;
        let maxdepth = quote_to_value(&tail[2]);

        // Build a temporary zero-arg ByteCodeFunction
        use crate::emacs_core::bytecode::decode::{
            decode_gnu_bytecode, string_value_to_bytes,
        };
        use crate::emacs_core::bytecode::ByteCodeFunction;
        use crate::emacs_core::value::LambdaParams;

        let raw_bytes = if let Some(s) = bytecode_str.as_str() {
            string_value_to_bytes(s)
        } else {
            Vec::new()
        };

        let mut constants: Vec<Value> = match constants_vec {
            Value::Vector(id) => with_heap(|h| h.get_vector(id).clone()),
            _ => Vec::new(),
        };

        // Convert nested bytecode vectors in constants
        for i in 0..constants.len() {
            constants[i] = crate::emacs_core::builtins::try_convert_nested_bytecode(constants[i]);
        }

        let ops = decode_gnu_bytecode(&raw_bytes, &mut constants).map_err(|e| {
            signal(
                "error",
                vec![Value::string(format!("bytecode decode error: {}", e))],
            )
        })?;

        let max_stack = match maxdepth {
            Value::Int(n) => n as u16,
            _ => 16,
        };

        let bc = ByteCodeFunction {
            ops,
            constants,
            max_stack,
            params: LambdaParams::simple(vec![]),
            env: None,
            docstring: None,
        };

        // Execute via VM
        self.refresh_features_from_variable();
        let mut vm = super::bytecode::Vm::new(
            &mut self.obarray,
            &mut self.dynamic,
            &mut self.lexenv,
            &mut self.features,
            &mut self.buffers,
            &mut self.match_data,
            &mut self.watchers,
            &mut self.catch_tags,
        );
        let result = vm.execute(&bc, vec![]);
        self.sync_features_variable();
        result
    }

    fn sf_defalias(&mut self, tail: &[Expr]) -> EvalResult {
        if !(2..=3).contains(&tail.len()) {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("defalias"), Value::Int(tail.len() as i64)],
            ));
        }
        let sym = self.eval(&tail[0])?;
        let def = self.eval(&tail[1])?;
        if tail.len() > 2 {
            let _ = self.eval(&tail[2])?;
        }
        self.defalias_value(sym, def)
    }

    fn sf_provide(&mut self, tail: &[Expr]) -> EvalResult {
        if !(1..=2).contains(&tail.len()) {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("provide"), Value::Int(tail.len() as i64)],
            ));
        }
        let feature = self.eval(&tail[0])?;
        let subfeatures = if tail.len() > 1 {
            Some(self.eval(&tail[1])?)
        } else {
            None
        };
        self.provide_value(feature, subfeatures)
    }

    fn sf_require(&mut self, tail: &[Expr]) -> EvalResult {
        if !(1..=3).contains(&tail.len()) {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("require"), Value::Int(tail.len() as i64)],
            ));
        }
        let feature = self.eval(&tail[0])?;
        let filename = if tail.len() > 1 {
            Some(self.eval(&tail[1])?)
        } else {
            None
        };
        let noerror = if tail.len() > 2 {
            Some(self.eval(&tail[2])?)
        } else {
            None
        };
        self.require_value(feature, filename, noerror)
    }

    pub(crate) fn defalias_value(&mut self, sym: Value, def: Value) -> EvalResult {
        let name = sym.as_symbol_name().map(str::to_string).ok_or_else(|| {
            signal(
                "wrong-type-argument",
                vec![Value::symbol("symbolp"), sym],
            )
        })?;
        if name == "nil" {
            return Err(signal("setting-constant", vec![Value::symbol("nil")]));
        }
        if builtins::would_create_function_alias_cycle(self, &name, &def) {
            return Err(signal(
                "cyclic-function-indirection",
                vec![Value::symbol(name.clone())],
            ));
        }
        self.obarray.set_symbol_function(&name, def);
        Ok(sym)
    }

    #[tracing::instrument(level = "info", skip(self, subfeatures))]
    pub(crate) fn provide_value(
        &mut self,
        feature: Value,
        subfeatures: Option<Value>,
    ) -> EvalResult {
        let name = match &feature {
            Value::Symbol(s) => resolve_sym(*s).to_owned(),
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("symbolp"), feature],
                ))
            }
        };
        if let Some(value) = subfeatures {
            self.obarray
                .put_property(&name, "subfeatures", value);
        }
        self.add_feature(&name);
        Ok(feature)
    }

    #[tracing::instrument(level = "info", skip(self), err(Debug))]
    pub(crate) fn require_value(
        &mut self,
        feature: Value,
        filename: Option<Value>,
        noerror: Option<Value>,
    ) -> EvalResult {
        let sym_id = match &feature {
            Value::Symbol(s) => *s,
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("symbolp"), feature],
                ))
            }
        };
        let name = resolve_sym(sym_id).to_owned();
        if self.has_feature(&name) {
            return Ok(Value::symbol(&name));
        }

        // Official Emacs treats recursive require as a no-op (returns feature symbol)
        // rather than signaling an error. This is common in practice when modules
        // have circular dependencies (e.g., dired ↔ dired-aux, project ↔ xref).
        if self.require_stack.iter().any(|f| *f == sym_id) {
            tracing::debug!("Recursive require for feature '{}', returning immediately", name);
            return Ok(Value::symbol(&name));
        }
        self.require_stack.push(sym_id);

        let result = (|| -> EvalResult {
            let filename = match &filename {
                Some(Value::Str(id)) => self.heap.get_string(*id).clone(),
                Some(_) | None => name.clone(),
            };

            let load_path = super::load::get_load_path(&self.obarray);
            match super::load::find_file_in_load_path(&filename, &load_path) {
                Some(path) => {
                    self.load_file_internal(&path)?;
                    if self.has_feature(&name) {
                        Ok(Value::symbol(name))
                    } else {
                        Err(signal(
                            "error",
                            vec![Value::string(format!(
                                "Required feature '{}' was not provided",
                                name
                            ))],
                        ))
                    }
                }
                None => {
                    if noerror.is_some_and(|value| value.is_truthy()) {
                        return Ok(Value::Nil);
                    }
                    Err(signal(
                        "file-missing",
                        vec![Value::string(format!(
                            "Cannot open load file: no such file or directory, {}",
                            name
                        ))],
                    ))
                }
            }
        })();
        let _ = self.require_stack.pop();
        result
    }

    fn sf_with_current_buffer(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("with-current-buffer"), Value::Int(tail.len() as i64)],
            ));
        }
        let buf_val = self.eval(&tail[0])?;
        let target_id = match &buf_val {
            Value::Buffer(id) => *id,
            Value::Str(id) => {
                let s = self.heap.get_string(*id).clone();
                self.buffers.find_buffer_by_name(&s).ok_or_else(|| {
                    signal("error", vec![Value::string(format!("No buffer named {s}"))])
                })?
            }
            other => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("bufferp"), *other],
                ))
            }
        };
        // Save current buffer, switch, run body, restore
        let saved = self.buffers.current_buffer().map(|b| b.id);
        self.buffers.set_current(target_id);
        let result = self.sf_progn(&tail[1..]);
        if let Some(saved_id) = saved {
            self.buffers.set_current(saved_id);
        }
        result
    }

    fn sf_save_excursion(&mut self, tail: &[Expr]) -> EvalResult {
        // Save current buffer, point, and mark; restore after body
        let saved_buf = self.buffers.current_buffer().map(|b| b.id);
        let (saved_pt, saved_mark) = match self.buffers.current_buffer() {
            Some(b) => (b.pt, b.mark),
            None => (0, None),
        };
        let result = self.sf_progn(tail);
        // Restore
        if let Some(buf_id) = saved_buf {
            self.buffers.set_current(buf_id);
            if let Some(buf) = self.buffers.get_mut(buf_id) {
                buf.pt = saved_pt;
                buf.mark = saved_mark;
            }
        }
        result
    }

    fn sf_save_window_excursion(&mut self, tail: &[Expr]) -> EvalResult {
        let saved_configuration =
            super::builtins::builtin_current_window_configuration(self, vec![])?;
        // Root saved configuration so GC during body can't collect it.
        self.temp_roots.push(saved_configuration);
        let result = self.sf_progn(tail);
        self.temp_roots.pop();
        let _ = super::builtins::builtin_set_window_configuration(self, vec![saved_configuration]);
        result
    }

    fn sf_save_selected_window(&mut self, tail: &[Expr]) -> EvalResult {
        let saved_window = super::window_cmds::builtin_selected_window(self, vec![]).ok();
        let saved_buffer = self.buffers.current_buffer().map(|b| b.id);
        // Root saved window so GC during body can't collect it.
        if let Some(ref w) = saved_window {
            self.temp_roots.push(*w);
        }
        let result = self.sf_progn(tail);
        if saved_window.is_some() {
            self.temp_roots.pop();
        }
        if let Some(window) = saved_window {
            let _ = super::window_cmds::builtin_select_window(self, vec![window, Value::Nil]);
        }
        if let Some(buffer_id) = saved_buffer {
            self.buffers.set_current(buffer_id);
        }
        result
    }

    fn sf_save_mark_and_excursion(&mut self, tail: &[Expr]) -> EvalResult {
        // Save mark-active dynamic/global state in addition to save-excursion state.
        let saved_mark_active = match self.eval_symbol("mark-active") {
            Ok(value) => value,
            Err(Flow::Signal(sig)) if sig.symbol_name() == "void-variable" => Value::Nil,
            Err(flow) => return Err(flow),
        };
        // Root saved value so GC during body can't collect it.
        self.temp_roots.push(saved_mark_active);
        let result = self.sf_save_excursion(tail);
        self.temp_roots.pop();
        self.assign("mark-active", saved_mark_active);
        result
    }

    fn sf_save_restriction(&mut self, tail: &[Expr]) -> EvalResult {
        // Save narrowing boundaries; restore after body
        let (saved_begv, saved_zv) = match self.buffers.current_buffer() {
            Some(b) => (b.begv, b.zv),
            None => (0, 0),
        };
        let result = self.sf_progn(tail);
        if let Some(buf) = self.buffers.current_buffer_mut() {
            buf.begv = saved_begv;
            buf.zv = saved_zv;
            buf.pt = buf.pt.clamp(buf.begv, buf.zv);
        }
        result
    }

    fn sf_save_match_data(&mut self, tail: &[Expr]) -> EvalResult {
        // Save global match data; restore after body (including non-local exits).
        let saved_match_data = self.match_data.clone();
        let result = self.sf_progn(tail);
        self.match_data = saved_match_data;
        result
    }

    fn sf_with_local_quit(&mut self, tail: &[Expr]) -> EvalResult {
        let mut frame = OrderedSymMap::new();
        frame.insert(intern("inhibit-quit"), Value::Nil);
        self.dynamic.push(frame);
        let result = self.sf_progn(tail);
        self.dynamic.pop();

        match result {
            Err(Flow::Signal(sig)) if sig.symbol_name() == "quit" => {
                self.assign("quit-flag", Value::True);
                Ok(Value::Nil)
            }
            other => other,
        }
    }

    fn sf_with_temp_message(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("with-temp-message"), Value::Int(0)],
            ));
        }

        let message_value = self.eval(&tail[0])?;
        // Root both values so GC during body can't collect them.
        self.temp_roots.push(message_value);
        let current_message = if message_value.is_truthy() {
            super::builtins::builtin_current_message(vec![])?
        } else {
            Value::Nil
        };
        self.temp_roots.push(current_message);
        if message_value.is_truthy() {
            let _ = super::builtins::builtin_message_eval(
                self,
                vec![Value::string("%s"), message_value],
            );
        }

        let result = self.sf_progn(&tail[1..]);

        if message_value.is_truthy() {
            if current_message.is_truthy() {
                let _ = super::builtins::builtin_message_eval(
                    self,
                    vec![Value::string("%s"), current_message],
                );
            } else {
                let _ = super::builtins::builtin_message_eval(self, vec![Value::Nil]);
            }
        }

        self.temp_roots.pop();
        self.temp_roots.pop();
        result
    }

    fn sf_with_demoted_errors(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::cons(Value::Int(1), Value::Int(1)), Value::Int(0)],
            ));
        }

        let (format, body) = match &tail[0] {
            Expr::Str(message) => {
                if tail.len() == 1 {
                    (message.clone(), tail)
                } else {
                    (message.clone(), &tail[1..])
                }
            }
            _ => ("Error: %S".to_string(), tail),
        };

        match self.sf_progn(body) {
            Ok(value) => Ok(value),
            Err(Flow::Signal(sig)) => {
                let _ = super::builtins::builtin_message_eval(
                    self,
                    vec![Value::string(format), make_signal_binding_value(&sig)],
                );
                Ok(Value::Nil)
            }
            Err(flow) => Err(flow),
        }
    }

    fn sf_ignore_errors(&mut self, tail: &[Expr]) -> EvalResult {
        match self.sf_progn(tail) {
            Ok(val) => Ok(val),
            Err(Flow::Signal(_)) => Ok(Value::Nil),
            Err(flow) => Err(flow),
        }
    }

    fn sf_dotimes(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("dotimes"), Value::Int(tail.len() as i64)],
            ));
        }
        let Expr::List(spec) = &tail[0] else {
            return Err(signal("wrong-type-argument", vec![]));
        };
        if spec.len() < 2 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("dotimes"), Value::Int(tail.len() as i64)],
            ));
        }
        let Expr::Symbol(var_id) = &spec[0] else {
            return Err(signal("wrong-type-argument", vec![]));
        };
        let var_id = *var_id;
        let count = self.eval(&spec[1])?;
        let count = match &count {
            Value::Int(n) => *n,
            _ => {
                return Err(signal(
                    "wrong-type-argument",
                    vec![Value::symbol("integerp"), count],
                ))
            }
        };

        self.dynamic.push(OrderedSymMap::new());
        for i in 0..count {
            if let Some(frame) = self.dynamic.last_mut() {
                frame.insert(var_id, Value::Int(i));
            }
            self.sf_progn(&tail[1..])?;
            self.gc_safe_point();
        }
        // Result value (third element of spec, or nil)
        let result = if spec.len() > 2 {
            if let Some(frame) = self.dynamic.last_mut() {
                frame.insert(var_id, Value::Int(count));
            }
            self.eval(&spec[2])?
        } else {
            Value::Nil
        };
        self.dynamic.pop();
        Ok(result)
    }

    fn sf_dolist(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("dolist"), Value::Int(tail.len() as i64)],
            ));
        }
        let Expr::List(spec) = &tail[0] else {
            return Err(signal("wrong-type-argument", vec![]));
        };
        if spec.len() < 2 {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("dolist"), Value::Int(tail.len() as i64)],
            ));
        }
        let Expr::Symbol(var_id) = &spec[0] else {
            return Err(signal("wrong-type-argument", vec![]));
        };
        let var_id = *var_id;
        let list_val = self.eval(&spec[1])?;
        let items = list_to_vec(&list_val).unwrap_or_default();

        self.dynamic.push(OrderedSymMap::new());
        for item in items {
            if let Some(frame) = self.dynamic.last_mut() {
                frame.insert(var_id, item);
            }
            self.sf_progn(&tail[1..])?;
            self.gc_safe_point();
        }
        let result = if spec.len() > 2 {
            if let Some(frame) = self.dynamic.last_mut() {
                frame.insert(var_id, Value::Nil);
            }
            self.eval(&spec[2])?
        } else {
            Value::Nil
        };
        self.dynamic.pop();
        Ok(result)
    }

    // -----------------------------------------------------------------------
    // Lambda / Function application
    // -----------------------------------------------------------------------

    pub(crate) fn eval_lambda(&mut self, tail: &[Expr]) -> EvalResult {
        if tail.is_empty() {
            return Err(signal(
                "wrong-number-of-arguments",
                vec![Value::symbol("lambda"), Value::Int(tail.len() as i64)],
            ));
        }

        let params = self.parse_lambda_params(&tail[0])?;

        // Extract docstring if present as the first body element.
        let (docstring, body_start) = match tail.get(1) {
            Some(Expr::Str(s)) => (Some(s.clone()), 2),
            _ => (None, 1),
        };

        // Check for (:documentation FORM) in the body — used by oclosures
        // to store the type symbol in slot 4 of the closure vector.
        let (doc_form, body_start) = if let Some(expr) = tail.get(body_start) {
            if let Expr::List(items) = expr {
                if items.len() == 2 {
                    if let Some(Expr::Keyword(kw)) = items.first() {
                        if resolve_sym(*kw) == ":documentation" {
                            let form_val = self.eval(&items[1])?;
                            (Some(form_val), body_start + 1)
                        } else {
                            (None, body_start)
                        }
                    } else {
                        (None, body_start)
                    }
                } else {
                    (None, body_start)
                }
            } else {
                (None, body_start)
            }
        } else {
            (None, body_start)
        };

        // Capture lexical environment for closures (when lexical-binding is on).
        // Always capture when lexical-binding is active, even if the env is empty.
        // This ensures lambda params are bound lexically (not dynamically) and that
        // inner closures can capture outer params — matching Emacs behavior.
        let env = if self.lexical_binding() {
            Some(self.lexenv)
        } else {
            None
        };

        Ok(Value::make_lambda(LambdaData {
            params,
            body: tail[body_start..].to_vec().into(),
            env,
            docstring,
            doc_form,
        }))
    }

    fn parse_lambda_params(&self, expr: &Expr) -> Result<LambdaParams, Flow> {
        match expr {
            Expr::Symbol(id) if resolve_sym(*id) == "nil" => Ok(LambdaParams::simple(vec![])),
            Expr::List(items) => {
                let mut required = Vec::new();
                let mut optional = Vec::new();
                let mut rest = None;
                let mut mode = 0; // 0=required, 1=optional, 2=rest

                for item in items {
                    let Expr::Symbol(id) = item else {
                        return Err(signal("wrong-type-argument", vec![]));
                    };
                    let name = resolve_sym(*id);
                    match name {
                        "&optional" => {
                            mode = 1;
                            continue;
                        }
                        "&rest" => {
                            mode = 2;
                            continue;
                        }
                        _ => {}
                    }
                    match mode {
                        0 => required.push(*id),
                        1 => optional.push(*id),
                        2 => {
                            rest = Some(*id);
                            break;
                        }
                        _ => unreachable!(),
                    }
                }

                Ok(LambdaParams {
                    required,
                    optional,
                    rest,
                })
            }
            _ => Err(signal("wrong-type-argument", vec![])),
        }
    }

    /// Apply a function value to evaluated arguments.
    pub(crate) fn apply(&mut self, function: Value, args: Vec<Value>) -> EvalResult {
        match function {
            Value::ByteCode(bc) => {
                self.refresh_features_from_variable();
                let func_val = Value::ByteCode(bc);
                let bc_data = self.heap.get_bytecode(bc).clone();
                let mut vm = super::bytecode::Vm::new(
                    &mut self.obarray,
                    &mut self.dynamic,
                    &mut self.lexenv,
                    &mut self.features,
                    &mut self.buffers,
                    &mut self.match_data,
                    &mut self.watchers,
                    &mut self.catch_tags,
                );
                let result = vm.execute_with_func_value(&bc_data, args, func_val);
                self.sync_features_variable();
                result
            }
            Value::Lambda(id) => {
                let func_val = Value::Lambda(id);
                let lambda_data = self.heap.get_lambda(id).clone();
                self.apply_lambda(&lambda_data, args, func_val)
            }
            Value::Macro(id) => {
                let func_val = Value::Macro(id);
                let lambda_data = self.heap.get_macro_data(id).clone();
                self.apply_lambda(&lambda_data, args, func_val)
            }
            Value::Subr(id) => self.apply_subr_object(resolve_sym(id), args, true),
            Value::Symbol(id) => {
                self.apply_named_callable(resolve_sym(id), args, Value::Subr(id), true)
            }
            Value::True => self.apply_named_callable("t", args, Value::Subr(intern("t")), true),
            Value::Keyword(id) => {
                self.apply_named_callable(resolve_sym(id), args, Value::Subr(id), true)
            }
            Value::Nil => {
                Err(signal("void-function", vec![Value::symbol("nil")]))
            }
            function @ Value::Cons(_) => {
                if super::autoload::is_autoload_value(&function) {
                    Err(signal(
                        "wrong-type-argument",
                        vec![Value::symbol("symbolp"), function],
                    ))
                } else if function.cons_car().is_symbol_named("lambda")
                    || function.cons_car().is_symbol_named("closure")
                {
                    match self.eval_value(&function) {
                        Ok(callable) => self.apply(callable, args),
                        Err(Flow::Signal(sig))
                            if sig.symbol_name() == "wrong-type-argument" =>
                        {
                            Err(signal("invalid-function", vec![function]))
                        }
                        Err(err) => Err(err),
                    }
                } else {
                    Err(signal("invalid-function", vec![function]))
                }
            }
            other => Err(signal("invalid-function", vec![other])),
        }
    }

    #[inline]
    fn apply_subr_object(
        &mut self,
        name: &str,
        args: Vec<Value>,
        rewrite_builtin_wrong_arity: bool,
    ) -> EvalResult {
        if super::subr_info::is_special_form(name) {
            return Err(signal(
                "invalid-function",
                vec![Value::Subr(intern(name))],
            ));
        }
        if super::subr_info::is_evaluator_callable_name(name) {
            return self.apply_evaluator_callable(name, args);
        }
        if let Some(result) = builtins::dispatch_builtin(self, name, args) {
            // Validate throws from builtins: builtins may generate Flow::Throw
            // (e.g. exit-minibuffer) without checking catch_tags.  Convert to
            // no-catch signal when no matching catch exists (GNU Emacs semantics).
            let result = result.map_err(|flow| self.validate_throw(flow));
            if rewrite_builtin_wrong_arity {
                result.map_err(|flow| rewrite_wrong_arity_function_object(flow, name))
            } else {
                result
            }
        } else {
            Err(signal("void-function", vec![Value::symbol(name)]))
        }
    }

    #[inline]
    fn resolve_named_call_target(&mut self, name: &str) -> NamedCallTarget {
        let function_epoch = self.obarray.function_epoch();
        if let Some(cache) = &self.named_call_cache {
            if cache.symbol == intern(name) && cache.function_epoch == function_epoch {
                return cache.target.clone();
            }
        }

        let target = if let Some(func) = self.obarray.symbol_function(name).cloned() {
            match &func {
                Value::Nil => NamedCallTarget::Void,
                // `(fset 'foo (symbol-function 'foo))` writes `#<subr foo>` into
                // the function cell. Treat this as a direct builtin/special-form
                // callable, not an obarray indirection cycle.
                Value::Subr(bound_name) if resolve_sym(*bound_name) == name => {
                    if super::subr_info::is_evaluator_callable_name(name) {
                        NamedCallTarget::EvaluatorCallable
                    } else if super::subr_info::is_special_form(name) {
                        NamedCallTarget::SpecialForm
                    } else {
                        NamedCallTarget::Probe
                    }
                }
                _ => NamedCallTarget::Obarray(func),
            }
        } else if self.obarray.is_function_unbound(name) {
            NamedCallTarget::Void
        } else if super::subr_info::is_evaluator_callable_name(name) {
            NamedCallTarget::EvaluatorCallable
        } else if super::subr_info::is_special_form(name) {
            NamedCallTarget::SpecialForm
        } else {
            NamedCallTarget::Probe
        };

        self.named_call_cache = Some(NamedCallCache {
            symbol: intern(name),
            function_epoch,
            target: target.clone(),
        });

        target
    }

    #[inline]
    fn apply_named_callable(
        &mut self,
        name: &str,
        args: Vec<Value>,
        invalid_fn: Value,
        rewrite_builtin_wrong_arity: bool,
    ) -> EvalResult {
        self.apply_named_callable_core(name, args, invalid_fn, rewrite_builtin_wrong_arity)
    }

    fn apply_named_callable_core(
        &mut self,
        name: &str,
        args: Vec<Value>,
        invalid_fn: Value,
        rewrite_builtin_wrong_arity: bool,
    ) -> EvalResult {
        match self.resolve_named_call_target(name) {
            NamedCallTarget::Obarray(func) => {
                if super::autoload::is_autoload_value(&func) {
                    return self.apply_named_autoload_callable(
                        name,
                        func,
                        args,
                        rewrite_builtin_wrong_arity,
                    );
                }
                let alias_target = match &func {
                    Value::Symbol(target) => Some(resolve_sym(*target).to_owned()),
                    Value::Subr(bound_name) if resolve_sym(*bound_name) != name => Some(resolve_sym(*bound_name).to_owned()),
                    _ => None,
                };
                let result = match self.apply(func, args) {
                    Err(Flow::Signal(sig)) if sig.symbol_name() == "invalid-function" => {
                        Err(signal("invalid-function", vec![Value::symbol(name)]))
                    }
                    other => other,
                };
                if let Some(target) = alias_target {
                    if rewrite_builtin_wrong_arity {
                        result
                    } else {
                        result.map_err(|flow| {
                            rewrite_wrong_arity_alias_function_object(flow, name, &target)
                        })
                    }
                } else {
                    result
                }
            }
            NamedCallTarget::EvaluatorCallable => self.apply_evaluator_callable(name, args),
            NamedCallTarget::Probe => {
                if let Some(result) = builtins::dispatch_builtin(self, name, args) {
                    self.named_call_cache = Some(NamedCallCache {
                        symbol: intern(name),
                        function_epoch: self.obarray.function_epoch(),
                        target: NamedCallTarget::Builtin,
                    });
                    let result = result.map_err(|flow| self.validate_throw(flow));
                    if rewrite_builtin_wrong_arity {
                        result.map_err(|flow| rewrite_wrong_arity_function_object(flow, name))
                    } else {
                        result
                    }
                } else {
                    self.named_call_cache = Some(NamedCallCache {
                        symbol: intern(name),
                        function_epoch: self.obarray.function_epoch(),
                        target: NamedCallTarget::Void,
                    });
                    Err(signal("void-function", vec![Value::symbol(name)]))
                }
            }
            NamedCallTarget::Builtin => {
                if let Some(result) = builtins::dispatch_builtin(self, name, args) {
                    let result = result.map_err(|flow| self.validate_throw(flow));
                    if rewrite_builtin_wrong_arity {
                        result.map_err(|flow| rewrite_wrong_arity_function_object(flow, name))
                    } else {
                        result
                    }
                } else {
                    self.named_call_cache = Some(NamedCallCache {
                        symbol: intern(name),
                        function_epoch: self.obarray.function_epoch(),
                        target: NamedCallTarget::Void,
                    });
                    Err(signal("void-function", vec![Value::symbol(name)]))
                }
            }
            NamedCallTarget::SpecialForm => Err(signal("invalid-function", vec![invalid_fn])),
            NamedCallTarget::Void => Err(signal("void-function", vec![Value::symbol(name)])),
        }
    }

    fn apply_named_autoload_callable(
        &mut self,
        name: &str,
        autoload_form: Value,
        args: Vec<Value>,
        rewrite_builtin_wrong_arity: bool,
    ) -> EvalResult {
        // Startup wrappers often expose autoload-shaped function cells for names
        // backed by builtins. Keep the autoload shape while preserving callability.
        if super::builtin_registry::is_dispatch_builtin_name(name) {
            if let Some(result) = builtins::dispatch_builtin(self, name, args.clone()) {
                return if rewrite_builtin_wrong_arity {
                    result.map_err(|flow| rewrite_wrong_arity_function_object(flow, name))
                } else {
                    result
                };
            }
        }

        let loaded = super::autoload::builtin_autoload_do_load(
            self,
            vec![autoload_form, Value::symbol(name)],
        )?;
        match self.apply(loaded, args) {
            Err(Flow::Signal(sig)) if sig.symbol_name() == "invalid-function" => {
                Err(signal("invalid-function", vec![Value::symbol(name)]))
            }
            other => other,
        }
    }

    fn apply_evaluator_callable(&mut self, name: &str, args: Vec<Value>) -> EvalResult {
        match name {
            "throw" => {
                if args.len() != 2 {
                    return Err(signal(
                        "wrong-number-of-arguments",
                        vec![
                            Value::Subr(intern("throw")),
                            Value::Int(args.len() as i64),
                        ],
                    ));
                }
                let tag = args[0];
                let value = args[1];
                if self.catch_tags.iter().rev().any(|t| eq_value(t, &tag)) {
                    Err(Flow::Throw { tag, value })
                } else {
                    Err(signal("no-catch", vec![tag, value]))
                }
            }
            _ => Err(signal("void-function", vec![Value::symbol(name)])),
        }
    }

    fn apply_lambda(
        &mut self,
        lambda: &LambdaData,
        args: Vec<Value>,
        func_value: Value,
    ) -> EvalResult {
        let params = &lambda.params;

        // Arity check
        if args.len() < params.min_arity() {
            tracing::warn!(
                "wrong-number-of-arguments (apply_lambda too few): got {} args, min={}, params={:?}, docstring={:?}",
                args.len(), params.min_arity(), params, lambda.docstring
            );
            return Err(signal(
                "wrong-number-of-arguments",
                vec![func_value, Value::Int(args.len() as i64)],
            ));
        }
        if let Some(max) = params.max_arity() {
            if args.len() > max {
                return Err(signal(
                    "wrong-number-of-arguments",
                    vec![func_value, Value::Int(args.len() as i64)],
                ));
            }
        }

        // If closure has a captured lexenv, restore it and prepend param bindings.
        // The old lexenv is saved on a GC-scanned stack so it survives
        // garbage collection during the function body evaluation.
        let has_lexenv = lambda.env.is_some();
        if let Some(env) = lambda.env {
            let old = std::mem::replace(&mut self.lexenv, env);
            // Prepend param bindings onto the captured env
            let mut arg_idx = 0;
            for param in &params.required {
                self.lexenv = lexenv_prepend(self.lexenv, *param, args[arg_idx]);
                arg_idx += 1;
            }
            for param in &params.optional {
                if arg_idx < args.len() {
                    self.lexenv = lexenv_prepend(self.lexenv, *param, args[arg_idx]);
                    arg_idx += 1;
                } else {
                    self.lexenv = lexenv_prepend(self.lexenv, *param, Value::Nil);
                }
            }
            if let Some(ref rest_name) = params.rest {
                let rest_args: Vec<Value> = args[arg_idx..].to_vec();
                self.lexenv = lexenv_prepend(self.lexenv, *rest_name, Value::list(rest_args));
            }
            // Save old lexenv on GC-scanned stack
            self.saved_lexenvs.push(old);
        } else {
            // Dynamic binding (no captured lexenv)
            let mut frame = OrderedSymMap::new();
            let mut arg_idx = 0;
            for param in &params.required {
                frame.insert(*param, args[arg_idx]);
                arg_idx += 1;
            }
            for param in &params.optional {
                if arg_idx < args.len() {
                    frame.insert(*param, args[arg_idx]);
                    arg_idx += 1;
                } else {
                    frame.insert(*param, Value::Nil);
                }
            }
            if let Some(ref rest_name) = params.rest {
                let rest_args: Vec<Value> = args[arg_idx..].to_vec();
                frame.insert(*rest_name, Value::list(rest_args));
            }
            self.dynamic.push(frame);
        }
        let saved_lexical_mode = if has_lexenv {
            let old = self.lexical_binding();
            self.set_lexical_binding(true);
            Some(old)
        } else {
            None
        };

        let result = self.sf_progn(&lambda.body);

        if let Some(old_mode) = saved_lexical_mode {
            self.set_lexical_binding(old_mode);
        }
        if has_lexenv {
            let old_lexenv = self.saved_lexenvs.pop().expect("saved_lexenvs underflow");
            self.lexenv = old_lexenv;
        } else {
            self.dynamic.pop();
        }
        result
    }

    // -----------------------------------------------------------------------
    // Macro expansion
    // -----------------------------------------------------------------------

    pub(crate) fn expand_macro(
        &mut self,
        macro_val: Value,
        args: &[Expr],
    ) -> Result<Rc<Expr>, Flow> {
        let Value::Macro(id) = macro_val else {
            return Err(signal("invalid-macro", vec![]));
        };

        // Check cache: same macro object + same source location (args slice
        // pointer from Rc<Vec<Expr>> body) → same expansion.
        // Fingerprint validation detects ABA from reused addresses.
        let cache_key = (id, args.as_ptr() as usize);
        let current_fp = tail_fingerprint(args);
        if !self.macro_cache_disabled {
            if let Some((cached, stored_fp)) = self.macro_expansion_cache.get(&cache_key) {
                if *stored_fp == current_fp {
                    self.macro_cache_hits += 1;
                    return Ok(cached.clone());
                }
                // Fingerprint mismatch → ABA, fall through to re-expand
            }
        }

        let expand_start = std::time::Instant::now();
        // Clone the macro data before calling self.apply_lambda
        let lambda_data = self.heap.get_macro_data(id).clone();

        // Root arg values during macro expansion to survive GC.
        // Use cached_quote_to_value so that the same Expr pointer (from a
        // shared Rc<Vec<Expr>> lambda body) produces the same Value, preserving
        // `eq` identity required by pcase's memoization cache.
        let saved = self.save_temp_roots();
        let arg_values: Vec<Value> = args.iter().map(|e| self.cached_quote_to_value(e)).collect();
        for v in &arg_values {
            self.push_temp_root(*v);
        }

        // Apply the macro body
        let expanded_value = self.apply_lambda(&lambda_data, arg_values, Value::Macro(id))?;
        // Root expansion result during value_to_expr traversal
        self.push_temp_root(expanded_value);

        // Convert value back to expr for re-evaluation
        let result = Rc::new(value_to_expr(&expanded_value));
        self.restore_temp_roots(saved);

        // Cache the expansion as Rc<Expr>.  The Rc keeps the expansion
        // data alive, so inner Vec addresses remain stable for future
        // cache key lookups by inner macro calls.
        let expand_elapsed = expand_start.elapsed();
        self.macro_cache_misses += 1;
        self.macro_expand_total_us += expand_elapsed.as_micros() as u64;
        if !self.macro_cache_disabled {
            if expand_elapsed.as_millis() > 50 {
                tracing::warn!(
                    "macro_cache MISS id={id:?} ptr={:#x} took {expand_elapsed:.2?}",
                    args.as_ptr() as usize
                );
            }
            self.macro_expansion_cache
                .insert(cache_key, (result.clone(), current_fp));
        }

        Ok(result)
    }

    // -----------------------------------------------------------------------
    // Variable assignment
    // -----------------------------------------------------------------------

    /// Assign a value to a variable identified by SymId.
    /// Uses the SymId directly for lexenv/dynamic lookup, preserving
    /// uninterned symbol identity (like Emacs's EQ-based setq).
    pub(crate) fn assign_by_id(&mut self, sym_id: SymId, value: Value) {
        let name = resolve_sym(sym_id);
        // If lexical binding and not special, check lexenv first
        if self.lexical_binding() && !self.obarray.is_special(name) {
            if let Some(cell_id) = lexenv_assq(self.lexenv, sym_id) {
                lexenv_set(cell_id, value);
                return;
            }
        }

        // Search dynamic frames (inner to outer)
        for frame in self.dynamic.iter_mut().rev() {
            if frame.contains_key(&sym_id) {
                frame.insert(sym_id, value);
                return;
            }
        }

        // Update existing buffer-local binding if present.
        if let Some(buf) = self.buffers.current_buffer_mut() {
            if buf.get_buffer_local(name).is_some() {
                buf.set_buffer_local(name, value);
                return;
            }
        }

        // Auto-local variables become local upon assignment.
        if self.custom.is_auto_buffer_local(name) {
            if let Some(buf) = self.buffers.current_buffer_mut() {
                buf.set_buffer_local(name, value);
                return;
            }
        }

        // Fall through to obarray value cell
        self.obarray.set_symbol_value(name, value);
    }

    pub(crate) fn assign(&mut self, name: &str, value: Value) {
        self.assign_by_id(intern(name), value);
    }

    pub(crate) fn visible_variable_value_or_nil(&self, name: &str) -> Value {
        if name == "nil" {
            return Value::Nil;
        }
        if name == "t" {
            return Value::True;
        }
        let name_id = intern(name);
        if let Some(value) = lexenv_lookup(self.lexenv, name_id) {
            return value;
        }
        for frame in self.dynamic.iter().rev() {
            if let Some(value) = frame.get(&name_id) {
                return *value;
            }
        }
        if let Some(buffer) = self.buffers.current_buffer() {
            if let Some(value) = buffer.get_buffer_local(name) {
                return *value;
            }
        }
        self.obarray
            .symbol_value(name)
            .cloned()
            .unwrap_or(Value::Nil)
    }

    fn run_unlet_watchers(&mut self, bindings: &[(String, Value, Value)]) -> Result<(), Flow> {
        for (name, _, restored_value) in bindings.iter().rev() {
            self.run_variable_watchers(name, restored_value, &Value::Nil, "unlet")?;
        }
        Ok(())
    }

    pub(crate) fn run_variable_watchers(
        &mut self,
        name: &str,
        new_value: &Value,
        old_value: &Value,
        operation: &str,
    ) -> Result<(), Flow> {
        self.run_variable_watchers_with_where(name, new_value, old_value, operation, &Value::Nil)
    }

    pub(crate) fn run_variable_watchers_with_where(
        &mut self,
        name: &str,
        new_value: &Value,
        old_value: &Value,
        operation: &str,
        where_value: &Value,
    ) -> Result<(), Flow> {
        if !self.watchers.has_watchers(name) {
            return Ok(());
        }
        let calls = self
            .watchers
            .notify_watchers(name, new_value, old_value, operation, where_value);
        for (callback, args) in calls {
            let _ = self.apply(callback, args)?;
        }
        Ok(())
    }

    pub(crate) fn assign_with_watchers(
        &mut self,
        name: &str,
        value: Value,
        operation: &str,
    ) -> EvalResult {
        self.assign(name, value);
        self.run_variable_watchers(name, &value, &Value::Nil, operation)?;
        Ok(value)
    }

    pub(crate) fn assign_with_watchers_by_id(
        &mut self,
        sym_id: SymId,
        value: Value,
        operation: &str,
    ) -> EvalResult {
        self.assign_by_id(sym_id, value);
        let name = resolve_sym(sym_id);
        self.run_variable_watchers(name, &value, &Value::Nil, operation)?;
        Ok(value)
    }

    /// Cached version of `quote_to_value` keyed on `Expr` pointer identity.
    ///
    /// When the same `&Expr` node is converted multiple times (e.g. pcase case
    /// patterns from a shared `Rc<Vec<Expr>>` lambda body), returns the same
    /// `Value` so that `eq` identity is preserved.  Only compound types
    /// (`List`, `DottedList`, `Vector`, `Str`) benefit from caching; scalars
    /// like `Int`, `Symbol`, `Char` already have identity-free representations.
    fn cached_quote_to_value(&mut self, expr: &Expr) -> Value {
        let key = expr as *const Expr;
        if let Some(&cached) = self.literal_cache.get(&key) {
            return cached;
        }
        // For compound types, recursively cache children too
        let value = match expr {
            Expr::List(items) => {
                let quoted: Vec<Value> = items.iter().map(|e| self.cached_quote_to_value(e)).collect();
                Value::list(quoted)
            }
            Expr::DottedList(items, last) => {
                let head_vals: Vec<Value> = items.iter().map(|e| self.cached_quote_to_value(e)).collect();
                let tail_val = self.cached_quote_to_value(last);
                head_vals
                    .into_iter()
                    .rev()
                    .fold(tail_val, |acc, item| Value::cons(item, acc))
            }
            Expr::Vector(items) => {
                let vals: Vec<Value> = items.iter().map(|e| self.cached_quote_to_value(e)).collect();
                Value::vector(vals)
            }
            _ => quote_to_value(expr),
        };
        self.literal_cache.insert(key, value);
        value
    }
}

fn rewrite_wrong_arity_function_object(flow: Flow, name: &str) -> Flow {
    match flow {
        Flow::Signal(mut sig) => {
            if sig.symbol_name() == "wrong-number-of-arguments"
                && sig.raw_data.is_none()
                && !sig.data.is_empty()
                && sig.data[0].as_symbol_name() == Some(name)
            {
                sig.data[0] = Value::Subr(intern(name));
            }
            Flow::Signal(sig)
        }
        other => other,
    }
}

fn rewrite_wrong_arity_alias_function_object(flow: Flow, alias: &str, target: &str) -> Flow {
    match flow {
        Flow::Signal(mut sig) => {
            let target_is_payload = sig.data.first().is_some_and(|value| match value {
                Value::Subr(id) => resolve_sym(*id) == target || resolve_sym(*id) == alias,
                _ => {
                    value.as_symbol_name() == Some(target) || value.as_symbol_name() == Some(alias)
                }
            });
            if sig.symbol_name() == "wrong-number-of-arguments"
                && !sig.data.is_empty()
                && target_is_payload
            {
                sig.data[0] = Value::symbol(alias);
            }
            Flow::Signal(sig)
        }
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert an Expr AST node to a Value (for quote).
pub fn quote_to_value(expr: &Expr) -> Value {
    match expr {
        Expr::Int(v) => Value::Int(*v),
        Expr::Float(v) => Value::Float(*v, next_float_id()),
        Expr::Str(s) => Value::string(s.clone()),
        Expr::Char(c) => Value::Char(*c),
        Expr::Keyword(id) => Value::Keyword(*id),
        Expr::Bool(true) => Value::True,
        Expr::Bool(false) => Value::Nil,
        Expr::Symbol(id) if resolve_sym(*id) == "nil" => Value::Nil,
        Expr::Symbol(id) if resolve_sym(*id) == "t" => Value::True,
        Expr::Symbol(id) => Value::Symbol(*id),
        Expr::List(items) => {
            let quoted = items.iter().map(quote_to_value).collect::<Vec<_>>();
            Value::list(quoted)
        }
        Expr::DottedList(items, last) => {
            let head_vals: Vec<Value> = items.iter().map(quote_to_value).collect();
            let tail_val = quote_to_value(last);
            head_vals
                .into_iter()
                .rev()
                .fold(tail_val, |acc, item| Value::cons(item, acc))
        }
        Expr::Vector(items) => {
            let vals = items.iter().map(quote_to_value).collect();
            Value::vector(vals)
        }
        Expr::OpaqueValue(v) => *v,
    }
}

/// Collect all `OpaqueValue` references from an Expr tree into a Vec.
/// Used to root them in temp_roots before evaluating the Expr.
pub(crate) fn collect_opaque_values(expr: &Expr, out: &mut Vec<Value>) {
    expr.collect_opaque_values(out);
}

/// Convert a Value back to an Expr (for macro expansion).
pub(crate) fn value_to_expr(value: &Value) -> Expr {
    match value {
        Value::Nil => Expr::Symbol(intern("nil")),
        Value::True => Expr::Symbol(intern("t")),
        Value::Int(n) => Expr::Int(*n),
        Value::Float(f, _) => Expr::Float(*f),
        Value::Symbol(id) => Expr::Symbol(*id),
        Value::Keyword(id) => Expr::Keyword(*id),
        Value::Str(id) => Expr::Str(with_heap(|h| h.get_string(*id).clone())),
        Value::Char(c) => Expr::Char(*c),
        Value::Cons(_) => {
            if let Some(items) = list_to_vec(value) {
                Expr::List(items.iter().map(value_to_expr).collect())
            } else {
                // Improper list / dotted pair — traverse cons cells and
                // produce Expr::DottedList(proper_items, tail).
                let mut items = Vec::new();
                let mut cursor = *value;
                loop {
                    match cursor {
                        Value::Cons(id) => {
                            items.push(value_to_expr(&with_heap(|h| h.cons_car(id))));
                            cursor = with_heap(|h| h.cons_cdr(id));
                        }
                        _ => {
                            break Expr::DottedList(
                                items,
                                Box::new(value_to_expr(&cursor)),
                            );
                        }
                    }
                }
            }
        }
        Value::Vector(v) => {
            let items = with_heap(|h| h.get_vector(*v).clone());
            Expr::Vector(items.iter().map(value_to_expr).collect())
        }
        Value::Subr(id) => Expr::OpaqueValue(Value::Subr(*id)),
        // Lambda, Macro, ByteCode, HashTable, Buffer, etc. — preserve as
        // opaque values so they survive the Value→Expr→Value round-trip
        // (e.g., closures embedded in defcustom backquote expansions).
        other => Expr::OpaqueValue(*other),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[path = "eval_test.rs"]
mod tests;
